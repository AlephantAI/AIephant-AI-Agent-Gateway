use std::{sync::Arc, time::Duration};

use futures::TryStreamExt;

use crate::{
    agent::{
        context::AgentContext,
        tools::{
            egress_policy::validate_target_url,
            mcp_http::{McpHttpError, execute_mcp_http_tool},
            mcp_sse, mcp_streamable_http, openapi,
            types::{
                ToolBillingOverride, ToolCallRequest, ToolCallResponse, ToolCost,
                ToolExecutionEvents, ToolExecutionStatus, ToolPolicySummary,
            },
        },
    },
    app_redis::AppRedis,
    config::agent::{
        AgentToolEgressPolicyConfig, AgentToolTargetConfig, AgentToolTargetKind, AgentToolsConfig,
    },
    types::extensions::AuthContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ToolExecutionErrorKind {
    #[error("tool target is unavailable")]
    ToolTargetUnavailable,
    #[error("tool execution failed")]
    ToolExecutionFailed,
}

#[derive(Clone)]
pub struct ToolExecutionContext {
    pub workspace_id: String,
    pub virtual_key_id: Option<String>,
    pub agent_id: String,
    pub caller_principal_id: String,
    pub target_id: String,
    pub target_revision: u64,
    pub target_hash: String,
    pub auth_revision: String,
    pub redis: Option<Arc<AppRedis>>,
    pub mcp_session_cache_ttl_secs: u64,
    pub mcp_session_lock_ttl_secs: u64,
    pub mcp_session_max_concurrent_per_session: usize,
    pub mcp_sse_max_event_bytes: usize,
    pub mcp_sse_max_line_bytes: usize,
    pub mcp_sse_max_events: usize,
    pub mcp_sse_max_batch_items: usize,
    pub mcp_sse_idle_timeout_ms: u64,
}

impl ToolExecutionContext {
    pub fn from_auth_and_target(
        auth: &AuthContext,
        agent_context: Option<&AgentContext>,
        target: &AgentToolTargetConfig,
        redis: Option<Arc<AppRedis>>,
        tools_cfg: &AgentToolsConfig,
    ) -> Self {
        let agent_id = agent_context
            .map(|ctx| {
                ctx.agent_identity_for_namespace(Some(*auth.org_id.as_ref()), auth.virtual_key_id)
            })
            .or_else(|| auth.registered_agent_name.clone())
            .unwrap_or_else(|| "unknown-agent".to_string());
        let target_id = target.tool_id.clone();
        let auth_revision = "0/static".to_string();
        let target_hash = match target.kind {
            AgentToolTargetKind::McpSse => {
                mcp_sse::target_hash::canonical_mcp_sse_target_hash(target, tools_cfg)
            }
            _ => mcp_streamable_http::target_hash::canonical_target_hash(
                target,
                0,
                &auth_revision,
                tools_cfg,
            ),
        };
        Self {
            workspace_id: auth.org_id.to_string(),
            virtual_key_id: auth.virtual_key_id.as_ref().map(ToString::to_string),
            agent_id,
            caller_principal_id: auth.entity_id.to_string(),
            target_id,
            target_revision: 0,
            target_hash,
            auth_revision,
            redis,
            mcp_session_cache_ttl_secs: tools_cfg.mcp_session_cache_ttl_secs,
            mcp_session_lock_ttl_secs: tools_cfg.mcp_session_lock_ttl_secs,
            mcp_session_max_concurrent_per_session: tools_cfg
                .mcp_session_max_concurrent_per_session,
            mcp_sse_max_event_bytes: tools_cfg.mcp_sse_max_event_bytes,
            mcp_sse_max_line_bytes: tools_cfg.mcp_sse_max_line_bytes,
            mcp_sse_max_events: tools_cfg.mcp_sse_max_events,
            mcp_sse_max_batch_items: tools_cfg.mcp_sse_max_batch_items,
            mcp_sse_idle_timeout_ms: tools_cfg.mcp_sse_idle_timeout_ms,
        }
    }

    #[cfg(test)]
    pub fn new_for_test(auth: &AuthContext, target: &AgentToolTargetConfig) -> Self {
        Self::from_auth_and_target(auth, None, target, None, &AgentToolsConfig::default())
    }
}

pub async fn execute_tool(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_request_bytes: usize,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    match target.kind {
        AgentToolTargetKind::Mock => Ok(mock_tool_response(target, request)),
        AgentToolTargetKind::Http => {
            execute_http_tool(
                target,
                request,
                egress_policy,
                default_timeout_ms,
                max_response_bytes,
            )
            .await
        }
        AgentToolTargetKind::McpHttp => execute_mcp_http_tool(
            target,
            request,
            egress_policy,
            default_timeout_ms,
            max_response_bytes,
        )
        .await
        .map_err(map_mcp_http_error),
        AgentToolTargetKind::McpStreamableHttp => {
            Err(ToolExecutionErrorKind::ToolTargetUnavailable)
        }
        AgentToolTargetKind::McpSse => Err(ToolExecutionErrorKind::ToolTargetUnavailable),
        AgentToolTargetKind::OpenApi => {
            openapi::executor::execute_openapi_tool(
                target,
                request,
                egress_policy,
                default_timeout_ms,
                max_request_bytes,
                max_response_bytes,
            )
            .await
        }
    }
}

pub async fn execute_tool_with_context(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_request_bytes: usize,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    match target.kind {
        AgentToolTargetKind::McpStreamableHttp => {
            mcp_streamable_http::execute_mcp_streamable_http_tool(
                ctx,
                target,
                request,
                egress_policy,
                default_timeout_ms,
                max_response_bytes,
            )
            .await
        }
        AgentToolTargetKind::McpSse => {
            mcp_sse::execute_mcp_sse_tool(
                ctx,
                target,
                request,
                egress_policy,
                default_timeout_ms,
                max_response_bytes,
            )
            .await
        }
        _ => {
            execute_tool(
                target,
                request,
                egress_policy,
                default_timeout_ms,
                max_request_bytes,
                max_response_bytes,
            )
            .await
        }
    }
}

fn map_mcp_http_error(error: McpHttpError) -> ToolExecutionErrorKind {
    match error {
        McpHttpError::TargetUrlMissing
        | McpHttpError::TargetUnavailable
        | McpHttpError::InitializeFailed
        | McpHttpError::CapabilityUnsupported
        | McpHttpError::Timeout => ToolExecutionErrorKind::ToolTargetUnavailable,
        McpHttpError::ProtocolError | McpHttpError::CallFailed | McpHttpError::ResponseTooLarge => {
            ToolExecutionErrorKind::ToolExecutionFailed
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod tests_support {
    use uuid::Uuid;

    use crate::types::{extensions::AuthContext, org::OrgId, secret::Secret, user::UserId};

    pub fn auth_context_for_executor_tests() -> AuthContext {
        AuthContext {
            api_key: Secret::from("test-key".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(Uuid::new_v4()),
            workspace_type: None,
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: "vk_test".to_string(),
            master_key_id: None,
            master_key_base_url: None,
            department_id: Uuid::nil(),
            entity_type: "agent".to_string(),
            entity_id: Uuid::new_v4(),
            entity_name: "support-agent".to_string(),
            registered_agent_name: Some("support-agent".to_string()),
            body_ttl_days: 30,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }
}

fn mock_tool_response(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
) -> ToolCallResponse {
    completed_response(
        target,
        request,
        serde_json::json!({
            "tool_id": request.tool_id,
            "arguments": request.arguments,
            "mocked": true,
            "observed_at": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

async fn execute_http_tool(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
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

    let response = client
        .post(parsed)
        .json(&serde_json::json!({
            "tool_id": request.tool_id,
            "arguments": request.arguments,
            "tool_call_id": request.tool_call_id,
            "tool_execution_id": request.tool_execution_id,
        }))
        .send()
        .await
        .map_err(|_| ToolExecutionErrorKind::ToolTargetUnavailable)?;
    if !response.status().is_success() {
        return Err(ToolExecutionErrorKind::ToolExecutionFailed);
    }
    let output = parse_limited_json_response(response, max_response_bytes).await?;

    Ok(completed_response(target, request, output))
}

async fn parse_limited_json_response(
    response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<serde_json::Value, ToolExecutionErrorKind> {
    if response
        .content_length()
        .is_some_and(|len| len > max_response_bytes as u64)
    {
        return Err(ToolExecutionErrorKind::ToolExecutionFailed);
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|_| ToolExecutionErrorKind::ToolExecutionFailed)?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or(ToolExecutionErrorKind::ToolExecutionFailed)?;
        if next_len > max_response_bytes {
            return Err(ToolExecutionErrorKind::ToolExecutionFailed);
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body).map_err(|_| ToolExecutionErrorKind::ToolExecutionFailed)
}

fn completed_response(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    output: serde_json::Value,
) -> ToolCallResponse {
    let tool_execution_id = request
        .tool_execution_id
        .clone()
        .unwrap_or_else(|| format!("exec_{}", uuid::Uuid::new_v4().simple()));
    let cost = ToolCost {
        amount_micros: target.rate_card.fixed_micros,
        currency: target.rate_card.currency.clone(),
        source: "rate_card".to_string(),
    };
    ToolCallResponse {
        status: ToolExecutionStatus::Completed,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output,
        error: None,
        gateway_metadata: None,
        billing: ToolBillingOverride {
            reason: "success".to_string(),
            billable: true,
            cost_micros: cost.amount_micros,
            currency: cost.currency.clone(),
            dedupe_key: tool_execution_id,
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

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::mpsc,
        thread,
    };

    use super::*;
    use crate::{
        config::agent::{
            AgentToolEgressPolicyConfig, AgentToolRateCardConfig, AgentToolTargetConfig,
            AgentToolTargetKind,
        },
        types::extensions::AuthContext,
    };

    const DEFAULT_HTTP_TIMEOUT_MS: u64 = 8000;

    fn test_egress_policy() -> AgentToolEgressPolicyConfig {
        AgentToolEgressPolicyConfig {
            https_only: false,
            block_loopback: false,
            ..AgentToolEgressPolicyConfig::default()
        }
    }

    #[test]
    fn tool_execution_context_builds_static_auth_revision() {
        let auth = AuthContext {
            api_key: crate::types::secret::Secret::from("test-key".to_string()),
            user_id: crate::types::user::UserId::new(uuid::Uuid::new_v4()),
            org_id: crate::types::org::OrgId::new(uuid::Uuid::new_v4()),
            workspace_type: None,
            virtual_key_id: Some(uuid::Uuid::new_v4()),
            virtual_key_prefix: "vk_test".to_string(),
            master_key_id: None,
            master_key_base_url: None,
            department_id: uuid::Uuid::nil(),
            entity_type: "agent".to_string(),
            entity_id: uuid::Uuid::new_v4(),
            entity_name: "support-agent".to_string(),
            registered_agent_name: Some("Support Agent".to_string()),
            body_ttl_days: 30,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        };
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpStreamableHttp,
            url: Some("https://mcp.example.com/mcp".to_string()),
            ..AgentToolTargetConfig::default()
        };

        let ctx = ToolExecutionContext::new_for_test(&auth, &target);

        assert_eq!(ctx.workspace_id, auth.org_id.to_string());
        assert_eq!(
            ctx.virtual_key_id.as_deref(),
            auth.virtual_key_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref()
        );
        assert_eq!(ctx.caller_principal_id, auth.entity_id.to_string());
        assert_eq!(ctx.auth_revision, "0/static");
        assert_eq!(ctx.target_id, "docs.search");
        assert!(ctx.target_hash.starts_with("sha256:"));
    }

    #[test]
    fn tool_execution_context_separates_mcp_sse_target_hash() {
        let auth = AuthContext {
            api_key: crate::types::secret::Secret::from("test-key".to_string()),
            user_id: crate::types::user::UserId::new(uuid::Uuid::new_v4()),
            org_id: crate::types::org::OrgId::new(uuid::Uuid::new_v4()),
            workspace_type: None,
            virtual_key_id: Some(uuid::Uuid::new_v4()),
            virtual_key_prefix: "vk_test".to_string(),
            master_key_id: None,
            master_key_base_url: None,
            department_id: uuid::Uuid::nil(),
            entity_type: "agent".to_string(),
            entity_id: uuid::Uuid::new_v4(),
            entity_name: "support-agent".to_string(),
            registered_agent_name: Some("Support Agent".to_string()),
            body_ttl_days: 30,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        };
        let sse_target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpSse,
            url: Some("https://mcp.example.com/sse".to_string()),
            ..AgentToolTargetConfig::default()
        };
        let streamable_target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::McpStreamableHttp,
            ..sse_target.clone()
        };

        let sse_ctx = ToolExecutionContext::new_for_test(&auth, &sse_target);
        let streamable_ctx = ToolExecutionContext::new_for_test(&auth, &streamable_target);

        assert!(sse_ctx.target_hash.starts_with("sha256:"));
        assert_ne!(sse_ctx.target_hash, streamable_ctx.target_hash);
    }

    #[tokio::test]
    async fn openapi_target_without_url_is_unavailable() {
        let target = AgentToolTargetConfig {
            tool_id: "support.lookup_ticket".to_string(),
            kind: AgentToolTargetKind::OpenApi,
            rate_card: AgentToolRateCardConfig {
                fixed_micros: 4200,
                currency: "USD".to_string(),
            },
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "support.lookup_ticket".to_string(),
            tool_call_id: Some("call-openapi".to_string()),
            tool_execution_id: Some("exec-openapi".to_string()),
            arguments: serde_json::json!({ "ticket_id": "T-123" }),
            ..ToolCallRequest::default()
        };

        let error = execute_tool(
            &target,
            &request,
            &AgentToolEgressPolicyConfig::default(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("OpenAPI URL missing");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    #[tokio::test]
    async fn mock_target_returns_completed_response() {
        let target = AgentToolTargetConfig {
            tool_id: "support.echo".to_string(),
            rate_card: AgentToolRateCardConfig {
                fixed_micros: 2500,
                currency: "USD".to_string(),
            },
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "support.echo".to_string(),
            tool_call_id: Some("call_1".to_string()),
            tool_execution_id: Some("exec_1".to_string()),
            arguments: serde_json::json!({ "message": "hello" }),
            ..ToolCallRequest::default()
        };

        let response = execute_tool(
            &target,
            &request,
            &AgentToolEgressPolicyConfig::default(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect("mock target executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(response.tool_execution_id, "exec_1");
        assert_eq!(response.output["tool_id"], "support.echo");
        assert_eq!(response.output["arguments"]["message"], "hello");
        assert_eq!(response.output["mocked"], true);
        assert!(response.output["observed_at"].is_string());
        assert_eq!(response.cost.amount_micros, 2500);
        assert_eq!(response.cost.currency, "USD");
        assert_eq!(response.cost.source, "rate_card");
        assert_eq!(response.policy.decision, "allowed");
        assert_eq!(response.policy.reason, "tool_allowed");
        assert_eq!(response.events, ToolExecutionEvents::default());
    }

    #[tokio::test]
    async fn mock_target_generates_execution_id_when_missing() {
        let target = AgentToolTargetConfig::default();
        let request = ToolCallRequest {
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };

        let response = execute_tool(
            &target,
            &request,
            &AgentToolEgressPolicyConfig::default(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect("mock target executes");

        assert!(response.tool_execution_id.starts_with("exec_"));
        assert_eq!(response.tool_execution_id.len(), "exec_".len() + 32);
    }

    #[tokio::test]
    async fn http_target_posts_arguments_and_normalizes_response() {
        let Some(fixture) = HttpToolFixture::start(200, r#"{"ok":true}"#) else {
            return;
        };
        let target = AgentToolTargetConfig {
            tool_id: "support.http".to_string(),
            kind: AgentToolTargetKind::Http,
            url: Some(fixture.url()),
            rate_card: AgentToolRateCardConfig {
                fixed_micros: 125,
                currency: "USD".to_string(),
            },
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "support.http".to_string(),
            tool_call_id: Some("call-http".to_string()),
            arguments: serde_json::json!({ "ticket_id": "T-1" }),
            ..ToolCallRequest::default()
        };

        let response = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect("http target executes");
        let observed = fixture.receive_body();

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.tool_call_id.as_deref(), Some("call-http"));
        assert!(response.tool_execution_id.starts_with("exec_"));
        assert_eq!(response.output, serde_json::json!({ "ok": true }));
        assert_eq!(response.cost.amount_micros, 125);
        assert_eq!(observed["tool_id"], "support.http");
        assert_eq!(observed["arguments"]["ticket_id"], "T-1");
        assert_eq!(observed["tool_call_id"], "call-http");
    }

    #[tokio::test]
    async fn http_target_rejects_non_http_url() {
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::Http,
            url: Some("file:///tmp/tool".to_string()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("non-http URL rejected");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    #[tokio::test]
    async fn http_target_rejects_blocked_egress_before_request() {
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::Http,
            url: Some("http://127.0.0.1/tool".to_string()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &AgentToolEgressPolicyConfig::default(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("blocked egress should make target unavailable");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    #[tokio::test]
    async fn http_target_failure_status_is_normalized() {
        let Some(fixture) = HttpToolFixture::start(500, r#"{"error":"bad"}"#) else {
            return;
        };
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::Http,
            url: Some(fixture.url()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("HTTP 500 should fail");

        assert_eq!(error, ToolExecutionErrorKind::ToolExecutionFailed);
    }

    #[tokio::test]
    async fn http_target_response_over_limit_is_normalized() {
        let Some(fixture) = HttpToolFixture::start(200, r#"{"ok":true}"#) else {
            return;
        };
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::Http,
            url: Some(fixture.url()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            4,
        )
        .await
        .expect_err("oversized response should fail");

        assert_eq!(error, ToolExecutionErrorKind::ToolExecutionFailed);
    }

    #[test]
    fn request_timeout_cannot_exceed_configured_timeout() {
        assert_eq!(
            effective_timeout_ms(Some(100), Some(1_000), DEFAULT_HTTP_TIMEOUT_MS),
            100
        );
        assert_eq!(
            effective_timeout_ms(Some(100), Some(50), DEFAULT_HTTP_TIMEOUT_MS),
            50
        );
        assert_eq!(
            effective_timeout_ms(None, Some(16_000), DEFAULT_HTTP_TIMEOUT_MS),
            DEFAULT_HTTP_TIMEOUT_MS
        );
    }

    #[tokio::test]
    async fn http_target_is_unavailable_without_url() {
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::Http,
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("HTTP URL missing");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    #[tokio::test]
    async fn mcp_http_target_without_url_is_unavailable() {
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::McpHttp,
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("MCP URL missing");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    #[tokio::test]
    async fn mcp_sse_target_requires_context_dispatch() {
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpSse,
            url: Some("https://mcp.example.com/sse".to_string()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };

        let error = execute_tool(
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("MCP SSE must use context-aware execution");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    #[tokio::test]
    async fn mcp_http_target_rejects_blocked_egress_before_request() {
        let target = AgentToolTargetConfig {
            kind: AgentToolTargetKind::McpHttp,
            url: Some("http://127.0.0.1/tool".to_string()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest::default();

        let error = execute_tool(
            &target,
            &request,
            &AgentToolEgressPolicyConfig::default(),
            DEFAULT_HTTP_TIMEOUT_MS,
            65_536,
            65_536,
        )
        .await
        .expect_err("blocked egress should make MCP target unavailable");

        assert_eq!(error, ToolExecutionErrorKind::ToolTargetUnavailable);
    }

    struct HttpToolFixture {
        url: String,
        receiver: mpsc::Receiver<String>,
    }

    impl HttpToolFixture {
        fn start(status: u16, response_body: &'static str) -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping HTTP target loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind test server: {err}"),
            };
            let addr = listener.local_addr().expect("test server addr");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept test request");
                let body = read_http_request_body(&mut stream);
                tx.send(body).expect("send observed request body");
                let status_line = match status {
                    200..=299 => format!("HTTP/1.1 {status} OK"),
                    _ => format!("HTTP/1.1 {status} Error"),
                };
                let response = format!(
                    "{status_line}\r\nContent-Type: \
                     application/json\r\nContent-Length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            });

            Some(Self {
                url: format!("http://{addr}/tool"),
                receiver: rx,
            })
        }

        fn url(&self) -> String {
            self.url.clone()
        }

        fn receive_body(self) -> serde_json::Value {
            let body = self.receiver.recv().expect("observed request");
            serde_json::from_str(&body).expect("request body JSON")
        }
    }

    fn read_http_request_body(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(body) = body_from_complete_request(&bytes) {
                return body;
            }
        }
        String::new()
    }

    fn body_from_complete_request(bytes: &[u8]) -> Option<String> {
        let request = String::from_utf8_lossy(bytes);
        let (headers, body) = request.split_once("\r\n\r\n")?;
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })?;
        (body.len() >= content_length).then(|| body[..content_length].to_string())
    }
}
