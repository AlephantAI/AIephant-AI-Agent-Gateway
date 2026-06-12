use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use chrono::Utc;
use http::{HeaderMap, HeaderName, HeaderValue, header};

use crate::{
    agent::tools::{
        egress_policy::validate_target_url,
        executor::{ToolExecutionContext, ToolExecutionErrorKind},
        mcp_streamable_http::{
            json_rpc::{
                CLIENT_PROTOCOL_VERSION, JSON_RPC_VERSION, JsonRpcProtocolError,
                McpInitializeResult, McpJsonRpcError, McpJsonRpcNotification, McpJsonRpcRequest,
                McpJsonRpcResponse, mcp_error_retryable, validate_json_rpc_envelope,
                validate_supported_protocol_version, validate_tools_capability,
            },
            session::{
                self, InMemorySessionSingleflight, McpSessionCache, McpStreamableSession,
                NoopMcpSessionCache, RedisMcpSessionCache, validate_session_id,
            },
            sse::SseLimits,
            transport::{SseReadOptions, TransportError, read_json_rpc_response},
        },
        types::{
            ToolBillingOverride, ToolCallRequest, ToolCallResponse, ToolCost,
            ToolExecutionErrorEnvelope, ToolExecutionEvents, ToolExecutionStatus,
            ToolGatewayMetadata, ToolPolicySummary,
        },
    },
    config::agent::{AgentToolEgressPolicyConfig, AgentToolTargetConfig},
};

const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const STREAMABLE_TARGET_KIND: &str = "mcp-streamable-http";
const CACHE_LOCK_LOSER_WAIT_MS: u64 = 25;

static LOCAL_SESSION_SINGLEFLIGHT: OnceLock<InMemorySessionSingleflight<InitializedSession>> =
    OnceLock::new();

pub async fn execute_lifecycle(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    if let Some(redis) = &ctx.redis {
        let cache = RedisMcpSessionCache::new(redis.clone());
        execute_lifecycle_with_cache(
            ctx,
            target,
            request,
            egress_policy,
            default_timeout_ms,
            max_response_bytes,
            &cache,
        )
        .await
    } else {
        let cache = NoopMcpSessionCache;
        execute_lifecycle_with_cache(
            ctx,
            target,
            request,
            egress_policy,
            default_timeout_ms,
            max_response_bytes,
            &cache,
        )
        .await
    }
}

pub async fn execute_lifecycle_with_cache(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
    cache: &dyn McpSessionCache,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    if !target.method.eq_ignore_ascii_case("POST") {
        return Err(ToolExecutionErrorKind::ToolTargetUnavailable);
    }
    let url = target
        .url
        .as_deref()
        .ok_or(ToolExecutionErrorKind::ToolTargetUnavailable)?;
    validate_target_url(url, egress_policy)
        .map_err(|_| ToolExecutionErrorKind::ToolTargetUnavailable)?;
    let parsed = url::Url::parse(url).map_err(|_| ToolExecutionErrorKind::ToolTargetUnavailable)?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err(ToolExecutionErrorKind::ToolTargetUnavailable),
    }

    let timeout_ms =
        effective_timeout_ms(target.timeout_ms, request.timeout_ms, default_timeout_ms);
    let mut client_builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms));
    if !egress_policy.allow_environment_proxy {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .map_err(|_| ToolExecutionErrorKind::ToolTargetUnavailable)?;

    let execution_id = request
        .tool_execution_id
        .clone()
        .unwrap_or_else(|| format!("exec_{}", uuid::Uuid::new_v4().simple()));
    let started_at = Instant::now();
    let sse_options = sse_read_options(ctx, max_response_bytes);
    let metadata_state = LifecycleMetadataState::shared();
    let session_lock_ttl_secs = session::session_lock_ttl_secs_for_timeout(ctx, timeout_ms);

    Ok(
        match tokio::time::timeout(
            lifecycle_timeout(timeout_ms),
            execute_lifecycle_inner(
                ctx,
                target,
                request,
                parsed,
                client,
                execution_id.clone(),
                max_response_bytes,
                &sse_options,
                metadata_state.clone(),
                started_at,
                cache,
                session_lock_ttl_secs,
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                failed_lifecycle_response(ctx, target, request, &execution_id, &error, started_at)
            }
            Err(_) => lifecycle_timeout_response(
                ctx,
                target,
                request,
                &execution_id,
                &metadata_state,
                started_at,
            ),
        },
    )
}

fn lifecycle_timeout(default_timeout_ms: u64) -> Duration {
    Duration::from_millis(default_timeout_ms.max(1))
}

#[derive(Debug, Clone, Default)]
struct LifecycleMetadataState {
    inner: Arc<Mutex<LifecycleMetadataSnapshot>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LifecycleMetadataSnapshot {
    protocol_version: Option<String>,
    sse_used: bool,
}

impl LifecycleMetadataState {
    fn shared() -> Self {
        Self::default()
    }

    fn mark_sse_used(&self) {
        self.with_snapshot_mut(|snapshot| {
            snapshot.sse_used = true;
        });
    }

    fn set_protocol_version(&self, protocol_version: String) {
        self.with_snapshot_mut(|snapshot| {
            snapshot.protocol_version = Some(protocol_version);
        });
    }

    fn snapshot(&self) -> LifecycleMetadataSnapshot {
        self.inner
            .lock()
            .expect("lifecycle metadata state lock")
            .clone()
    }

    fn with_snapshot_mut(&self, update: impl FnOnce(&mut LifecycleMetadataSnapshot)) {
        let mut guard = self.inner.lock().expect("lifecycle metadata state lock");
        update(&mut guard);
    }
}

fn response_is_sse(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| {
            content_type
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        })
}

fn failed_lifecycle_response(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    execution_id: &str,
    error: &LifecycleFailure,
    started_at: Instant,
) -> ToolCallResponse {
    failed_response(
        ctx,
        target,
        request,
        execution_id,
        error.code(),
        error.message(),
        error.retryable(),
        error.protocol_version().map(str::to_string),
        error.sse_used(),
        started_at,
    )
}

fn lifecycle_timeout_response(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    execution_id: &str,
    metadata_state: &LifecycleMetadataState,
    started_at: Instant,
) -> ToolCallResponse {
    let metadata = metadata_state.snapshot();
    failed_response(
        ctx,
        target,
        request,
        execution_id,
        "mcp_lifecycle_timeout",
        "mcp streamable http lifecycle timed out",
        true,
        metadata.protocol_version,
        metadata.sse_used,
        started_at,
    )
}

#[allow(clippy::too_many_arguments)]
async fn execute_lifecycle_inner(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: reqwest::Client,
    execution_id: String,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    cache: &dyn McpSessionCache,
    session_lock_ttl_secs: u64,
) -> Result<ToolCallResponse, LifecycleFailure> {
    let key = session::session_key(ctx);
    if let Some(cached) = load_reusable_cached_session(ctx, cache, &key).await {
        return execute_with_cached_session(
            ctx,
            target,
            request,
            url,
            client,
            execution_id,
            max_response_bytes,
            sse_options,
            metadata_state,
            started_at,
            cache,
            key,
            cached,
            false,
            session_lock_ttl_secs,
        )
        .await;
    }

    execute_fresh_lifecycle_guarded(
        ctx,
        target,
        request,
        url,
        client,
        execution_id,
        max_response_bytes,
        sse_options,
        metadata_state,
        started_at,
        cache,
        key,
        false,
        session_lock_ttl_secs,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_with_cached_session(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: reqwest::Client,
    execution_id: String,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    cache: &dyn McpSessionCache,
    key: String,
    cached: McpStreamableSession,
    reinitialized_on_cached_response: bool,
    session_lock_ttl_secs: u64,
) -> Result<ToolCallResponse, LifecycleFailure> {
    metadata_state.set_protocol_version(cached.negotiated_protocol_version.clone());
    match call_tool_with_session(
        ctx,
        target,
        request,
        url.clone(),
        &client,
        &execution_id,
        max_response_bytes,
        sse_options,
        metadata_state.clone(),
        started_at,
        Some(&cached),
        &cached.negotiated_protocol_version,
        false,
    )
    .await?
    {
        CallToolOutcome::Response(mut response) => {
            mark_gateway_metadata(&mut response, true, reinitialized_on_cached_response);
            if response.status == ToolExecutionStatus::Completed {
                let mut refreshed = cached;
                let now = Utc::now();
                refreshed.last_used_at = now;
                refreshed.expires_at =
                    now + chrono::Duration::seconds(ctx.mcp_session_cache_ttl_secs as i64);
                cache
                    .store(&key, &refreshed, ctx.mcp_session_cache_ttl_secs)
                    .await;
            }
            Ok(response)
        }
        CallToolOutcome::SessionExpired(mut response) => {
            mark_gateway_metadata(&mut response, true, reinitialized_on_cached_response);
            cache.delete(&key).await;
            execute_fresh_lifecycle_guarded(
                ctx,
                target,
                request,
                url,
                client,
                execution_id,
                max_response_bytes,
                sse_options,
                metadata_state,
                started_at,
                cache,
                key,
                true,
                session_lock_ttl_secs,
            )
            .await
        }
        CallToolOutcome::UnauthorizedOrForbidden(mut response) => {
            mark_gateway_metadata(&mut response, true, reinitialized_on_cached_response);
            cache.delete(&key).await;
            Ok(response)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_fresh_lifecycle_guarded(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: reqwest::Client,
    execution_id: String,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    cache: &dyn McpSessionCache,
    key: String,
    reinitialized: bool,
    session_lock_ttl_secs: u64,
) -> Result<ToolCallResponse, LifecycleFailure> {
    if ctx.redis.is_none() {
        return execute_fresh_lifecycle_with_local_singleflight(
            ctx,
            target,
            request,
            url,
            client,
            execution_id,
            max_response_bytes,
            sse_options,
            metadata_state,
            started_at,
            cache,
            key,
            reinitialized,
        )
        .await;
    }

    let mut reinitialized = reinitialized;
    loop {
        match session::try_acquire_session_lock_with_ttl(ctx, session_lock_ttl_secs).await {
            Ok(Some(token)) => {
                let cached = load_reusable_cached_session(ctx, cache, &key).await;
                let response = match redis_initialize_guard_step(
                    RedisInitializeGuardRole::Owner,
                    cached.is_some(),
                ) {
                    RedisInitializeGuardStep::UseCached => {
                        let cached = cached.expect("cached session exists");
                        let cached_result = call_cached_session_for_guard(
                            ctx,
                            target,
                            request,
                            url.clone(),
                            &client,
                            &execution_id,
                            max_response_bytes,
                            sse_options,
                            metadata_state.clone(),
                            started_at,
                            cache,
                            &key,
                            &cached,
                            reinitialized,
                        )
                        .await;
                        match cached_result {
                            Ok(GuardedCachedCallResult::Return(response)) => Ok(response),
                            Ok(GuardedCachedCallResult::Reinitialize) => {
                                execute_fresh_lifecycle_once(
                                    ctx,
                                    target,
                                    request,
                                    url.clone(),
                                    client.clone(),
                                    execution_id.clone(),
                                    max_response_bytes,
                                    sse_options,
                                    metadata_state.clone(),
                                    started_at,
                                    cache,
                                    key.clone(),
                                    true,
                                )
                                .await
                            }
                            Err(error) => Err(error),
                        }
                    }
                    RedisInitializeGuardStep::InitializeUnderHeldGuard => {
                        execute_fresh_lifecycle_once(
                            ctx,
                            target,
                            request,
                            url.clone(),
                            client.clone(),
                            execution_id.clone(),
                            max_response_bytes,
                            sse_options,
                            metadata_state.clone(),
                            started_at,
                            cache,
                            key.clone(),
                            reinitialized,
                        )
                        .await
                    }
                    RedisInitializeGuardStep::RetryGuardPath => {
                        unreachable!("lock owner never retries guard path")
                    }
                };
                let _ = session::release_session_lock(ctx, &token).await;
                return response;
            }
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(CACHE_LOCK_LOSER_WAIT_MS)).await;
                let cached = load_reusable_cached_session(ctx, cache, &key).await;
                match redis_initialize_guard_step(
                    RedisInitializeGuardRole::LoserAfterWait,
                    cached.is_some(),
                ) {
                    RedisInitializeGuardStep::UseCached => {
                        let cached = cached.expect("cached session exists");
                        match call_cached_session_for_guard(
                            ctx,
                            target,
                            request,
                            url.clone(),
                            &client,
                            &execution_id,
                            max_response_bytes,
                            sse_options,
                            metadata_state.clone(),
                            started_at,
                            cache,
                            &key,
                            &cached,
                            reinitialized,
                        )
                        .await?
                        {
                            GuardedCachedCallResult::Return(response) => {
                                return Ok(response);
                            }
                            GuardedCachedCallResult::Reinitialize => {
                                reinitialized = true;
                                continue;
                            }
                        }
                    }
                    RedisInitializeGuardStep::RetryGuardPath => continue,
                    RedisInitializeGuardStep::InitializeUnderHeldGuard => {
                        unreachable!("lock loser must not initialize directly")
                    }
                }
            }
            Err(_) => {
                return execute_fresh_lifecycle_with_local_singleflight(
                    ctx,
                    target,
                    request,
                    url,
                    client,
                    execution_id,
                    max_response_bytes,
                    sse_options,
                    metadata_state,
                    started_at,
                    cache,
                    key,
                    reinitialized,
                )
                .await;
            }
        }
    }
}

async fn load_reusable_cached_session(
    ctx: &ToolExecutionContext,
    cache: &dyn McpSessionCache,
    key: &str,
) -> Option<McpStreamableSession> {
    let cached = cache.load(key).await?;
    if session::should_evict_session(ctx, &cached) {
        cache.delete(key).await;
        return None;
    }

    Some(cached)
}

#[allow(clippy::too_many_arguments)]
async fn call_cached_session_for_guard(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: &reqwest::Client,
    execution_id: &str,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    cache: &dyn McpSessionCache,
    key: &str,
    cached: &McpStreamableSession,
    reinitialized: bool,
) -> Result<GuardedCachedCallResult, LifecycleFailure> {
    metadata_state.set_protocol_version(cached.negotiated_protocol_version.clone());
    let outcome = call_tool_with_session(
        ctx,
        target,
        request,
        url,
        client,
        execution_id,
        max_response_bytes,
        sse_options,
        metadata_state,
        started_at,
        Some(cached),
        &cached.negotiated_protocol_version,
        false,
    )
    .await?;
    match outcome {
        CallToolOutcome::Response(mut response) => {
            mark_gateway_metadata(&mut response, true, reinitialized);
            if response.status == ToolExecutionStatus::Completed {
                let mut refreshed = cached.clone();
                let now = Utc::now();
                refreshed.last_used_at = now;
                refreshed.expires_at =
                    now + chrono::Duration::seconds(ctx.mcp_session_cache_ttl_secs as i64);
                cache
                    .store(key, &refreshed, ctx.mcp_session_cache_ttl_secs)
                    .await;
            }
            Ok(GuardedCachedCallResult::Return(response))
        }
        CallToolOutcome::SessionExpired(mut response) => {
            mark_gateway_metadata(&mut response, true, reinitialized);
            cache.delete(key).await;
            Ok(GuardedCachedCallResult::Reinitialize)
        }
        CallToolOutcome::UnauthorizedOrForbidden(mut response) => {
            mark_gateway_metadata(&mut response, true, reinitialized);
            cache.delete(key).await;
            Ok(GuardedCachedCallResult::Return(response))
        }
    }
}

enum GuardedCachedCallResult {
    Return(ToolCallResponse),
    Reinitialize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisInitializeGuardRole {
    Owner,
    LoserAfterWait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisInitializeGuardStep {
    UseCached,
    InitializeUnderHeldGuard,
    RetryGuardPath,
}

fn redis_initialize_guard_step(
    role: RedisInitializeGuardRole,
    cached_available: bool,
) -> RedisInitializeGuardStep {
    match (role, cached_available) {
        (_, true) => RedisInitializeGuardStep::UseCached,
        (RedisInitializeGuardRole::Owner, false) => {
            RedisInitializeGuardStep::InitializeUnderHeldGuard
        }
        (RedisInitializeGuardRole::LoserAfterWait, false) => {
            RedisInitializeGuardStep::RetryGuardPath
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_fresh_lifecycle_with_local_singleflight(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: reqwest::Client,
    execution_id: String,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    cache: &dyn McpSessionCache,
    key: String,
    reinitialized: bool,
) -> Result<ToolCallResponse, LifecycleFailure> {
    let initialized = local_session_singleflight()
        .get_or_try_init(key.clone(), || async {
            let initialized = initialize_session(
                ctx,
                url.clone(),
                &client,
                &execution_id,
                max_response_bytes,
                sse_options,
                metadata_state.clone(),
            )
            .await?;
            if let Some(session) = &initialized.session {
                cache
                    .store(key.as_str(), session, ctx.mcp_session_cache_ttl_secs)
                    .await;
            }
            Ok::<_, LifecycleFailure>(Some(initialized))
        })
        .await?;

    let protocol_version = initialized
        .as_ref()
        .map(|initialized| initialized.protocol_version.clone())
        .unwrap_or_else(|| CLIENT_PROTOCOL_VERSION.to_string());
    let sse_used = initialized
        .as_ref()
        .is_some_and(|initialized| initialized.sse_used);
    let outcome = call_tool_with_session(
        ctx,
        target,
        request,
        url,
        &client,
        &execution_id,
        max_response_bytes,
        sse_options,
        metadata_state,
        started_at,
        initialized
            .as_ref()
            .and_then(|initialized| initialized.session.as_ref()),
        &protocol_version,
        sse_used,
    )
    .await?;

    response_from_call_outcome(outcome, cache, &key, false, reinitialized).await
}

#[allow(clippy::too_many_arguments)]
async fn execute_fresh_lifecycle_once(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: reqwest::Client,
    execution_id: String,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    cache: &dyn McpSessionCache,
    key: String,
    reinitialized: bool,
) -> Result<ToolCallResponse, LifecycleFailure> {
    let initialized = initialize_session(
        ctx,
        url.clone(),
        &client,
        &execution_id,
        max_response_bytes,
        sse_options,
        metadata_state.clone(),
    )
    .await?;
    if let Some(session) = &initialized.session {
        cache
            .store(&key, session, ctx.mcp_session_cache_ttl_secs)
            .await;
    }
    let outcome = call_tool_with_session(
        ctx,
        target,
        request,
        url,
        &client,
        &execution_id,
        max_response_bytes,
        sse_options,
        metadata_state,
        started_at,
        initialized.session.as_ref(),
        &initialized.protocol_version,
        initialized.sse_used,
    )
    .await?;

    response_from_call_outcome(outcome, cache, &key, false, reinitialized).await
}

async fn response_from_call_outcome(
    outcome: CallToolOutcome,
    cache: &dyn McpSessionCache,
    key: &str,
    cache_hit: bool,
    reinitialized: bool,
) -> Result<ToolCallResponse, LifecycleFailure> {
    match outcome {
        CallToolOutcome::Response(mut response) => {
            mark_gateway_metadata(&mut response, cache_hit, reinitialized);
            Ok(response)
        }
        CallToolOutcome::SessionExpired(mut response)
        | CallToolOutcome::UnauthorizedOrForbidden(mut response) => {
            cache.delete(key).await;
            mark_gateway_metadata(&mut response, cache_hit, reinitialized);
            Ok(response)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn initialize_session(
    ctx: &ToolExecutionContext,
    url: url::Url,
    client: &reqwest::Client,
    execution_id: &str,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
) -> Result<InitializedSession, LifecycleFailure> {
    let initialize_id = format!("init_{execution_id}");
    let initialize_request = McpJsonRpcRequest {
        jsonrpc: JSON_RPC_VERSION,
        id: initialize_id.clone(),
        method: "initialize".to_string(),
        params: serde_json::json!({
            "protocolVersion": CLIENT_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "alephant-ai-gateway",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    };
    let initialize_http_response = post_json_rpc(
        client,
        url.clone(),
        &initialize_request,
        None,
        CLIENT_PROTOCOL_VERSION,
    )
    .await?;
    if !initialize_http_response.status().is_success() {
        return Err(LifecycleFailure::new(
            "mcp_initialize_http_error",
            "mcp initialize returned a non-success status",
            true,
        ));
    }
    let initialize_response_sse = response_is_sse(initialize_http_response.headers());
    if initialize_response_sse {
        metadata_state.mark_sse_used();
    }
    let initialize_transport = read_json_rpc_response::<McpJsonRpcResponse<McpInitializeResult>>(
        initialize_http_response,
        &initialize_id,
        max_response_bytes,
        sse_options,
    )
    .await
    .map_err(|error| {
        LifecycleFailure::from_transport(error).with_sse_used(initialize_response_sse)
    })?;
    let initialize_sse_used = initialize_transport.sse_used;
    validate_json_rpc_envelope(&initialize_transport.value, &initialize_id)
        .map_err(LifecycleFailure::from_json_rpc_protocol)?;
    if let Some(error) = initialize_transport.value.error {
        return Err(LifecycleFailure::from_mcp_error(error));
    }
    let initialize_result = initialize_transport.value.result.ok_or_else(|| {
        LifecycleFailure::new(
            "mcp_protocol_parse_error",
            "mcp initialize result is missing",
            false,
        )
        .with_sse_used(initialize_sse_used)
    })?;
    validate_supported_protocol_version(&initialize_result.protocol_version).map_err(|_| {
        LifecycleFailure::new(
            "mcp_unsupported_protocol_version",
            "mcp server negotiated an unsupported protocol version",
            false,
        )
        .with_protocol_version(initialize_result.protocol_version.clone())
        .with_sse_used(initialize_sse_used)
    })?;
    metadata_state.set_protocol_version(initialize_result.protocol_version.clone());
    validate_tools_capability(&initialize_result.capabilities).map_err(|_| {
        LifecycleFailure::new(
            "mcp_tools_capability_missing",
            "mcp server initialize result is missing tools capability",
            false,
        )
        .with_protocol_version(initialize_result.protocol_version.clone())
        .with_sse_used(initialize_sse_used)
    })?;

    let session_id = session_id_from_headers(&initialize_transport.headers).map_err(|error| {
        error
            .with_protocol_version(initialize_result.protocol_version.clone())
            .with_sse_used(initialize_sse_used)
    })?;

    send_initialized(
        client,
        url.clone(),
        session_id.as_deref(),
        &initialize_result.protocol_version,
    )
    .await
    .map_err(|error| {
        error
            .with_protocol_version(initialize_result.protocol_version.clone())
            .with_sse_used(initialize_sse_used)
    })?;

    let session = session_id.map(|session_id| {
        let now = Utc::now();
        McpStreamableSession {
            session_id,
            negotiated_protocol_version: initialize_result.protocol_version.clone(),
            target_hash: ctx.target_hash.clone(),
            auth_revision: ctx.auth_revision.clone(),
            server_info: initialize_result.server_info.clone(),
            capabilities: initialize_result.capabilities.clone(),
            created_at: now,
            last_used_at: now,
            expires_at: now + chrono::Duration::seconds(ctx.mcp_session_cache_ttl_secs as i64),
        }
    });

    Ok(InitializedSession {
        session,
        protocol_version: initialize_result.protocol_version,
        sse_used: initialize_sse_used,
    })
}

#[derive(Debug, Clone)]
struct InitializedSession {
    session: Option<McpStreamableSession>,
    protocol_version: String,
    sse_used: bool,
}

#[allow(clippy::too_many_arguments)]
async fn call_tool_with_session(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    url: url::Url,
    client: &reqwest::Client,
    execution_id: &str,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
    metadata_state: LifecycleMetadataState,
    started_at: Instant,
    session: Option<&McpStreamableSession>,
    protocol_version: &str,
    initialize_sse_used: bool,
) -> Result<CallToolOutcome, LifecycleFailure> {
    let call_request = McpJsonRpcRequest {
        jsonrpc: JSON_RPC_VERSION,
        id: execution_id.to_string(),
        method: "tools/call".to_string(),
        params: serde_json::json!({
            "name": target.tool_id,
            "arguments": request.arguments,
        }),
    };
    let call_http_response = post_json_rpc(
        client,
        url,
        &call_request,
        session.map(|session| session.session_id.as_str()),
        protocol_version,
    )
    .await?;
    if !call_http_response.status().is_success() {
        let status = call_http_response.status();
        let response = failed_response(
            ctx,
            target,
            request,
            execution_id,
            call_http_error_code(status),
            "mcp tool call returned a non-success status",
            status != http::StatusCode::UNAUTHORIZED && status != http::StatusCode::FORBIDDEN,
            Some(protocol_version.to_string()),
            initialize_sse_used,
            started_at,
        );
        return Ok(match status {
            http::StatusCode::NOT_FOUND => CallToolOutcome::SessionExpired(response),
            http::StatusCode::UNAUTHORIZED | http::StatusCode::FORBIDDEN => {
                CallToolOutcome::UnauthorizedOrForbidden(response)
            }
            _ => CallToolOutcome::Response(response),
        });
    }
    let call_response_sse = response_is_sse(call_http_response.headers());
    if call_response_sse {
        metadata_state.mark_sse_used();
    }
    let call_transport = read_json_rpc_response::<McpJsonRpcResponse<serde_json::Value>>(
        call_http_response,
        &execution_id,
        max_response_bytes,
        sse_options,
    )
    .await
    .map_err(|error| {
        LifecycleFailure::from_transport(error)
            .with_protocol_version(protocol_version.to_string())
            .with_sse_used(initialize_sse_used || call_response_sse)
    })?;
    let sse_used = initialize_sse_used || call_transport.sse_used;
    validate_json_rpc_envelope(&call_transport.value, &execution_id).map_err(|error| {
        LifecycleFailure::from_json_rpc_protocol(error)
            .with_protocol_version(protocol_version.to_string())
            .with_sse_used(sse_used)
    })?;
    if let Some(error) = call_transport.value.error {
        return Err(LifecycleFailure::from_mcp_error(error)
            .with_protocol_version(protocol_version.to_string())
            .with_sse_used(sse_used));
    }
    let output = call_transport.value.result.ok_or_else(|| {
        LifecycleFailure::new(
            "mcp_protocol_parse_error",
            "mcp tool call result is missing",
            false,
        )
        .with_protocol_version(protocol_version.to_string())
        .with_sse_used(sse_used)
    })?;

    Ok(CallToolOutcome::Response(completed_response(
        ctx,
        target,
        request,
        execution_id,
        output,
        protocol_version.to_string(),
        sse_used,
        started_at,
    )))
}

#[derive(Debug)]
enum CallToolOutcome {
    Response(ToolCallResponse),
    SessionExpired(ToolCallResponse),
    UnauthorizedOrForbidden(ToolCallResponse),
}

fn call_http_error_code(status: http::StatusCode) -> &'static str {
    match status {
        http::StatusCode::NOT_FOUND => "mcp_session_expired",
        http::StatusCode::UNAUTHORIZED => "mcp_call_unauthorized",
        http::StatusCode::FORBIDDEN => "mcp_call_forbidden",
        _ => "mcp_call_http_error",
    }
}

fn local_session_singleflight() -> &'static InMemorySessionSingleflight<InitializedSession> {
    LOCAL_SESSION_SINGLEFLIGHT.get_or_init(InMemorySessionSingleflight::default)
}

async fn send_initialized(
    client: &reqwest::Client,
    url: url::Url,
    session_id: Option<&str>,
    protocol_version: &str,
) -> Result<(), LifecycleFailure> {
    let notification = McpJsonRpcNotification {
        jsonrpc: JSON_RPC_VERSION,
        method: "notifications/initialized".to_string(),
    };
    let response = post_json_rpc(client, url, &notification, session_id, protocol_version).await?;
    if !response.status().is_success() {
        return Err(LifecycleFailure::new(
            "mcp_initialized_http_error",
            "mcp initialized notification returned a non-success status",
            true,
        )
        .with_protocol_version(protocol_version.to_string()));
    }

    Ok(())
}

async fn post_json_rpc<T>(
    client: &reqwest::Client,
    url: url::Url,
    body: &T,
    session_id: Option<&str>,
    protocol_version: &str,
) -> Result<reqwest::Response, LifecycleFailure>
where
    T: serde::Serialize + ?Sized,
{
    let mut builder = client
        .post(url)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .header(
            HeaderName::from_static(MCP_PROTOCOL_VERSION_HEADER),
            HeaderValue::from_str(protocol_version).map_err(|_| {
                LifecycleFailure::new(
                    "mcp_protocol_parse_error",
                    "mcp protocol version is invalid",
                    false,
                )
            })?,
        )
        .json(body);
    if let Some(session_id) = session_id {
        builder = builder.header(
            HeaderName::from_static(MCP_SESSION_ID_HEADER),
            HeaderValue::from_str(session_id).map_err(|_| {
                LifecycleFailure::new(
                    "mcp_protocol_parse_error",
                    "mcp session id is invalid",
                    false,
                )
            })?,
        );
    }

    builder.send().await.map_err(|error| {
        if error.is_timeout() {
            LifecycleFailure::new("mcp_transport_timeout", "mcp target timed out", true)
        } else {
            LifecycleFailure::new("mcp_target_unavailable", "mcp target is unavailable", true)
        }
    })
}

fn mark_gateway_metadata(response: &mut ToolCallResponse, cache_hit: bool, reinitialized: bool) {
    if let Some(metadata) = &mut response.gateway_metadata {
        metadata.cache_hit = cache_hit;
        metadata.reinitialized = reinitialized;
    }
}

fn completed_response(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    execution_id: &str,
    output: serde_json::Value,
    protocol_version: String,
    sse_used: bool,
    started_at: Instant,
) -> ToolCallResponse {
    let cost = ToolCost {
        amount_micros: target.rate_card.fixed_micros,
        currency: target.rate_card.currency.clone(),
        source: "rate_card".to_string(),
    };
    let billing_reason = completed_billing_reason(&output);
    ToolCallResponse {
        status: ToolExecutionStatus::Completed,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: execution_id.to_string(),
        output,
        error: None,
        gateway_metadata: Some(base_metadata(
            ctx,
            target,
            Some(protocol_version),
            sse_used,
            None,
            started_at,
        )),
        billing: ToolBillingOverride {
            reason: billing_reason.to_string(),
            billable: true,
            cost_micros: cost.amount_micros,
            currency: cost.currency.clone(),
            dedupe_key: execution_id.to_string(),
        },
        cost,
        policy: ToolPolicySummary {
            allowed: true,
            decision: "allowed".to_string(),
            reason: "tool_allowed".to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

fn completed_billing_reason(output: &serde_json::Value) -> &'static str {
    if output.get("isError").and_then(|value| value.as_bool()) == Some(true) {
        "tool_business_error"
    } else {
        "success"
    }
}

fn session_id_from_headers(headers: &HeaderMap) -> Result<Option<String>, LifecycleFailure> {
    let Some(value) = headers.get(HeaderName::from_static(MCP_SESSION_ID_HEADER)) else {
        return Ok(None);
    };
    let session_id = value.to_str().map_err(|_| invalid_session_id_failure())?;
    validate_session_id(session_id).map_err(|_| invalid_session_id_failure())?;

    Ok(Some(session_id.to_string()))
}

fn invalid_session_id_failure() -> LifecycleFailure {
    LifecycleFailure::new(
        "mcp_invalid_session_id",
        "mcp initialize response contains an invalid session id",
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn failed_response(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    execution_id: &str,
    code: &str,
    message: &str,
    retryable: bool,
    protocol_version: Option<String>,
    sse_used: bool,
    started_at: Instant,
) -> ToolCallResponse {
    ToolCallResponse {
        status: ToolExecutionStatus::Failed,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: execution_id.to_string(),
        output: serde_json::json!({
            "error": {
                "code": code,
                "retryable": retryable,
                "message": message,
            }
        }),
        error: Some(ToolExecutionErrorEnvelope {
            code: code.to_string(),
            message: message.to_string(),
            retryable,
        }),
        gateway_metadata: Some(base_metadata(
            ctx,
            target,
            protocol_version,
            sse_used,
            Some(code.to_string()),
            started_at,
        )),
        billing: ToolBillingOverride {
            reason: failed_billing_reason(code).to_string(),
            billable: false,
            cost_micros: 0,
            currency: target.rate_card.currency.clone(),
            dedupe_key: execution_id.to_string(),
        },
        cost: ToolCost {
            amount_micros: 0,
            currency: target.rate_card.currency.clone(),
            source: "waived".to_string(),
        },
        policy: ToolPolicySummary {
            allowed: true,
            decision: "allowed".to_string(),
            reason: "tool_allowed".to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

fn failed_billing_reason(code: &str) -> &str {
    match code {
        "mcp_call_failed" | "mcp_json_rpc_error" => "json_rpc_error",
        _ => code,
    }
}

fn base_metadata(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    protocol_version: Option<String>,
    sse_used: bool,
    failure_class: Option<String>,
    started_at: Instant,
) -> ToolGatewayMetadata {
    ToolGatewayMetadata {
        execution_source: "gateway_executed".to_string(),
        target_kind: STREAMABLE_TARGET_KIND.to_string(),
        target_id: target.tool_id.clone(),
        target_hash: ctx.target_hash.clone(),
        auth_revision: ctx.auth_revision.clone(),
        cache_hit: false,
        reinitialized: false,
        protocol_version,
        sse_used,
        failure_class,
        blocked_before_dispatch: false,
        latency_ms: Some(started_at.elapsed().as_millis() as u64),
        ..ToolGatewayMetadata::default()
    }
}

fn sse_read_options(ctx: &ToolExecutionContext, max_response_bytes: usize) -> SseReadOptions {
    SseReadOptions {
        limits: SseLimits {
            max_total_bytes: max_response_bytes,
            max_event_bytes: ctx.mcp_sse_max_event_bytes,
            max_line_bytes: ctx.mcp_sse_max_line_bytes,
            max_events: ctx.mcp_sse_max_events,
            max_batch_items: ctx.mcp_sse_max_batch_items,
        },
        idle_timeout: Duration::from_millis(ctx.mcp_sse_idle_timeout_ms.max(1)),
    }
}

fn effective_timeout_ms(
    target_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
    default_timeout_ms: u64,
) -> u64 {
    let configured_timeout_ms = target_timeout_ms.unwrap_or(default_timeout_ms).max(1);
    request_timeout_ms
        .map(|request_timeout_ms| request_timeout_ms.max(1).min(configured_timeout_ms))
        .unwrap_or(configured_timeout_ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LifecycleFailure {
    code: String,
    message: String,
    retryable: bool,
    protocol_version: Option<String>,
    sse_used: bool,
}

impl LifecycleFailure {
    fn new(code: &str, message: &str, retryable: bool) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
            retryable,
            protocol_version: None,
            sse_used: false,
        }
    }

    fn from_transport(error: TransportError) -> Self {
        match error {
            TransportError::ResponseTooLarge => Self::new(
                "mcp_response_too_large",
                "mcp response exceeds size limit",
                false,
            ),
            TransportError::TargetUnavailable => {
                Self::new("mcp_target_unavailable", "mcp target is unavailable", true)
            }
            TransportError::ProtocolParse => Self::new(
                "mcp_protocol_parse_error",
                "mcp response is not valid JSON-RPC",
                false,
            ),
            TransportError::SseParse => Self::new(
                "mcp_sse_parse_error",
                "mcp SSE response is not valid JSON-RPC",
                false,
            ),
            TransportError::SseServerRequestUnsupported => Self::new(
                "mcp_server_request_unsupported",
                "mcp SSE server requests are not supported",
                false,
            ),
            TransportError::SseIncomplete => Self::new(
                "mcp_sse_incomplete",
                "mcp SSE response did not include a matching JSON-RPC response",
                false,
            ),
            TransportError::SseIdleTimeout => Self::new(
                "mcp_sse_idle_timeout",
                "mcp SSE response timed out while waiting for an event",
                true,
            ),
        }
    }

    fn from_json_rpc_protocol(error: JsonRpcProtocolError) -> Self {
        match error {
            JsonRpcProtocolError::InvalidEnvelope => Self::new(
                "mcp_protocol_parse_error",
                "mcp response is not a valid JSON-RPC envelope",
                false,
            ),
            JsonRpcProtocolError::UnsupportedProtocolVersion => Self::new(
                "mcp_unsupported_protocol_version",
                "mcp server negotiated an unsupported protocol version",
                false,
            ),
            JsonRpcProtocolError::ToolsCapabilityMissing => Self::new(
                "mcp_tools_capability_missing",
                "mcp server initialize result is missing tools capability",
                false,
            ),
        }
    }

    fn from_mcp_error(error: McpJsonRpcError) -> Self {
        Self::new(
            "mcp_call_failed",
            &error.message,
            mcp_error_retryable(error.code),
        )
    }

    fn with_protocol_version(mut self, protocol_version: String) -> Self {
        self.protocol_version = Some(protocol_version);
        self
    }

    fn with_sse_used(mut self, sse_used: bool) -> Self {
        self.sse_used = sse_used;
        self
    }

    fn code(&self) -> &str {
        &self.code
    }

    fn message(&self) -> &str {
        &self.message
    }

    fn retryable(&self) -> bool {
        self.retryable
    }

    fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    fn sse_used(&self) -> bool {
        self.sse_used
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::agent::tools::{
        mcp_streamable_http::test_support::{
            test_context, test_request, test_session_for_ctx, test_streamable_target,
        },
        types::ToolExecutionStatus,
    };

    #[test]
    fn whole_flow_timeout_maps_to_failed_response_metadata() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();
        let metadata_state = LifecycleMetadataState::shared();

        let response = lifecycle_timeout_response(
            &ctx,
            &target,
            &request,
            "exec-1",
            &metadata_state,
            Instant::now(),
        );

        assert_eq!(lifecycle_timeout(0), Duration::from_millis(1));
        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_lifecycle_timeout")
        );
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_lifecycle_timeout")
        );
        assert_eq!(metadata.protocol_version, None);
        assert!(!metadata.sse_used);
        assert!(!metadata.blocked_before_dispatch);
    }

    #[test]
    fn lifecycle_timeout_uses_resolved_request_target_budget() {
        let resolved = effective_timeout_ms(Some(250), Some(25), 1000);

        assert_eq!(resolved, 25);
        assert_eq!(lifecycle_timeout(resolved), Duration::from_millis(25));
    }

    #[test]
    fn json_rpc_failure_uses_explainable_billing_reason() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();

        let response = failed_response(
            &ctx,
            &target,
            &request,
            "exec-1",
            "mcp_call_failed",
            "upstream failed",
            false,
            Some(CLIENT_PROTOCOL_VERSION.to_string()),
            false,
            Instant::now(),
        );

        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_call_failed")
        );
        assert_eq!(response.billing.reason, "json_rpc_error");
        assert!(!response.billing.billable);
        assert_eq!(response.billing.cost_micros, 0);
    }

    #[test]
    fn redis_guard_step_never_initializes_from_loser_cache_miss() {
        assert_eq!(
            redis_initialize_guard_step(RedisInitializeGuardRole::Owner, true),
            RedisInitializeGuardStep::UseCached
        );
        assert_eq!(
            redis_initialize_guard_step(RedisInitializeGuardRole::Owner, false),
            RedisInitializeGuardStep::InitializeUnderHeldGuard
        );
        assert_eq!(
            redis_initialize_guard_step(RedisInitializeGuardRole::LoserAfterWait, true,),
            RedisInitializeGuardStep::UseCached
        );
        assert_eq!(
            redis_initialize_guard_step(RedisInitializeGuardRole::LoserAfterWait, false,),
            RedisInitializeGuardStep::RetryGuardPath
        );
    }

    #[tokio::test]
    async fn post_guard_cache_read_reuses_valid_and_evicts_stale_session() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let key = session::session_key(&ctx);
        let cache = session::InMemoryMcpSessionCache::default();
        let valid = test_session_for_ctx(&ctx, "session-valid");

        cache.store(&key, &valid, 60).await;
        let loaded = load_reusable_cached_session(&ctx, &cache, &key)
            .await
            .expect("valid cached session is reused");
        assert_eq!(loaded.session_id, "session-valid");

        let stale = McpStreamableSession {
            target_hash: "sha256:old-target".to_string(),
            ..test_session_for_ctx(&ctx, "session-stale")
        };
        cache.store(&key, &stale, 60).await;

        assert!(
            load_reusable_cached_session(&ctx, &cache, &key)
                .await
                .is_none()
        );
        assert!(cache.load(&key).await.is_none());
    }

    #[test]
    fn post_initialize_timeout_preserves_metadata_state() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();
        let metadata_state = LifecycleMetadataState::shared();
        metadata_state.set_protocol_version(CLIENT_PROTOCOL_VERSION.to_string());
        metadata_state.mark_sse_used();

        let response = lifecycle_timeout_response(
            &ctx,
            &target,
            &request,
            "exec-1",
            &metadata_state,
            Instant::now(),
        );

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_lifecycle_timeout")
        );
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.protocol_version.as_deref(),
            Some(CLIENT_PROTOCOL_VERSION)
        );
        assert!(metadata.sse_used);
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_lifecycle_timeout")
        );
    }

    #[test]
    fn failure_after_initialize_preserves_protocol_and_sse_metadata() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();
        let failure = LifecycleFailure::from_transport(TransportError::ProtocolParse)
            .with_protocol_version(CLIENT_PROTOCOL_VERSION.to_string())
            .with_sse_used(true);

        let response =
            failed_lifecycle_response(&ctx, &target, &request, "exec-1", &failure, Instant::now());

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_protocol_parse_error")
        );
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.protocol_version.as_deref(),
            Some(CLIENT_PROTOCOL_VERSION)
        );
        assert!(metadata.sse_used);
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_protocol_parse_error")
        );
    }

    #[test]
    fn completed_response_marks_is_error_output_as_business_error() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();

        let response = completed_response(
            &ctx,
            &target,
            &request,
            "exec-1",
            serde_json::json!({ "isError": true, "content": [] }),
            CLIENT_PROTOCOL_VERSION.to_string(),
            false,
            Instant::now(),
        );

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.billing.reason, "tool_business_error");
        assert!(response.billing.billable);
    }

    #[test]
    fn completed_response_marks_non_error_output_as_success() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();

        let response = completed_response(
            &ctx,
            &target,
            &request,
            "exec-1",
            serde_json::json!({ "isError": false, "content": [] }),
            CLIENT_PROTOCOL_VERSION.to_string(),
            false,
            Instant::now(),
        );

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.billing.reason, "success");
    }

    #[test]
    fn present_invalid_session_id_maps_to_failed_response() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let ctx = test_context(&target);
        let request = test_request();
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(MCP_SESSION_ID_HEADER),
            HeaderValue::from_static("has space"),
        );
        let failure = session_id_from_headers(&headers)
            .expect_err("invalid present session id fails")
            .with_protocol_version(CLIENT_PROTOCOL_VERSION.to_string())
            .with_sse_used(true);

        let response =
            failed_lifecycle_response(&ctx, &target, &request, "exec-1", &failure, Instant::now());

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_invalid_session_id")
        );
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.protocol_version.as_deref(),
            Some(CLIENT_PROTOCOL_VERSION)
        );
        assert!(metadata.sse_used);
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_invalid_session_id")
        );
    }

    #[test]
    fn missing_session_id_stays_stateless() {
        let headers = HeaderMap::new();

        assert_eq!(session_id_from_headers(&headers).expect("valid"), None);
    }

    #[test]
    fn sse_reader_options_use_context_idle_timeout() {
        let target = test_streamable_target("http://127.0.0.1:1/mcp");
        let mut ctx = test_context(&target);
        ctx.mcp_sse_idle_timeout_ms = 37;
        ctx.mcp_sse_max_event_bytes = 101;
        ctx.mcp_sse_max_line_bytes = 102;
        ctx.mcp_sse_max_events = 103;
        ctx.mcp_sse_max_batch_items = 104;

        let options = sse_read_options(&ctx, 1000);

        assert_eq!(options.idle_timeout, Duration::from_millis(37));
        assert_eq!(options.limits.max_total_bytes, 1000);
        assert_eq!(options.limits.max_event_bytes, 101);
        assert_eq!(options.limits.max_line_bytes, 102);
        assert_eq!(options.limits.max_events, 103);
        assert_eq!(options.limits.max_batch_items, 104);
    }
}
