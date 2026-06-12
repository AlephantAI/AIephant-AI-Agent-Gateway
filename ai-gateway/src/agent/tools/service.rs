use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use http::{Method, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::Service;

use crate::{
    agent::{
        context::AgentContext,
        headers::parse_agent_context_from_headers,
        policy::validate_agent_policy,
        sink::emit_agent_event,
        tools::{
            audit::{tool_call_requested_event, tool_execution_completed_event_with_sequence},
            catalog::{find_callable_tool, schema_hash_for_value, visible_tools},
            executor::{ToolExecutionContext, ToolExecutionErrorKind, execute_tool_with_context},
            idempotency::arguments_hash,
            openapi::outcome::{OpenApiOutcomeInput, OpenApiOutcomeStatus, decide},
            response::{
                AgentAction, CostStage, ToolCallCost, ToolCallEnvelope, ToolErrorEnvelope,
                ToolEventIds, ToolExecutionStatus as EnvelopeExecutionStatus, ToolPolicyEnvelope,
            },
            schema_validator::validate_arguments,
            types::{
                ToolBillingOverride, ToolCallRequest, ToolCallResponse, ToolCost,
                ToolExecutionErrorEnvelope, ToolExecutionEvents,
                ToolExecutionStatus as InternalExecutionStatus, ToolGatewayMetadata,
                ToolListRequest, ToolListResponse, ToolPolicySummary,
            },
        },
    },
    app_state::AppState,
    config::agent::{AgentToolTargetConfig, AgentToolTargetKind, AgentToolsConfig},
    router::router_details::{AgentToolsRouteAction, RouteType},
    types::{
        body::Body,
        extensions::{AuthContext, RequestContext},
        request::Request,
        response::Response,
    },
};

#[derive(Debug, Clone)]
pub struct AgentToolsService {
    app_state: AppState,
    concurrency_limiter: AgentToolsConcurrencyLimiter,
}

impl AgentToolsService {
    #[must_use]
    pub fn new(app_state: AppState) -> Self {
        Self {
            app_state,
            concurrency_limiter: AgentToolsConcurrencyLimiter::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct AgentToolsConcurrencyLimiter {
    workspaces: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

#[derive(Debug)]
enum AgentToolsWorkspacePermit {
    Unlimited,
    Limited { _permit: OwnedSemaphorePermit },
}

impl AgentToolsConcurrencyLimiter {
    fn try_acquire_workspace(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Option<AgentToolsWorkspacePermit> {
        if limit == 0 {
            return Some(AgentToolsWorkspacePermit::Unlimited);
        }

        let semaphore = {
            let mut workspaces = self.workspaces.lock().expect("agent tools limiter lock");
            workspaces
                .entry(workspace_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(limit)))
                .clone()
        };

        semaphore
            .try_acquire_owned()
            .ok()
            .map(|permit| AgentToolsWorkspacePermit::Limited { _permit: permit })
    }
}

impl Service<Request> for AgentToolsService {
    type Response = Response;
    type Error = Infallible;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let app_state = self.app_state.clone();
        let concurrency_limiter = self.concurrency_limiter.clone();
        Box::pin(async move {
            Ok(handle_agent_tools(app_state, concurrency_limiter, req)
                .await
                .unwrap_or_else(error_response))
        })
    }
}

#[cfg(feature = "testing")]
pub fn prepare_e2e_request(
    mut request: Request,
    action: &str,
    auth_context: AuthContext,
) -> Request {
    let route_action = match action {
        "list" => AgentToolsRouteAction::List,
        "call" => AgentToolsRouteAction::Call,
        _ => panic!("unsupported Agent Tools E2E action: {action}"),
    };
    request.extensions_mut().insert(RouteType::AgentTools {
        action: route_action,
    });
    request.extensions_mut().insert(auth_context);
    request
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum AgentToolsServiceError {
    #[error("agent tools gateway is disabled")]
    Disabled,
    #[error("agent tools route only supports POST")]
    MethodNotAllowed,
    #[error("agent tools route type is missing")]
    MissingRoute,
    #[error("agent tools authentication context is missing")]
    MissingAuth,
    #[error("agent tools request payload is too large")]
    PayloadTooLarge,
    #[error("agent tools request payload is invalid JSON")]
    InvalidJson,
    #[error("agent tool arguments do not satisfy the input schema")]
    InvalidArguments,
    #[error("agent tools call requires tool_id")]
    ToolIdRequired,
    #[error("agent tool is not allowed")]
    ToolNotAllowed,
    #[error("agent tool target is unavailable")]
    ToolTargetUnavailable,
    #[error("agent tool execution failed")]
    ToolExecutionFailed,
}

impl AgentToolsServiceError {
    const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "agent_tools_disabled",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::MissingRoute => "missing_route",
            Self::MissingAuth => "missing_auth",
            Self::PayloadTooLarge => "payload_too_large",
            Self::InvalidJson => "invalid_json",
            Self::InvalidArguments => "invalid_arguments",
            Self::ToolIdRequired => "tool_id_required",
            Self::ToolNotAllowed => "tool_not_allowed",
            Self::ToolTargetUnavailable => "tool_target_unavailable",
            Self::ToolExecutionFailed => "tool_execution_failed",
        }
    }
}

async fn handle_agent_tools(
    app_state: AppState,
    concurrency_limiter: AgentToolsConcurrencyLimiter,
    req: Request,
) -> Result<Response, AgentToolsServiceError> {
    let agent_enabled = app_state.config().agent.enabled;
    let allow_header_context = app_state.config().agent.allow_header_context;
    let max_header_value_bytes = app_state.config().agent.max_header_value_bytes;
    let tools_cfg = app_state.config().agent.tools.clone();
    if !agent_enabled || !tools_cfg.enabled {
        return Err(AgentToolsServiceError::Disabled);
    }
    if req.method() != Method::POST {
        return Err(AgentToolsServiceError::MethodNotAllowed);
    }

    let (parts, body) = req.into_parts();
    let action = match parts.extensions.get::<RouteType>() {
        Some(RouteType::AgentTools { action }) => *action,
        _ => return Err(AgentToolsServiceError::MissingRoute),
    };
    tracing::trace!(action = %action.as_str(), "handling agent tools request");

    let auth_ctx = auth_context_from_extensions(&parts.extensions)
        .ok_or(AgentToolsServiceError::MissingAuth)?;
    let parsed_header_context = allow_header_context
        .then(|| parse_agent_context_from_headers(&parts.headers, max_header_value_bytes))
        .flatten();
    let header_context =
        agent_context_from_extensions(&parts.extensions).or(parsed_header_context.as_ref());
    let body = Limited::new(body, tools_cfg.max_request_bytes)
        .collect()
        .await
        .map_err(|_| AgentToolsServiceError::PayloadTooLarge)?
        .to_bytes();

    if matches!(action, AgentToolsRouteAction::Call) {
        return handle_call(
            &app_state,
            &concurrency_limiter,
            auth_ctx,
            header_context,
            &tools_cfg,
            &body,
        )
        .await;
    }

    let request: ToolListRequest =
        serde_json::from_slice(&body).map_err(|_| AgentToolsServiceError::InvalidJson)?;
    let tools = visible_tools(
        auth_ctx,
        &tools_cfg.targets,
        request.agent_id.as_deref(),
        tools_cfg.timeout_ms,
    );

    json_response(
        StatusCode::OK,
        &ToolListResponse {
            snapshot_revision: 0,
            policy_revision: 0,
            snapshot_source: "static".to_string(),
            tools,
        },
    )
}

async fn handle_call(
    app_state: &AppState,
    concurrency_limiter: &AgentToolsConcurrencyLimiter,
    auth_ctx: &AuthContext,
    header_context: Option<&AgentContext>,
    tools_cfg: &AgentToolsConfig,
    body: &[u8],
) -> Result<Response, AgentToolsServiceError> {
    let mut request: ToolCallRequest =
        serde_json::from_slice(body).map_err(|_| AgentToolsServiceError::InvalidJson)?;
    if request.tool_id.trim().is_empty() {
        return Err(AgentToolsServiceError::ToolIdRequired);
    }
    let tool_execution_id = format!("exec_{}", uuid::Uuid::new_v4().simple());
    request.tool_execution_id = Some(tool_execution_id.clone());

    if let Some(reason) = tool_call_static_snapshot_guard_stale(&request, None) {
        let response = stale_snapshot_response(&request, tool_execution_id, 0, reason);
        return json_response(StatusCode::OK, &response);
    }

    let target = find_callable_tool(
        auth_ctx,
        &tools_cfg.targets,
        request.agent_id.as_deref(),
        &request.tool_id,
    )
    .ok_or(AgentToolsServiceError::ToolNotAllowed)?;

    if let Some(reason) = tool_call_static_snapshot_guard_stale(&request, Some(target)) {
        if target.kind == AgentToolTargetKind::OpenApi {
            return openapi_pre_execution_failure_response_with_event(
                app_state,
                auth_ctx,
                header_context,
                target,
                &request,
                tool_execution_id,
                OpenApiOutcomeStatus::SnapshotStale,
                1,
                "snapshot-stale",
            )
            .await;
        }
        let response = stale_snapshot_response(&request, tool_execution_id, 0, reason);
        return json_response(StatusCode::OK, &response);
    }

    if tools_cfg.schema_validation_enabled {
        if let Err(error) = validate_arguments(&target.input_schema, &request.arguments) {
            tracing::debug!(
                error = %error,
                tool_id = %request.tool_id,
                "agent tool call arguments failed schema validation"
            );
            if target.kind == AgentToolTargetKind::OpenApi {
                return openapi_pre_execution_failure_response_with_event(
                    app_state,
                    auth_ctx,
                    header_context,
                    target,
                    &request,
                    tool_execution_id,
                    OpenApiOutcomeStatus::SchemaInvalid,
                    1,
                    "schema-invalid",
                )
                .await;
            }
            return Err(AgentToolsServiceError::InvalidArguments);
        }
    }

    let _workspace_permit = match concurrency_limiter.try_acquire_workspace(
        auth_ctx.org_id.to_string().as_str(),
        tools_cfg.max_concurrent_per_workspace,
    ) {
        Some(permit) => permit,
        None => {
            let mut response = blocked_before_dispatch_response(
                &request,
                "agent_tools_concurrency_limited",
                tool_execution_id,
            );
            enrich_target_gateway_metadata(target, &request, &mut response, tools_cfg);
            if let Some(metadata) = response.gateway_metadata.as_mut() {
                metadata.failure_stage = Some("concurrency".to_string());
                metadata.billing_status = Some("waived".to_string());
                metadata.billing_reason = Some("agent_tools_concurrency_limited".to_string());
                metadata.executed = Some(false);
            }
            response.output = serde_json::json!({
                "error": {
                    "code": "agent_tools_concurrency_limited",
                    "retryable": false,
                    "message": "agent tool call blocked before dispatch",
                },
                "metadata": {
                    "billing": {
                        "reason": "agent_tools_concurrency_limited",
                        "billable": false,
                        "dedupeKey": response.billing.dedupe_key.clone(),
                    },
                    "gateway": {
                        "targetKind": target_kind_for_metadata(target.kind),
                        "targetId": target.tool_id.clone(),
                        "executed": false,
                        "failureStage": "concurrency",
                        "failureClass": "agent_tools_concurrency_limited",
                    },
                },
            });
            return json_response(
                StatusCode::TOO_MANY_REQUESTS,
                &envelope_from_response(&request, &response),
            );
        }
    };

    let requested_event = tool_call_requested_event(
        auth_ctx,
        header_context,
        &request,
        &tool_execution_id,
        1,
        requested_tool_operation_metadata(target, &request, tools_cfg),
    );
    if let Err(err) = emit_agent_event(app_state, auth_ctx, &requested_event).await {
        if is_fail_closed_risk_level(&target.risk_level) {
            tracing::warn!(
                error = %err,
                event_id = %requested_event.event_id,
                tool_id = %request.tool_id,
                risk_level = %target.risk_level,
                "agent tool requested audit event sink failed; blocking high-risk tool call"
            );
            let response = audit_unavailable_response(
                &request,
                tool_execution_id,
                requested_event.event_id.clone(),
            );
            return json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &envelope_from_response(&request, &response),
            );
        }

        tracing::warn!(
            error = %err,
            event_id = %requested_event.event_id,
            tool_id = %request.tool_id,
            risk_level = %target.risk_level,
            "agent tool requested audit event sink failed; continuing low-risk tool call"
        );
    }

    if target_requires_policy_preflight(target.kind) {
        match validate_agent_policy(app_state, auth_ctx, &requested_event).await {
            Ok(decision) if !decision.allowed => {
                return policy_blocked_response_with_event(
                    app_state,
                    auth_ctx,
                    header_context,
                    target,
                    &request,
                    tool_execution_id,
                    requested_event.event_id.clone(),
                    &decision.policy_decision,
                    &decision.reason,
                    tools_cfg,
                )
                .await;
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    event_id = %requested_event.event_id,
                    tool_id = %request.tool_id,
                    target_kind = ?target.kind,
                    "agent tool policy preflight failed; blocking tool call"
                );
                return policy_blocked_response_with_event(
                    app_state,
                    auth_ctx,
                    header_context,
                    target,
                    &request,
                    tool_execution_id,
                    requested_event.event_id.clone(),
                    "blocked",
                    "policy_unavailable",
                    tools_cfg,
                )
                .await;
            }
        }
    }

    if let Some(limit) = tools_cfg.budget.max_tool_call_cost_micros {
        let estimated = estimate_tool_cost_micros(target);
        if estimated > limit {
            let mut response =
                blocked_before_dispatch_response(&request, "budget_blocked", tool_execution_id);
            enrich_target_gateway_metadata(target, &request, &mut response, tools_cfg);
            response.events.started_event_id = Some(requested_event.event_id.clone());
            let completed_event = tool_execution_completed_event_with_sequence(
                auth_ctx,
                header_context,
                &request,
                &response,
                2,
            );
            response.events.completed_event_id = Some(completed_event.event_id.clone());
            if let Err(err) = emit_agent_event(app_state, auth_ctx, &completed_event).await {
                tracing::warn!(
                    error = %err,
                    event_id = %completed_event.event_id,
                    tool_id = %request.tool_id,
                    "agent tool budget blocked audit event sink failed"
                );
            }
            return json_response(StatusCode::OK, &envelope_from_response(&request, &response));
        }
    }

    let execution_ctx = ToolExecutionContext::from_auth_and_target(
        auth_ctx,
        header_context,
        target,
        app_state.redis().cloned(),
        tools_cfg,
    );
    let mut response = execute_tool_with_context(
        &execution_ctx,
        target,
        &request,
        &tools_cfg.egress_policy,
        tools_cfg.timeout_ms,
        tools_cfg.max_request_bytes,
        tools_cfg.max_response_bytes,
    )
    .await
    .map_err(|error| match error {
        ToolExecutionErrorKind::ToolTargetUnavailable => {
            AgentToolsServiceError::ToolTargetUnavailable
        }
        ToolExecutionErrorKind::ToolExecutionFailed => AgentToolsServiceError::ToolExecutionFailed,
    })?;
    enrich_target_gateway_metadata(target, &request, &mut response, tools_cfg);
    response.events.started_event_id = Some(requested_event.event_id.clone());
    let completed_event = tool_execution_completed_event_with_sequence(
        auth_ctx,
        header_context,
        &request,
        &response,
        2,
    );
    response.events.completed_event_id = Some(completed_event.event_id.clone());
    if let Err(err) = emit_agent_event(app_state, auth_ctx, &completed_event).await {
        tracing::warn!(
            error = %err,
            event_id = %completed_event.event_id,
            tool_id = %request.tool_id,
            "agent tool audit event sink failed"
        );
    }

    json_response(StatusCode::OK, &envelope_from_response(&request, &response))
}

fn is_fail_closed_risk_level(risk_level: &str) -> bool {
    matches!(
        risk_level.trim().to_ascii_lowercase().as_str(),
        "high" | "critical"
    )
}

fn target_requires_policy_preflight(kind: AgentToolTargetKind) -> bool {
    matches!(
        kind,
        AgentToolTargetKind::OpenApi
            | AgentToolTargetKind::McpStreamableHttp
            | AgentToolTargetKind::McpSse
    )
}

fn audit_unavailable_response(
    request: &ToolCallRequest,
    tool_execution_id: String,
    requested_event_id: String,
) -> ToolCallResponse {
    ToolCallResponse {
        status: InternalExecutionStatus::Failed,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output: serde_json::json!({
            "error": {
                "code": "audit_unavailable",
                "retryable": true,
                "message": "audit sink unavailable; high-risk tool execution cannot proceed",
            }
        }),
        error: Some(ToolExecutionErrorEnvelope {
            code: "audit_unavailable".to_string(),
            message: "audit sink unavailable; high-risk tool execution cannot \
                      proceed"
                .to_string(),
            retryable: true,
        }),
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind: String::new(),
            target_id: request.tool_id.clone(),
            target_hash: String::new(),
            auth_revision: "0/static".to_string(),
            cache_hit: false,
            reinitialized: false,
            protocol_version: None,
            sse_used: false,
            failure_class: Some("audit_unavailable".to_string()),
            blocked_before_dispatch: true,
            latency_ms: None,
            ..ToolGatewayMetadata::default()
        }),
        billing: ToolBillingOverride {
            reason: "audit_unavailable".to_string(),
            billable: false,
            cost_micros: 0,
            currency: "USD".to_string(),
            dedupe_key: tool_execution_id,
        },
        cost: ToolCost {
            amount_micros: 0,
            currency: "USD".to_string(),
            source: "waived".to_string(),
        },
        policy: ToolPolicySummary {
            allowed: false,
            decision: "blocked".to_string(),
            reason: "audit_unavailable".to_string(),
        },
        events: ToolExecutionEvents {
            started_event_id: Some(requested_event_id),
            completed_event_id: None,
        },
    }
}

fn blocked_before_dispatch_response(
    request: &ToolCallRequest,
    reason: &str,
    tool_execution_id: String,
) -> ToolCallResponse {
    ToolCallResponse {
        status: InternalExecutionStatus::Blocked,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output: serde_json::json!({
            "error": {
                "code": reason,
                "retryable": false,
                "message": "agent tool call blocked before dispatch",
            }
        }),
        error: Some(ToolExecutionErrorEnvelope {
            code: reason.to_string(),
            message: "agent tool call blocked before dispatch".to_string(),
            retryable: false,
        }),
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind: String::new(),
            target_id: request.tool_id.clone(),
            target_hash: String::new(),
            auth_revision: "0/static".to_string(),
            cache_hit: false,
            reinitialized: false,
            protocol_version: None,
            sse_used: false,
            failure_class: Some(reason.to_string()),
            blocked_before_dispatch: true,
            latency_ms: None,
            ..ToolGatewayMetadata::default()
        }),
        billing: ToolBillingOverride {
            reason: reason.to_string(),
            billable: false,
            cost_micros: 0,
            currency: "USD".to_string(),
            dedupe_key: tool_execution_id,
        },
        cost: ToolCost {
            amount_micros: 0,
            currency: "USD".to_string(),
            source: "waived".to_string(),
        },
        policy: ToolPolicySummary {
            allowed: false,
            decision: "blocked".to_string(),
            reason: reason.to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

fn openapi_pre_execution_failure_response(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tool_execution_id: String,
    status: OpenApiOutcomeStatus,
) -> ToolCallResponse {
    let decision = decide(OpenApiOutcomeInput {
        status,
        fixed_micros: target.rate_card.fixed_micros,
        currency: target.rate_card.currency.clone(),
        charge_on_failure: false,
        tool_execution_id: tool_execution_id.clone(),
    });
    let failure_class = decision
        .error
        .as_ref()
        .map(|error| error.code.clone())
        .unwrap_or_else(|| decision.billing_reason.clone());

    ToolCallResponse {
        status: decision.status,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output: serde_json::json!({
            "error": decision.error.as_ref().map(|error| {
                serde_json::json!({
                    "code": error.code,
                    "retryable": error.retryable,
                    "message": error.message,
                })
            }),
            "metadata": {
                "billing": {
                    "reason": decision.billing.reason.clone(),
                    "billable": decision.billing.billable,
                    "dedupeKey": decision.billing.dedupe_key.clone(),
                },
                "gateway": {
                    "targetKind": "openapi",
                    "executed": decision.executed,
                    "failureStage": decision.failure_stage.clone(),
                },
            },
        }),
        error: decision.error,
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind: "openapi".to_string(),
            target_id: target.tool_id.clone(),
            target_hash: openapi_target_hash(target, request),
            auth_revision: "0/static".to_string(),
            cache_hit: false,
            reinitialized: false,
            protocol_version: None,
            sse_used: false,
            failure_class: Some(failure_class),
            blocked_before_dispatch: true,
            latency_ms: Some(0),
            billing_status: Some(decision.billing_status),
            billing_reason: Some(decision.billing_reason),
            executed: Some(decision.executed),
            failure_stage: Some(decision.failure_stage),
            service_slug: Some(openapi_service_slug(target)),
            operation_id: Some(openapi_operation_id(target)),
            operation_slug: Some(openapi_operation_slug(target)),
            target_revision: openapi_target_revision(request),
            schema_hash: Some(schema_hash_for_value(&target.input_schema)),
            rate_card_revision: Some(0),
            ..ToolGatewayMetadata::default()
        }),
        billing: decision.billing,
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

async fn openapi_pre_execution_failure_response_with_event(
    app_state: &AppState,
    auth_ctx: &AuthContext,
    header_context: Option<&AgentContext>,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tool_execution_id: String,
    status: OpenApiOutcomeStatus,
    sequence: u64,
    log_label: &str,
) -> Result<Response, AgentToolsServiceError> {
    let mut response =
        openapi_pre_execution_failure_response(target, request, tool_execution_id, status);
    let completed_event = tool_execution_completed_event_with_sequence(
        auth_ctx,
        header_context,
        request,
        &response,
        sequence,
    );
    response.events.completed_event_id = Some(completed_event.event_id.clone());
    if let Err(err) = emit_agent_event(app_state, auth_ctx, &completed_event).await {
        tracing::warn!(
            error = %err,
            event_id = %completed_event.event_id,
            tool_id = %request.tool_id,
            failure = %log_label,
            "agent OpenAPI pre-execution terminal audit event sink failed"
        );
    }
    json_response(StatusCode::OK, &envelope_from_response(request, &response))
}

#[allow(clippy::too_many_arguments)]
async fn policy_blocked_response_with_event(
    app_state: &AppState,
    auth_ctx: &AuthContext,
    header_context: Option<&AgentContext>,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tool_execution_id: String,
    requested_event_id: String,
    policy_decision: &str,
    policy_reason: &str,
    tools_cfg: &AgentToolsConfig,
) -> Result<Response, AgentToolsServiceError> {
    let mut response = match target.kind {
        AgentToolTargetKind::OpenApi => openapi_pre_execution_failure_response(
            target,
            request,
            tool_execution_id,
            OpenApiOutcomeStatus::PolicyBlocked,
        ),
        _ => generic_policy_blocked_response(
            target,
            request,
            tool_execution_id,
            policy_reason,
            tools_cfg,
        ),
    };
    response.events.started_event_id = Some(requested_event_id);
    response.policy = ToolPolicySummary {
        allowed: false,
        decision: policy_decision.to_string(),
        reason: policy_reason.to_string(),
    };
    let completed_event = tool_execution_completed_event_with_sequence(
        auth_ctx,
        header_context,
        request,
        &response,
        2,
    );
    response.events.completed_event_id = Some(completed_event.event_id.clone());
    if let Err(err) = emit_agent_event(app_state, auth_ctx, &completed_event).await {
        tracing::warn!(
            error = %err,
            event_id = %completed_event.event_id,
            tool_id = %request.tool_id,
            target_kind = ?target.kind,
            "agent tool policy blocked terminal audit event sink failed"
        );
    }
    json_response(StatusCode::OK, &envelope_from_response(request, &response))
}

fn generic_policy_blocked_response(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tool_execution_id: String,
    policy_reason: &str,
    tools_cfg: &AgentToolsConfig,
) -> ToolCallResponse {
    let target_kind = target_kind_for_metadata(target.kind).to_string();
    let target_hash = target_hash_for_gateway_metadata(target, request, tools_cfg);

    ToolCallResponse {
        status: InternalExecutionStatus::Blocked,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output: serde_json::json!({
            "error": {
                "code": if policy_reason == "policy_unavailable" {
                    "tool_policy_unavailable"
                } else {
                    "tool_policy_denied"
                },
                "retryable": policy_reason == "policy_unavailable",
                "message": if policy_reason == "policy_unavailable" {
                    "Agent tool policy is unavailable"
                } else {
                    "Agent tool call was blocked by policy"
                },
            },
            "metadata": {
                "billing": {
                    "reason": "policy_blocked",
                    "billable": false,
                    "dedupeKey": format!("tool_execution:{tool_execution_id}"),
                },
                "gateway": {
                    "targetKind": target_kind,
                    "executed": false,
                    "failureStage": "policy",
                },
            },
        }),
        error: Some(ToolExecutionErrorEnvelope {
            code: if policy_reason == "policy_unavailable" {
                "tool_policy_unavailable".to_string()
            } else {
                "tool_policy_denied".to_string()
            },
            message: if policy_reason == "policy_unavailable" {
                "Agent tool policy is unavailable".to_string()
            } else {
                "Agent tool call was blocked by policy".to_string()
            },
            retryable: policy_reason == "policy_unavailable",
        }),
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind,
            target_id: target.tool_id.clone(),
            target_hash,
            auth_revision: "0/static".to_string(),
            cache_hit: false,
            reinitialized: false,
            protocol_version: None,
            sse_used: target.kind == AgentToolTargetKind::McpSse,
            failure_class: Some(policy_reason.to_string()),
            blocked_before_dispatch: true,
            latency_ms: Some(0),
            billing_status: Some("waived".to_string()),
            billing_reason: Some("policy_blocked".to_string()),
            executed: Some(false),
            failure_stage: Some("policy".to_string()),
            target_revision: Some(0),
            schema_hash: Some(schema_hash_for_value(&target.input_schema)),
            rate_card_revision: Some(0),
            ..ToolGatewayMetadata::default()
        }),
        billing: ToolBillingOverride {
            reason: "policy_blocked".to_string(),
            billable: false,
            cost_micros: 0,
            currency: target.rate_card.currency.clone(),
            dedupe_key: format!("tool_execution:{tool_execution_id}"),
        },
        cost: ToolCost {
            amount_micros: 0,
            currency: target.rate_card.currency.clone(),
            source: "waived".to_string(),
        },
        policy: ToolPolicySummary {
            allowed: false,
            decision: "blocked".to_string(),
            reason: policy_reason.to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

const fn estimate_tool_cost_micros(target: &AgentToolTargetConfig) -> u64 {
    target.rate_card.fixed_micros
}

fn tool_call_static_snapshot_guard_stale(
    request: &ToolCallRequest,
    target: Option<&AgentToolTargetConfig>,
) -> Option<ToolCallStaleReason> {
    if let Some(requested) = request.snapshot_revision.filter(|revision| *revision != 0) {
        return Some(ToolCallStaleReason::SnapshotRevision {
            requested,
            active: 0,
        });
    }

    let Some(target) = target else {
        return None;
    };

    if let Some(requested) = request.tool_version.filter(|version| *version != 0) {
        return Some(ToolCallStaleReason::ToolVersion {
            requested,
            active: 0,
        });
    }

    if let Some(requested) = request.target_revision.filter(|revision| *revision != 0) {
        return Some(ToolCallStaleReason::TargetRevision {
            requested,
            active: 0,
        });
    }

    if let Some(requested_schema_hash) = request.schema_hash.as_deref() {
        let active_schema_hash = schema_hash_for_value(&target.input_schema);
        if requested_schema_hash != active_schema_hash {
            return Some(ToolCallStaleReason::SchemaHash {
                requested: requested_schema_hash.to_string(),
                active: active_schema_hash,
            });
        }
    }

    if let Some(requested) = request.target_hash.as_deref() {
        return Some(ToolCallStaleReason::TargetHash {
            requested: requested.to_string(),
            active: static_target_hash(target).to_string(),
        });
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolCallStaleReason {
    SnapshotRevision { requested: i64, active: i64 },
    SchemaHash { requested: String, active: String },
    ToolVersion { requested: i64, active: i64 },
    TargetHash { requested: String, active: String },
    TargetRevision { requested: i64, active: i64 },
}

impl ToolCallStaleReason {
    const fn field_name(&self) -> &'static str {
        match self {
            Self::SnapshotRevision { .. } => "snapshotRevision",
            Self::SchemaHash { .. } => "schemaHash",
            Self::ToolVersion { .. } => "toolVersion",
            Self::TargetHash { .. } => "targetHash",
            Self::TargetRevision { .. } => "targetRevision",
        }
    }

    fn developer_message(&self) -> String {
        format!(
            "tool catalog {} is stale; refresh tools and retry",
            self.field_name()
        )
    }

    fn admin_message(&self) -> String {
        match self {
            Self::SnapshotRevision { requested, active } => format!(
                "requested snapshot revision {requested} does not match \
                 active static snapshot revision {active}"
            ),
            Self::SchemaHash { requested, active } => format!(
                "requested schema hash {requested} does not match active \
                 schema hash {active}"
            ),
            Self::ToolVersion { requested, active } => format!(
                "requested tool version {requested} does not match active \
                 tool version {active}"
            ),
            Self::TargetHash { requested, active } => format!(
                "requested target hash {requested} does not match active \
                 target hash {active}"
            ),
            Self::TargetRevision { requested, active } => format!(
                "requested target revision {requested} does not match active \
                 target revision {active}"
            ),
        }
    }

    fn error_message(&self) -> String {
        format!(
            "tool catalog {} is stale; refresh tools before retrying",
            self.field_name()
        )
    }
}

const fn static_target_hash(_target: &AgentToolTargetConfig) -> &'static str {
    "<absent>"
}

fn openapi_target_hash(target: &AgentToolTargetConfig, request: &ToolCallRequest) -> String {
    request
        .target_hash
        .clone()
        .unwrap_or_else(|| static_target_hash(target).to_string())
}

fn openapi_target_revision(request: &ToolCallRequest) -> Option<u64> {
    Some(
        request
            .target_revision
            .filter(|revision| *revision >= 0)
            .unwrap_or_default() as u64,
    )
}

fn openapi_service_slug(target: &AgentToolTargetConfig) -> String {
    target
        .service_slug
        .clone()
        .unwrap_or_else(|| target.tool_id.clone())
}

fn openapi_operation_id(target: &AgentToolTargetConfig) -> String {
    target
        .operation_id
        .clone()
        .unwrap_or_else(|| target.tool_id.clone())
}

fn openapi_operation_slug(target: &AgentToolTargetConfig) -> String {
    target
        .operation_slug
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| target.tool_id.clone())
}

fn requested_tool_operation_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tools_cfg: &AgentToolsConfig,
) -> serde_json::Value {
    match target.kind {
        AgentToolTargetKind::OpenApi => requested_openapi_operation_metadata(target, request),
        AgentToolTargetKind::McpSse => {
            requested_mcp_sse_operation_metadata(target, request, tools_cfg)
        }
        _ => serde_json::json!({}),
    }
}

fn requested_openapi_operation_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
) -> serde_json::Value {
    let schema_hash = schema_hash_for_value(&target.input_schema);
    let target_hash = openapi_target_hash(target, request);
    let target_revision = openapi_target_revision(request).unwrap_or_default();
    let estimated_cost_micros = target.rate_card.fixed_micros;
    let auth_revision = "0/static";
    let rate_card_revision = 0_i64;
    let arguments_hash = arguments_hash(&request.arguments);
    let service_slug = openapi_service_slug(target);
    let operation_id = openapi_operation_id(target);
    let operation_slug = openapi_operation_slug(target);

    serde_json::json!({
        "targetKind": "openapi",
        "serviceSlug": service_slug,
        "operationId": operation_id,
        "operationSlug": operation_slug,
        "targetHash": target_hash,
        "schemaHash": schema_hash,
        "targetRevision": target_revision,
        "authRevision": auth_revision,
        "rateCardRevision": rate_card_revision,
        "estimatedCostMicros": estimated_cost_micros,
        "argumentsHash": arguments_hash,
        "currency": target.rate_card.currency,
        "estimated_cost_micros": estimated_cost_micros,
        "schema_hash": schema_hash,
        "arguments_hash": arguments_hash,
        "gateway": {
            "targetKind": "openapi",
            "serviceSlug": service_slug,
            "operationId": operation_id,
            "operationSlug": operation_slug,
            "targetHash": target_hash,
            "schemaHash": schema_hash,
            "targetRevision": target_revision,
            "authRevision": auth_revision,
            "rateCardRevision": rate_card_revision,
        },
    })
}

fn requested_mcp_sse_operation_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tools_cfg: &AgentToolsConfig,
) -> serde_json::Value {
    let target_hash =
        crate::agent::tools::mcp_sse::target_hash::canonical_mcp_sse_target_hash(target, tools_cfg);
    let schema_hash = schema_hash_for_value(&target.input_schema);
    let arguments_hash = arguments_hash(&request.arguments);
    let auth_revision = "0/static";
    let target_revision = "0/static";
    let rate_card_revision = 0_i64;
    let estimated_cost_micros = target.rate_card.fixed_micros;

    serde_json::json!({
        "targetKind": "mcp-sse",
        "toolId": target.tool_id,
        "upstreamToolName": target.tool_id,
        "targetHash": target_hash,
        "schemaHash": schema_hash,
        "targetRevision": target_revision,
        "authRevision": auth_revision,
        "rateCardRevision": rate_card_revision,
        "estimatedCostMicros": estimated_cost_micros,
        "argumentsHash": arguments_hash,
        "currency": target.rate_card.currency,
        "estimated_cost_micros": estimated_cost_micros,
        "schema_hash": schema_hash,
        "arguments_hash": arguments_hash,
        "gateway": {
            "targetKind": "mcp-sse",
            "targetId": target.tool_id,
            "targetHash": target_hash,
            "schemaHash": schema_hash,
            "targetRevision": target_revision,
            "authRevision": auth_revision,
            "rateCardRevision": rate_card_revision,
        },
    })
}

fn enrich_target_gateway_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    response: &mut ToolCallResponse,
    tools_cfg: &AgentToolsConfig,
) {
    match target.kind {
        AgentToolTargetKind::OpenApi => {
            enrich_openapi_gateway_metadata(target, request, response);
        }
        AgentToolTargetKind::McpSse => {
            enrich_mcp_sse_gateway_metadata(target, request, response, tools_cfg);
        }
        _ => {}
    }
}

fn target_kind_for_metadata(kind: AgentToolTargetKind) -> &'static str {
    match kind {
        AgentToolTargetKind::Mock => "mock",
        AgentToolTargetKind::Http => "http",
        AgentToolTargetKind::McpHttp => "mcp-http",
        AgentToolTargetKind::McpStreamableHttp => "mcp-streamable-http",
        AgentToolTargetKind::McpSse => "mcp-sse",
        AgentToolTargetKind::OpenApi => "openapi",
    }
}

fn target_hash_for_gateway_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    tools_cfg: &AgentToolsConfig,
) -> String {
    match target.kind {
        AgentToolTargetKind::McpSse => {
            crate::agent::tools::mcp_sse::target_hash::canonical_mcp_sse_target_hash(
                target, tools_cfg,
            )
        }
        AgentToolTargetKind::OpenApi => openapi_target_hash(target, request),
        _ => request
            .target_hash
            .clone()
            .unwrap_or_else(|| static_target_hash(target).to_string()),
    }
}

fn enrich_openapi_gateway_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    response: &mut ToolCallResponse,
) {
    if target.kind != AgentToolTargetKind::OpenApi {
        return;
    }
    let Some(metadata) = response.gateway_metadata.as_mut() else {
        return;
    };

    if metadata.target_kind.is_empty() {
        metadata.target_kind = "openapi".to_string();
    }
    if metadata.target_hash.is_empty() {
        metadata.target_hash = openapi_target_hash(target, request);
    }
    if metadata.service_slug.is_none() {
        metadata.service_slug = Some(openapi_service_slug(target));
    }
    if metadata.operation_id.is_none() {
        metadata.operation_id = Some(openapi_operation_id(target));
    }
    if metadata.operation_slug.is_none() {
        metadata.operation_slug = Some(openapi_operation_slug(target));
    }
    if metadata.target_revision.is_none() {
        metadata.target_revision = openapi_target_revision(request);
    }
    if metadata.schema_hash.is_none() {
        metadata.schema_hash = Some(schema_hash_for_value(&target.input_schema));
    }
    if metadata.rate_card_revision.is_none() {
        metadata.rate_card_revision = Some(0);
    }
    if metadata.auth_revision.is_empty() {
        metadata.auth_revision = "0/static".to_string();
    }
}

fn enrich_mcp_sse_gateway_metadata(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    response: &mut ToolCallResponse,
    tools_cfg: &AgentToolsConfig,
) {
    let Some(metadata) = response.gateway_metadata.as_mut() else {
        return;
    };

    if metadata.target_kind.is_empty() {
        metadata.target_kind = "mcp-sse".to_string();
    }
    if metadata.target_id.is_empty() {
        metadata.target_id = target.tool_id.clone();
    }
    if metadata.target_hash.is_empty() {
        metadata.target_hash = target_hash_for_gateway_metadata(target, request, tools_cfg);
    }
    if metadata.auth_revision.is_empty() {
        metadata.auth_revision = "0/static".to_string();
    }
    metadata.sse_used = true;
    if metadata.target_revision.is_none() {
        metadata.target_revision = Some(0);
    }
    if metadata.schema_hash.is_none() {
        metadata.schema_hash = Some(schema_hash_for_value(&target.input_schema));
    }
    if metadata.rate_card_revision.is_none() {
        metadata.rate_card_revision = Some(0);
    }
}

fn stale_snapshot_response(
    request: &ToolCallRequest,
    tool_execution_id: String,
    active_snapshot_revision: i64,
    reason: ToolCallStaleReason,
) -> ToolCallEnvelope {
    ToolCallEnvelope {
        status: EnvelopeExecutionStatus::SnapshotStale,
        executed: false,
        tool_execution_id,
        tool_id: request.tool_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step_id.clone(),
        tool_call_id: request.tool_call_id.clone(),
        snapshot_revision: active_snapshot_revision,
        snapshot_source: "static".to_string(),
        policy_revision: 0,
        policy: ToolPolicyEnvelope {
            decision: "blocked".to_string(),
            reason: "snapshot_stale".to_string(),
            ..ToolPolicyEnvelope::default()
        },
        cost: ToolCallCost::default(),
        events: ToolEventIds::default(),
        agent_action: AgentAction::RefreshTools,
        visible_to_user: false,
        user_message: String::new(),
        developer_message: reason.developer_message(),
        admin_message: reason.admin_message(),
        output: serde_json::json!({}),
        error: Some(ToolErrorEnvelope {
            code: "snapshot_stale".to_string(),
            retryable: true,
            retry_after_ms: None,
            message: reason.error_message(),
        }),
        approval: None,
    }
}

fn envelope_from_response(
    request: &ToolCallRequest,
    response: &ToolCallResponse,
) -> ToolCallEnvelope {
    ToolCallEnvelope {
        status: match response.status {
            InternalExecutionStatus::Completed => EnvelopeExecutionStatus::Completed,
            InternalExecutionStatus::Denied => EnvelopeExecutionStatus::Denied,
            InternalExecutionStatus::Blocked => EnvelopeExecutionStatus::Blocked,
            InternalExecutionStatus::Failed => EnvelopeExecutionStatus::Failed,
            InternalExecutionStatus::Timeout => EnvelopeExecutionStatus::Timeout,
        },
        executed: tool_response_executed(response),
        tool_execution_id: response.tool_execution_id.clone(),
        tool_id: request.tool_id.clone(),
        run_id: request.run_id.clone(),
        step_id: request.step_id.clone(),
        tool_call_id: response.tool_call_id.clone(),
        snapshot_revision: 0,
        snapshot_source: "static".to_string(),
        policy_revision: 0,
        policy: ToolPolicyEnvelope {
            decision: if response.policy.decision == "allowed" {
                "allow".to_string()
            } else {
                response.policy.decision.clone()
            },
            reason: response.policy.reason.clone(),
            ..ToolPolicyEnvelope::default()
        },
        cost: ToolCallCost {
            stage: if response.billing.billable {
                CostStage::Settled
            } else {
                CostStage::Waived
            },
            estimated_micros: response.billing.cost_micros,
            reserved_micros: 0,
            actual_micros: response.billing.cost_micros,
            currency: response.billing.currency.clone(),
            billable: response.billing.billable,
            rate_card_revision: 0,
            charge_on_failure: false,
        },
        events: ToolEventIds {
            requested_event_id: response.events.started_event_id.clone(),
            policy_event_id: None,
            completed_event_id: response.events.completed_event_id.clone(),
        },
        agent_action: agent_action_for_response(response),
        visible_to_user: false,
        user_message: String::new(),
        developer_message: String::new(),
        admin_message: String::new(),
        output: response.output.clone(),
        error: response.error.as_ref().map(|error| ToolErrorEnvelope {
            code: error.code.clone(),
            retryable: error.retryable,
            retry_after_ms: None,
            message: error.message.clone(),
        }),
        approval: None,
    }
}

fn agent_action_for_response(response: &ToolCallResponse) -> AgentAction {
    match response.status {
        InternalExecutionStatus::Timeout | InternalExecutionStatus::Failed => {
            if response.error.as_ref().is_some_and(|error| error.retryable) {
                AgentAction::RetryAfter
            } else {
                AgentAction::None
            }
        }
        InternalExecutionStatus::Blocked | InternalExecutionStatus::Denied => AgentAction::Stop,
        InternalExecutionStatus::Completed => AgentAction::None,
    }
}

fn tool_response_executed(response: &ToolCallResponse) -> bool {
    if let Some(executed) = response
        .gateway_metadata
        .as_ref()
        .and_then(|metadata| metadata.executed)
    {
        return executed;
    }

    !response
        .gateway_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.blocked_before_dispatch)
        && response.status != InternalExecutionStatus::Blocked
        && response.status != InternalExecutionStatus::Denied
}

fn auth_context_from_extensions(extensions: &http::Extensions) -> Option<&AuthContext> {
    extensions.get::<AuthContext>().or_else(|| {
        extensions
            .get::<std::sync::Arc<RequestContext>>()
            .and_then(|req_ctx| req_ctx.auth_context.as_ref())
    })
}

fn agent_context_from_extensions(extensions: &http::Extensions) -> Option<&AgentContext> {
    extensions
        .get::<std::sync::Arc<RequestContext>>()
        .and_then(|req_ctx| req_ctx.agent_context.as_ref())
}

fn error_response(error: AgentToolsServiceError) -> Response {
    let status = match error {
        AgentToolsServiceError::Disabled => StatusCode::SERVICE_UNAVAILABLE,
        AgentToolsServiceError::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        AgentToolsServiceError::MissingRoute => StatusCode::NOT_FOUND,
        AgentToolsServiceError::MissingAuth => StatusCode::UNAUTHORIZED,
        AgentToolsServiceError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        AgentToolsServiceError::InvalidJson => StatusCode::BAD_REQUEST,
        AgentToolsServiceError::InvalidArguments => StatusCode::BAD_REQUEST,
        AgentToolsServiceError::ToolIdRequired => StatusCode::BAD_REQUEST,
        AgentToolsServiceError::ToolNotAllowed => StatusCode::FORBIDDEN,
        AgentToolsServiceError::ToolTargetUnavailable => StatusCode::BAD_GATEWAY,
        AgentToolsServiceError::ToolExecutionFailed => StatusCode::BAD_GATEWAY,
    };
    let body = serde_json::json!({
        "error": {
            "type": "agent_tools_error",
            "code": error.code(),
            "message": error.to_string(),
        }
    });
    json_response(status, &body).expect("JSON error response should build")
}

fn json_response<T: serde::Serialize>(
    status: StatusCode,
    value: &T,
) -> Result<Response, AgentToolsServiceError> {
    let body = serde_json::to_vec(value).map_err(|_| AgentToolsServiceError::InvalidJson)?;
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::new(Full::new(Bytes::from(body))))
        .map_err(|_| AgentToolsServiceError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use http::{Method, StatusCode};
    use http_body_util::{BodyExt, Full};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
        time::{Duration, sleep},
    };
    use tonic::{Request as GrpcRequest, Response as GrpcResponse, Status};
    use tower::Service;
    use uuid::Uuid;

    use super::{
        AgentAction, AgentToolsService, blocked_before_dispatch_response, envelope_from_response,
    };
    use crate::{
        agent::tools::{
            mcp_sse::test_support::{
                McpSseFixture, sse_json_rpc_response_for_request, test_mcp_sse_target,
            },
            mcp_streamable_http::{
                json_rpc::CLIENT_PROTOCOL_VERSION, test_support::StreamableFixture,
            },
            response::ToolExecutionStatus as EnvelopeExecutionStatus,
            types::{
                ToolBillingOverride, ToolCallRequest, ToolCallResponse, ToolCost,
                ToolExecutionErrorEnvelope, ToolExecutionEvents, ToolExecutionStatus,
                ToolGatewayMetadata, ToolPolicySummary,
            },
        },
        config::{
            Config,
            agent::{
                AgentToolAllowlistConfig, AgentToolEgressPolicyConfig, AgentToolRateCardConfig,
                AgentToolTargetConfig, AgentToolTargetKind, AgentToolsBudgetConfig,
                AgentToolsConfig,
            },
        },
        policy_proto::{
            AgentPolicyDecisionKind, EvaluateRequest, EvaluateResponse, ValidateAgentPolicyRequest,
            ValidateAgentPolicyResponse, X402InboundEvaluateRequest, X402InboundEvaluateResponse,
            policy_service_server::{PolicyService, PolicyServiceServer},
        },
        router::router_details::{AgentToolsRouteAction, RouteType},
        types::{
            body::Body, extensions::AuthContext, org::OrgId, request::Request, response::Response,
            secret::Secret, user::UserId,
        },
    };

    #[test]
    fn failed_dispatch_response_builds_error_envelope_and_keeps_executed_true() {
        let request = ToolCallRequest {
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Failed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({"error":{"message":"SSE parse failed"}}),
            error: Some(ToolExecutionErrorEnvelope {
                code: "mcp_sse_parse_error".to_string(),
                message: "SSE parse failed".to_string(),
                retryable: false,
            }),
            gateway_metadata: Some(ToolGatewayMetadata {
                execution_source: "gateway_executed".to_string(),
                target_kind: "mcp-streamable-http".to_string(),
                target_id: "docs.search".to_string(),
                target_hash: "sha256:test".to_string(),
                auth_revision: "0/static".to_string(),
                cache_hit: false,
                reinitialized: false,
                protocol_version: Some("2025-06-18".to_string()),
                sse_used: true,
                failure_class: Some("mcp_sse_parse_error".to_string()),
                blocked_before_dispatch: false,
                latency_ms: Some(12),
                ..ToolGatewayMetadata::default()
            }),
            billing: ToolBillingOverride {
                reason: "mcp_sse_parse_error".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "run-1:step-1:call-1:exec-1".to_string(),
            },
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "waived".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: ToolExecutionEvents::default(),
        };

        let envelope = envelope_from_response(&request, &response);

        assert_eq!(envelope.status, EnvelopeExecutionStatus::Failed);
        assert_eq!(envelope.error.as_ref().unwrap().code, "mcp_sse_parse_error");
        assert_eq!(envelope.executed, true);
        assert_eq!(envelope.cost.actual_micros, 0);
    }

    #[test]
    fn gateway_metadata_executed_false_overrides_failed_envelope_executed() {
        let request = ToolCallRequest {
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            tool_id: "billing.create_invoice".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Failed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: Some(ToolExecutionErrorEnvelope {
                code: "openapi_schema_invalid".to_string(),
                message: "OpenAPI request schema validation failed".to_string(),
                retryable: false,
            }),
            gateway_metadata: Some(ToolGatewayMetadata {
                execution_source: "gateway_executed".to_string(),
                target_kind: "openapi".to_string(),
                target_id: "billing.create_invoice".to_string(),
                target_hash: "sha256:test".to_string(),
                auth_revision: "0/static".to_string(),
                cache_hit: false,
                reinitialized: false,
                protocol_version: None,
                sse_used: false,
                failure_class: Some("openapi_schema_invalid".to_string()),
                blocked_before_dispatch: false,
                latency_ms: Some(0),
                executed: Some(false),
                failure_stage: Some("schema".to_string()),
                ..ToolGatewayMetadata::default()
            }),
            billing: ToolBillingOverride {
                reason: "schema_invalid".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "tool_execution:exec-1".to_string(),
            },
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "waived".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: ToolExecutionEvents::default(),
        };

        let envelope = envelope_from_response(&request, &response);

        assert_eq!(envelope.status, EnvelopeExecutionStatus::Failed);
        assert_eq!(envelope.executed, false);
        assert_eq!(
            envelope.error.as_ref().unwrap().code,
            "openapi_schema_invalid"
        );
    }

    #[test]
    fn pre_dispatch_blocked_response_builds_blocked_envelope_and_executed_false() {
        let request = ToolCallRequest {
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            tool_call_id: Some("call-1".to_string()),
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };
        let response =
            blocked_before_dispatch_response(&request, "budget_blocked", "exec-1".to_string());
        let envelope = envelope_from_response(&request, &response);

        assert_eq!(envelope.status, EnvelopeExecutionStatus::Blocked);
        assert_eq!(envelope.executed, false);
        assert_eq!(envelope.cost.actual_micros, 0);
        assert_eq!(response.billing.reason, "budget_blocked");
        assert_eq!(response.billing.billable, false);
    }

    #[test]
    fn envelope_from_response_maps_retryable_timeout_to_retry_action() {
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Timeout,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: Some(ToolExecutionErrorEnvelope {
                code: "mcp_sse_idle_timeout".to_string(),
                message: "timed out".to_string(),
                retryable: true,
            }),
            gateway_metadata: None,
            billing: ToolBillingOverride {
                reason: "timeout".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "tool_execution:exec-1".to_string(),
            },
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "waived".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: ToolExecutionEvents::default(),
        };

        let envelope = envelope_from_response(&request, &response);

        assert_eq!(envelope.status, EnvelopeExecutionStatus::Timeout);
        assert_eq!(envelope.agent_action, AgentAction::RetryAfter);
    }

    #[test]
    fn envelope_from_response_maps_blocked_to_stop_action() {
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };
        let response =
            blocked_before_dispatch_response(&request, "budget_blocked", "exec-1".to_string());

        let envelope = envelope_from_response(&request, &response);

        assert_eq!(envelope.status, EnvelopeExecutionStatus::Blocked);
        assert_eq!(envelope.agent_action, AgentAction::Stop);
    }

    #[tokio::test]
    async fn disabled_returns_agent_tools_disabled_503() {
        let app = crate::app::build_test_app(Config::default())
            .await
            .expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(list_request(auth_context(Uuid::new_v4())))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "agent_tools_disabled"
        );
    }

    #[tokio::test]
    async fn enabled_list_returns_visible_tools() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                name: "Echo".to_string(),
                description: "Echo input".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(list_request(auth_context(Uuid::new_v4())))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["snapshotRevision"], 0);
        assert_eq!(body["policyRevision"], 0);
        assert_eq!(body["snapshotSource"], "static");
        assert_eq!(body["tools"][0]["toolId"], "support.echo");
        assert_eq!(body["tools"][0]["frameworkToolName"], "support_echo");
        assert_eq!(body["tools"][0]["upstreamToolName"], "support.echo");
        assert_eq!(
            body["tools"][0]["availability"]["visibility"],
            "listed_available"
        );
        assert_eq!(body["tools"][0]["timeoutMs"], 8000);
    }

    #[tokio::test]
    async fn mock_tool_call_returns_completed_response() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                name: "Echo".to_string(),
                description: "Echo input".to_string(),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 25,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "tool_call_id": "call_1",
                    "tool_execution_id": "exec_existing",
                    "arguments": { "message": "hello" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "completed");
        assert_eq!(body["toolCallId"], "call_1");
        assert!(
            body["toolExecutionId"]
                .as_str()
                .is_some_and(|id| id.starts_with("exec_"))
        );
        assert_ne!(body["toolExecutionId"], "exec_existing");
        assert_eq!(body["toolId"], "support.echo");
        assert_eq!(body["runId"], serde_json::Value::Null);
        assert_eq!(body["stepId"], serde_json::Value::Null);
        assert_eq!(body["executed"], true);
        assert_eq!(body["agentAction"], "none");
        assert_eq!(body["output"]["tool_id"], "support.echo");
        assert_eq!(body["output"]["arguments"]["message"], "hello");
        assert_eq!(body["output"]["mocked"], true);
        assert!(body["output"]["observed_at"].is_string());
        assert_eq!(body["cost"]["actualMicros"], 25);
        assert_eq!(body["cost"]["currency"], "USD");
        assert_eq!(body["cost"]["stage"], "settled");
        assert_eq!(body["policy"]["decision"], "allow");
        assert_eq!(body["policy"]["reason"], "tool_allowed");
        assert!(
            body["events"]["requestedEventId"]
                .as_str()
                .is_some_and(|id| id.starts_with("evt_"))
        );
        assert!(
            body["events"]["completedEventId"]
                .as_str()
                .is_some_and(|id| id.starts_with("evt_"))
        );
    }

    #[tokio::test]
    async fn tool_call_cost_limit_returns_blocked_without_dispatch() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            budget: crate::config::agent::AgentToolsBudgetConfig {
                max_tool_call_cost_micros: Some(24),
            },
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 25,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "run_id": "run_1",
                    "step_id": "step_1",
                    "tool_call_id": "call_1",
                    "tool_id": "support.echo",
                    "arguments": { "message": "hello" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["cost"]["actualMicros"], 0);
        assert_eq!(body["cost"]["billable"], false);
        assert_eq!(body["policy"]["reason"], "budget_blocked");
        assert_eq!(body["error"]["code"], "budget_blocked");
        assert_eq!(body["output"]["mocked"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn call_with_stale_snapshot_returns_snapshot_stale() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "tool_call_id": "call_stale",
                    "snapshot_revision": 999,
                    "schema_hash": "invalid",
                    "arguments": { "message": 42 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "snapshot_stale");
        assert_eq!(body["executed"], false);
        assert_eq!(body["agentAction"], "refresh_tools");
        assert_eq!(body["toolId"], "support.echo");
        assert_eq!(body["toolCallId"], "call_stale");
        assert!(
            body["toolExecutionId"]
                .as_str()
                .is_some_and(|id| id.starts_with("exec_"))
        );
        assert_eq!(body["events"]["completedEventId"], serde_json::Value::Null);
        assert_eq!(body["error"]["code"], "snapshot_stale");
        assert!(
            body["developerMessage"]
                .as_str()
                .is_some_and(|message| message.contains("snapshotRevision"))
        );
    }

    #[tokio::test]
    async fn call_with_stale_schema_hash_returns_snapshot_stale_before_validation() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "tool_call_id": "call_schema_stale",
                    "schemaHash": "sha256:stale",
                    "arguments": { "message": 42 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "snapshot_stale");
        assert_eq!(body["executed"], false);
        assert_eq!(body["agentAction"], "refresh_tools");
        assert_eq!(body["error"]["code"], "snapshot_stale");
        assert!(
            body["developerMessage"]
                .as_str()
                .is_some_and(|message| message.contains("schemaHash"))
        );
        assert_eq!(body["events"]["requestedEventId"], serde_json::Value::Null);
        assert_eq!(body["events"]["completedEventId"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn call_with_stale_target_hash_returns_snapshot_stale_before_target() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "tool_call_id": "call_target_stale",
                    "targetHash": "sha256:target-stale",
                    "arguments": { "message": "hello" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "snapshot_stale");
        assert_eq!(body["executed"], false);
        assert_eq!(body["agentAction"], "refresh_tools");
        assert_eq!(body["error"]["code"], "snapshot_stale");
        assert!(
            body["developerMessage"]
                .as_str()
                .is_some_and(|message| message.contains("targetHash"))
        );
        assert_eq!(body["events"]["requestedEventId"], serde_json::Value::Null);
        assert_eq!(body["events"]["completedEventId"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn call_with_list_schema_hash_reaches_target() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);
        let auth = auth_context(Uuid::new_v4());

        let list_response = service
            .call(list_request(auth.clone()))
            .await
            .expect("agent tools list response");
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = response_json(list_response).await;
        let schema_hash = list_body["tools"][0]["schemaHash"]
            .as_str()
            .expect("schema hash");

        let call_response = service
            .call(call_request(
                auth,
                serde_json::json!({
                    "tool_id": "support.echo",
                    "tool_call_id": "call_current_schema",
                    "schemaHash": schema_hash,
                    "arguments": { "message": "hello" }
                }),
            ))
            .await
            .expect("agent tools call response");

        assert_eq!(call_response.status(), StatusCode::OK);
        let call_body = response_json(call_response).await;
        assert_eq!(call_body["status"], "completed");
        assert_eq!(call_body["agentAction"], "none");
        assert_eq!(call_body["error"], serde_json::Value::Null);
        assert_eq!(call_body["output"]["mocked"], true);
    }

    #[tokio::test]
    async fn call_generates_gateway_tool_execution_id() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "tool_execution_id": "client-supplied",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let tool_execution_id = body["toolExecutionId"]
            .as_str()
            .expect("gateway tool execution id");
        assert!(tool_execution_id.starts_with("exec_"));
        assert_ne!(tool_execution_id, "client-supplied");
    }

    #[tokio::test]
    async fn successful_call_returns_requested_and_completed_event_ids() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "run_id": "run_1",
                    "step_id": "step_1",
                    "tool_call_id": "call_1",
                    "tool_id": "support.echo",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let requested_event_id = body["events"]["requestedEventId"]
            .as_str()
            .expect("requested event id");
        let completed_event_id = body["events"]["completedEventId"]
            .as_str()
            .expect("completed event id");
        assert!(requested_event_id.starts_with("evt_"));
        assert!(completed_event_id.starts_with("evt_"));
        assert_ne!(requested_event_id, completed_event_id);
        assert_eq!(body["runId"], "run_1");
        assert_eq!(body["stepId"], "step_1");
        assert_eq!(body["toolCallId"], "call_1");
    }

    #[tokio::test]
    async fn successful_call_sends_completed_audit_log_with_billing_mirrors() {
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                name: "Echo".to_string(),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 25,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "run_id": "run_1",
                    "step_id": "step_1",
                    "tool_call_id": "call_1",
                    "tool_id": "support.echo",
                    "arguments": { "message": "hello" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let tool_execution_id = body["toolExecutionId"].as_str().expect("tool execution id");
        let completed_event_id = body["events"]["completedEventId"]
            .as_str()
            .expect("completed event id");

        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert!(requests.iter().all(|request| {
            request.method == "POST"
                && request.path == "/v1/log/agent-event"
                && request.header("authorization") == Some("Bearer test-key")
        }));
        let completed_payload = requests
            .iter()
            .map(|request| {
                serde_json::from_str::<serde_json::Value>(&request.body)
                    .expect("agent log body should be JSON")
            })
            .find(|payload| {
                payload["eventType"] == "tool.result.received"
                    && payload["eventId"] == completed_event_id
            })
            .expect("completed tool event log payload");
        let metadata: serde_json::Value =
            serde_json::from_str(completed_payload["metadata"].as_str().unwrap())
                .expect("metadata JSON");

        assert_eq!(metadata["billing"]["costType"], "tool");
        assert_eq!(metadata["billing"]["costSubtype"], "tool");
        assert_eq!(metadata["billing"]["status"], "settled");
        assert_eq!(metadata["billing"]["amountMicros"], 25);
        assert_eq!(metadata["billing"]["currency"], "USD");
        assert_eq!(metadata["billing"]["pricingSource"], "rate_card");
        assert_eq!(metadata["billing"]["dedupeKey"], tool_execution_id);
        assert_eq!(completed_payload["toolExecutionId"], tool_execution_id);
        assert_eq!(completed_payload["toolCostMicros"], 25);
        assert_eq!(completed_payload["toolCostCurrency"], "USD");
        assert_eq!(completed_payload["toolCostSource"], "rate_card");
        assert_eq!(completed_payload["billingCostType"], "tool");
        assert_eq!(completed_payload["billingCostSubtype"], "tool");
        assert_eq!(completed_payload["billingStatus"], "settled");
        assert_eq!(completed_payload["billingAmountMicros"], 25);
        assert_eq!(completed_payload["billingCurrency"], "USD");
        assert_eq!(completed_payload["billingBillable"], true);
        assert_eq!(completed_payload["billingDedupeKey"], tool_execution_id);
        assert_eq!(completed_payload["cost"], 0.000025);
    }

    #[test]
    fn mcp_sse_requested_metadata_is_safe_and_complete() {
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            name: "Search docs".to_string(),
            kind: AgentToolTargetKind::McpSse,
            url: Some("https://mcp.example.com/sse?token=secret".to_string()),
            rate_card: AgentToolRateCardConfig {
                currency: "USD".to_string(),
                fixed_micros: 1500,
            },
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            arguments: serde_json::json!({"query": "refund policy"}),
            ..ToolCallRequest::default()
        };

        let metadata = super::requested_tool_operation_metadata(
            &target,
            &request,
            &AgentToolsConfig::default(),
        );

        assert_eq!(metadata["targetKind"], "mcp-sse");
        assert_eq!(metadata["toolId"], "docs.search");
        assert_eq!(metadata["upstreamToolName"], "docs.search");
        assert_eq!(metadata["estimatedCostMicros"], 1500);
        assert_eq!(metadata["currency"], "USD");
        assert!(
            metadata["targetHash"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        assert!(
            metadata["schemaHash"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        assert!(
            metadata["argumentsHash"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        assert_eq!(metadata["gateway"]["targetKind"], "mcp-sse");
        assert_eq!(metadata["gateway"]["targetId"], "docs.search");
        assert!(!metadata.to_string().contains("refund policy"));
        assert!(!metadata.to_string().contains("secret"));
    }

    #[tokio::test]
    async fn openapi_policy_preflight_includes_safe_operation_metadata() {
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            decision: AgentPolicyDecisionKind::Allow as i32,
            allowed: true,
            reason: "allowed".to_string(),
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint.clone();
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            budget: AgentToolsBudgetConfig {
                max_tool_call_cost_micros: Some(1),
            },
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                method: "POST".to_string(),
                service_slug: Some("support-api".to_string()),
                operation_id: Some("getTicket".to_string()),
                operation_slug: Some("get_ticket".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "ticket_id": { "type": "string" },
                        "token": { "type": "string" }
                    }
                }),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await = Some(Arc::new(client));
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-openapi",
                    "run_id": "run-openapi",
                    "step_id": "step-openapi",
                    "tool_call_id": "call-openapi-policy",
                    "tool_id": "support.lookup_ticket",
                    "arguments": {
                        "ticket_id": "T-123",
                        "token": "secret-value"
                    }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let requests = policy.requests.lock().expect("policy request lock").clone();
        assert!(!requests.is_empty(), "expected policy preflight request");
        let metadata: serde_json::Value =
            serde_json::from_slice(&requests[0].metadata).expect("policy metadata JSON");

        assert_eq!(metadata["targetKind"], "openapi");
        assert_eq!(metadata["serviceSlug"], "support-api");
        assert_eq!(metadata["operationId"], "getTicket");
        assert_eq!(metadata["operationSlug"], "get_ticket");
        assert_eq!(metadata["targetHash"], "<absent>");
        assert!(
            metadata["schemaHash"]
                .as_str()
                .is_some_and(|value| { value.starts_with("sha256:") })
        );
        assert_eq!(metadata["targetRevision"], 0);
        assert_eq!(metadata["authRevision"], "0/static");
        assert_eq!(metadata["rateCardRevision"], 0);
        assert_eq!(metadata["estimatedCostMicros"], 4200);
        assert!(
            metadata["argumentsHash"]
                .as_str()
                .is_some_and(|value| { value.starts_with("sha256:") })
        );
        assert!(metadata.get("arguments").is_none());
        assert!(!metadata.to_string().contains("secret-value"));
    }

    #[tokio::test]
    async fn openapi_fallback_events_include_operation_and_terminal_billing_payload() {
        let upstream = spawn_json_http_fixture().await;
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                method: "POST".to_string(),
                url: Some(upstream.url),
                service_slug: Some("support-api".to_string()),
                operation_id: Some("getTicket".to_string()),
                operation_slug: Some("get_ticket".to_string()),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-openapi",
                    "run_id": "run-openapi",
                    "step_id": "step-openapi",
                    "tool_call_id": "call-openapi-fallback",
                    "tool_id": "support.lookup_ticket",
                    "arguments": { "ticket_id": "T-123" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let tool_execution_id = body["toolExecutionId"].as_str().expect("tool execution id");
        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert_eq!(requests.len(), 2);
        let payloads: Vec<serde_json::Value> = requests
            .iter()
            .map(|request| {
                serde_json::from_str(&request.body).expect("agent log body should be JSON")
            })
            .collect();
        let requested = payloads
            .iter()
            .find(|payload| payload["eventType"] == "tool.call.requested")
            .expect("requested event payload");
        let terminal = payloads
            .iter()
            .find(|payload| payload["eventType"] == "tool.result.received")
            .expect("terminal event payload");

        for payload in [requested, terminal] {
            assert!(
                payload["eventTime"]
                    .as_str()
                    .is_some_and(|value| { chrono::DateTime::parse_from_rfc3339(value).is_ok() })
            );
            assert!(
                payload["observedAt"]
                    .as_str()
                    .is_some_and(|value| { chrono::DateTime::parse_from_rfc3339(value).is_ok() })
            );
            assert_eq!(payload["toolExecutionId"], tool_execution_id);
            assert_eq!(payload["alephantAgentId"], "agent-openapi");
            assert_eq!(payload["alephantRunId"], "run-openapi");
            assert_eq!(payload["alephantStepId"], "step-openapi");
            assert_eq!(payload["targetKind"], "openapi");
            assert_eq!(payload["serviceSlug"], "support-api");
            assert_eq!(payload["operationId"], "getTicket");
        }
        assert_eq!(terminal["billingStatus"], "settled");
        assert_eq!(terminal["billingReason"], "openapi_2xx");
        assert_eq!(
            terminal["billingDedupeKey"],
            format!("tool_execution:{tool_execution_id}")
        );
    }

    #[tokio::test]
    async fn openapi_operation_slug_falls_back_to_tool_id_when_unset() {
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            decision: AgentPolicyDecisionKind::Allow as i32,
            allowed: true,
            reason: "allowed".to_string(),
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint.clone();
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            budget: AgentToolsBudgetConfig {
                max_tool_call_cost_micros: Some(1),
            },
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                method: "POST".to_string(),
                service_slug: Some("support-api".to_string()),
                operation_id: Some("getTicket".to_string()),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await = Some(Arc::new(client));
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-openapi",
                    "run_id": "run-openapi",
                    "step_id": "step-openapi",
                    "tool_call_id": "call-openapi-slug",
                    "tool_id": "support.lookup_ticket",
                    "arguments": { "ticket_id": "T-123" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let requests = policy.requests.lock().expect("policy request lock").clone();
        let metadata: serde_json::Value =
            serde_json::from_slice(&requests[0].metadata).expect("policy metadata JSON");

        assert_eq!(metadata["serviceSlug"], "support-api");
        assert_eq!(metadata["operationId"], "getTicket");
        assert_eq!(metadata["operationSlug"], "support.lookup_ticket");
    }

    #[tokio::test]
    async fn openapi_policy_deny_blocks_before_dispatch_and_logs_consistent_policy() {
        let upstream = spawn_json_http_fixture().await;
        let fixture = spawn_agent_log_http_fixture(202).await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            decision: AgentPolicyDecisionKind::Deny as i32,
            allowed: false,
            reason: "policy_denied_tool".to_string(),
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint.clone();
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                method: "POST".to_string(),
                url: Some(upstream.url.clone()),
                service_slug: Some("support-api".to_string()),
                operation_id: Some("getTicket".to_string()),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await = Some(Arc::new(client));
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-openapi",
                    "run_id": "run-openapi",
                    "step_id": "step-openapi",
                    "tool_call_id": "call-openapi-deny",
                    "tool_id": "support.lookup_ticket",
                    "arguments": { "ticket_id": "T-123" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "openapi_policy_blocked");
        assert_eq!(json_http_request_count(&upstream), 0);

        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert_eq!(requests.len(), 2);
        let terminal: serde_json::Value = requests
            .iter()
            .map(|request| {
                serde_json::from_str(&request.body).expect("agent log body should be JSON")
            })
            .find(|payload: &serde_json::Value| payload["eventType"] == "tool.result.received")
            .expect("terminal event payload");
        let metadata: serde_json::Value =
            serde_json::from_str(terminal["metadata"].as_str().unwrap()).expect("metadata JSON");

        assert_eq!(terminal["policyAllowed"], false);
        assert_eq!(terminal["policyDecision"], "denied");
        assert_eq!(terminal["billingStatus"], "waived");
        assert_eq!(terminal["billingReason"], "policy_blocked");
        assert_eq!(terminal["billingBillable"], false);
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["decision"], "denied");
        assert_eq!(metadata["policy"]["reason"], "policy_denied_tool");
        assert_eq!(metadata["gateway"]["targetKind"], "openapi");
        assert_eq!(metadata["gateway"]["serviceSlug"], "support-api");
        assert_eq!(metadata["gateway"]["operationId"], "getTicket");
    }

    #[tokio::test]
    async fn openapi_policy_unavailable_fails_closed_before_dispatch() {
        let upstream = spawn_json_http_fixture().await;
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                method: "POST".to_string(),
                url: Some(upstream.url.clone()),
                service_slug: Some("support-api".to_string()),
                operation_id: Some("getTicket".to_string()),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-openapi",
                    "run_id": "run-openapi",
                    "step_id": "step-openapi",
                    "tool_call_id": "call-openapi-unavailable",
                    "tool_id": "support.lookup_ticket",
                    "arguments": { "ticket_id": "T-123" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "openapi_policy_blocked");
        assert_eq!(json_http_request_count(&upstream), 0);

        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert_eq!(requests.len(), 2);
        let terminal: serde_json::Value = requests
            .iter()
            .map(|request| {
                serde_json::from_str(&request.body).expect("agent log body should be JSON")
            })
            .find(|payload: &serde_json::Value| payload["eventType"] == "tool.result.received")
            .expect("terminal event payload");

        assert_eq!(terminal["policyAllowed"], false);
        assert_eq!(terminal["policyDecision"], "blocked");
        assert_eq!(terminal["policyReason"], "policy_unavailable");
        assert_eq!(terminal["billingStatus"], "waived");
        assert_eq!(terminal["billingReason"], "policy_blocked");
    }

    #[tokio::test]
    async fn mcp_sse_policy_deny_blocks_before_dispatch() {
        let fixture = spawn_agent_log_http_fixture(202).await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            decision: AgentPolicyDecisionKind::Deny as i32,
            allowed: false,
            reason: "tool_policy_denied".to_string(),
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint.clone();
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                name: "Search docs".to_string(),
                kind: AgentToolTargetKind::McpSse,
                url: Some("https://mcp.example.com/sse".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 1500,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await = Some(Arc::new(client));
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-mcp-sse",
                    "run_id": "run-mcp-sse",
                    "step_id": "step-mcp-sse",
                    "tool_call_id": "call-mcp-sse-deny",
                    "tool_id": "docs.search",
                    "arguments": {"query": "refund policy"}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "tool_policy_denied");
        assert_eq!(body["policy"]["decision"], "denied");
        assert_eq!(body["policy"]["reason"], "tool_policy_denied");
        assert_eq!(body["cost"]["stage"], "waived");

        let requests = policy.requests.lock().expect("policy request lock").clone();
        assert_eq!(requests.len(), 1);
        let metadata: serde_json::Value =
            serde_json::from_slice(&requests[0].metadata).expect("policy metadata JSON");
        assert_eq!(metadata["targetKind"], "mcp-sse");
        assert_eq!(metadata["toolId"], "docs.search");
        assert_eq!(metadata["estimatedCostMicros"], 1500);

        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert_eq!(requests.len(), 2);
        let terminal: serde_json::Value = requests
            .iter()
            .map(|request| {
                serde_json::from_str(&request.body).expect("agent log body should be JSON")
            })
            .find(|payload: &serde_json::Value| payload["eventType"] == "tool.result.received")
            .expect("terminal event payload");
        let metadata: serde_json::Value =
            serde_json::from_str(terminal["metadata"].as_str().unwrap()).expect("metadata JSON");

        assert_eq!(terminal["policyAllowed"], false);
        assert_eq!(terminal["policyDecision"], "denied");
        assert_eq!(terminal["policyReason"], "tool_policy_denied");
        assert_eq!(terminal["billingStatus"], "waived");
        assert_eq!(terminal["billingReason"], "policy_blocked");
        assert_eq!(metadata["gateway"]["targetKind"], "mcp-sse");
        assert_eq!(metadata["gateway"]["targetId"], "docs.search");
        assert_eq!(metadata["gateway"]["blockedBeforeDispatch"], true);
        assert_eq!(metadata["gateway"]["sseUsed"], true);
    }

    #[tokio::test]
    async fn mcp_sse_policy_unavailable_fails_closed_before_dispatch() {
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                name: "Search docs".to_string(),
                kind: AgentToolTargetKind::McpSse,
                url: Some("https://mcp.example.com/sse".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 1500,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-mcp-sse",
                    "run_id": "run-mcp-sse",
                    "step_id": "step-mcp-sse",
                    "tool_call_id": "call-mcp-sse-unavailable",
                    "tool_id": "docs.search",
                    "arguments": {"query": "refund policy"}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "tool_policy_unavailable");
        assert_eq!(body["error"]["retryable"], true);
        assert_eq!(body["policy"]["decision"], "blocked");
        assert_eq!(body["policy"]["reason"], "policy_unavailable");
        assert_eq!(body["cost"]["stage"], "waived");

        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert_eq!(requests.len(), 2);
        let terminal: serde_json::Value = requests
            .iter()
            .map(|request| {
                serde_json::from_str(&request.body).expect("agent log body should be JSON")
            })
            .find(|payload: &serde_json::Value| payload["eventType"] == "tool.result.received")
            .expect("terminal event payload");

        assert_eq!(terminal["policyAllowed"], false);
        assert_eq!(terminal["policyDecision"], "blocked");
        assert_eq!(terminal["policyReason"], "policy_unavailable");
        assert_eq!(terminal["billingStatus"], "waived");
        assert_eq!(terminal["billingReason"], "policy_blocked");
    }

    #[tokio::test]
    async fn mcp_sse_tool_call_emits_requested_and_terminal_events() {
        let Some(mcp) = McpSseFixture::start(vec![
            sse_json_rpc_response_for_request(serde_json::json!({
                    "protocolVersion": CLIENT_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1"}
            })),
            sse_json_rpc_response_for_request(serde_json::json!({
                    "content": [{"type": "text", "text": "found docs"}],
                    "isError": false
            })),
        ]) else {
            return;
        };
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            targets: vec![test_mcp_sse_target(mcp.sse_url())],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-mcp-sse",
                    "run_id": "run-mcp-sse",
                    "step_id": "step-mcp-sse",
                    "tool_call_id": "call-1",
                    "tool_execution_id": "exec-1",
                    "tool_id": "docs.search",
                    "arguments": {"query": "refund policy"}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "completed");
        assert_eq!(body["executed"], true);
        assert_eq!(body["cost"]["stage"], "settled");
        assert_eq!(body["events"]["requestedEventId"].is_string(), true);
        assert_eq!(body["events"]["completedEventId"].is_string(), true);

        let requests = wait_for_agent_log_requests(&fixture, 2).await;
        assert_eq!(requests.len(), 2);
        let payloads: Vec<serde_json::Value> = requests
            .iter()
            .map(|request| {
                serde_json::from_str(&request.body).expect("agent log body should be JSON")
            })
            .collect();
        assert_eq!(payloads[0]["eventType"], "tool.call.requested");
        let terminal = payloads
            .iter()
            .find(|payload| payload["eventType"] == "tool.result.received")
            .expect("terminal event");
        let metadata: serde_json::Value =
            serde_json::from_str(terminal["metadata"].as_str().unwrap()).expect("metadata JSON");
        assert_eq!(metadata["gateway"]["targetKind"], "mcp-sse");
        assert_eq!(metadata["billing"]["billable"], true);
        assert_eq!(terminal["billingStatus"], "settled");
        assert_eq!(terminal["billingBillable"], true);
    }

    #[tokio::test]
    async fn mcp_sse_budget_blocked_does_not_dispatch_target() {
        let Some(mcp) = McpSseFixture::start(vec![
            sse_json_rpc_response_for_request(serde_json::json!({
                    "protocolVersion": CLIENT_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1"}
            })),
            sse_json_rpc_response_for_request(serde_json::json!({
                    "content": [{"type": "text", "text": "found docs"}],
                    "isError": false
            })),
        ]) else {
            return;
        };
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut target = test_mcp_sse_target(mcp.sse_url());
        target.rate_card.fixed_micros = 10_000;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            egress_policy: local_openapi_egress_policy(),
            budget: AgentToolsBudgetConfig {
                max_tool_call_cost_micros: Some(1_000),
            },
            targets: vec![target],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-mcp-sse",
                    "run_id": "run-mcp-sse",
                    "step_id": "step-mcp-sse",
                    "tool_call_id": "call-1",
                    "tool_execution_id": "exec-1",
                    "tool_id": "docs.search",
                    "arguments": {"query": "refund policy"}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["policy"]["reason"], "budget_blocked");
        assert_eq!(body["agentAction"], "stop");
        assert_eq!(mcp.requests().len(), 0);
    }

    #[tokio::test]
    async fn agent_tools_workspace_concurrency_limit_blocks_second_call() {
        let Some(mcp) = McpSseFixture::start_holding_call_response() else {
            return;
        };
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            max_concurrent_per_workspace: 1,
            egress_policy: local_openapi_egress_policy(),
            targets: vec![test_mcp_sse_target(mcp.sse_url())],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let service = AgentToolsService::new(app.state);
        let org_id = Uuid::new_v4();

        let mut first_service = service.clone();
        let first = tokio::spawn(async move {
            first_service
                .call(call_request(
                    auth_context(org_id),
                    serde_json::json!({
                        "agent_id": "agent-mcp-sse",
                        "run_id": "run-mcp-sse",
                        "step_id": "step-mcp-sse-1",
                        "tool_call_id": "call-1",
                        "tool_id": "docs.search",
                        "arguments": {"query": "refund policy"}
                    }),
                ))
                .await
                .expect("first agent tools response")
        });

        wait_for_mcp_sse_requests(&mcp, 3).await;

        let mut second_service = service;
        let second_response = second_service
            .call(call_request(
                auth_context(org_id),
                serde_json::json!({
                    "agent_id": "agent-mcp-sse",
                    "run_id": "run-mcp-sse",
                    "step_id": "step-mcp-sse-2",
                    "tool_call_id": "call-2",
                    "tool_id": "docs.search",
                    "arguments": {"query": "billing policy"}
                }),
            ))
            .await
            .expect("second agent tools response");

        assert_eq!(second_response.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = response_json(second_response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "agent_tools_concurrency_limited");
        assert_eq!(body["agentAction"], "stop");
        assert_eq!(
            body["output"]["metadata"]["gateway"]["failureClass"],
            "agent_tools_concurrency_limited"
        );
        assert_eq!(
            body["output"]["metadata"]["gateway"]["failureStage"],
            "concurrency"
        );
        assert_eq!(mcp.requests().len(), 3);

        mcp.release_held_call("found docs");
        let first_response = first.await.expect("first task joined");
        assert_eq!(first_response.status(), StatusCode::OK);
        let first_body = response_json(first_response).await;
        assert_eq!(first_body["status"], "completed");
    }

    #[tokio::test]
    async fn schema_validation_rejects_invalid_call_arguments() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "arguments": { "message": 42 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "invalid_arguments"
        );
    }

    #[tokio::test]
    async fn openapi_schema_invalid_returns_agent_envelope_with_terminal_event() {
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["ticket_id"],
                    "properties": {
                        "ticket_id": { "type": "string" }
                    }
                }),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.lookup_ticket",
                    "tool_call_id": "call-openapi-schema",
                    "arguments": { "ticket_id": 42 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "failed");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "openapi_schema_invalid");
        assert_eq!(body["cost"]["billable"], false);
        assert_eq!(body["cost"]["actualMicros"], 0);
        assert_eq!(body["cost"]["stage"], "waived");
        assert_eq!(
            body["output"]["metadata"]["billing"]["reason"],
            "schema_invalid"
        );
        assert_eq!(body["output"]["metadata"]["billing"]["billable"], false);
        assert_eq!(
            body["output"]["metadata"]["gateway"]["targetKind"],
            "openapi"
        );
        assert_eq!(body["output"]["metadata"]["gateway"]["executed"], false);
        assert_eq!(
            body["output"]["metadata"]["gateway"]["failureStage"],
            "schema"
        );
        let completed_event_id = body["events"]["completedEventId"]
            .as_str()
            .expect("completed event id");
        assert!(completed_event_id.starts_with("evt_"));

        let requests = wait_for_agent_log_requests(&fixture, 1).await;
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|request| {
            request.method == "POST"
                && request.path == "/v1/log/agent-event"
                && request.header("authorization") == Some("Bearer test-key")
        }));
        let completed_payload: serde_json::Value =
            serde_json::from_str(&requests[0].body).expect("agent log body should be JSON");
        assert_eq!(completed_payload["eventId"], completed_event_id);
        assert_eq!(completed_payload["eventType"], "tool.result.received");
        let metadata: serde_json::Value =
            serde_json::from_str(completed_payload["metadata"].as_str().unwrap())
                .expect("metadata JSON");
        assert_eq!(metadata["billing"]["reason"], "schema_invalid");
        assert_eq!(metadata["billing"]["billable"], false);
        assert_eq!(metadata["billing"]["status"], "waived");
        assert_eq!(metadata["gateway"]["targetKind"], "openapi");
        assert_eq!(metadata["gateway"]["executed"], false);
        assert_eq!(metadata["gateway"]["failureStage"], "schema");
    }

    #[tokio::test]
    async fn openapi_snapshot_stale_returns_terminal_event() {
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint = format!("{}/v1/log/agent-event", fixture.url);
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.lookup_ticket".to_string(),
                name: "Lookup ticket".to_string(),
                description: "Lookup support ticket".to_string(),
                kind: AgentToolTargetKind::OpenApi,
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "ticket_id": { "type": "string" }
                    }
                }),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 4200,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.lookup_ticket",
                    "tool_call_id": "call-openapi-stale",
                    "targetHash": "sha256:stale",
                    "arguments": { "ticket_id": "T-123" }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "failed");
        assert_eq!(body["executed"], false);
        assert_eq!(body["error"]["code"], "openapi_snapshot_stale");
        assert_eq!(
            body["output"]["metadata"]["billing"]["reason"],
            "snapshot_stale"
        );
        assert_eq!(
            body["output"]["metadata"]["gateway"]["failureStage"],
            "snapshot"
        );
        let completed_event_id = body["events"]["completedEventId"]
            .as_str()
            .expect("completed event id");
        assert!(completed_event_id.starts_with("evt_"));

        let requests = wait_for_agent_log_requests(&fixture, 1).await;
        assert_eq!(requests.len(), 1);
        let completed_payload: serde_json::Value =
            serde_json::from_str(&requests[0].body).expect("agent log body should be JSON");
        assert_eq!(completed_payload["eventId"], completed_event_id);
        assert_eq!(completed_payload["eventType"], "tool.result.received");
        let metadata: serde_json::Value =
            serde_json::from_str(completed_payload["metadata"].as_str().unwrap())
                .expect("metadata JSON");
        assert_eq!(metadata["billing"]["reason"], "snapshot_stale");
        assert_eq!(metadata["billing"]["billable"], false);
        assert_eq!(metadata["gateway"]["targetKind"], "openapi");
        assert_eq!(metadata["gateway"]["executed"], false);
        assert_eq!(metadata["gateway"]["failureStage"], "snapshot");
    }

    #[tokio::test]
    async fn mcp_http_call_invalid_arguments_returns_bad_request_before_target() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "mcp.search".to_string(),
                name: "MCP Search".to_string(),
                description: "Search MCP endpoint".to_string(),
                kind: AgentToolTargetKind::McpHttp,
                url: Some("https://mcp.example.com/mcp".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "mcp.search",
                    "arguments": { "query": 123 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "invalid_arguments"
        );
    }

    #[tokio::test]
    async fn mcp_http_call_with_stale_snapshot_returns_snapshot_stale_before_target() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "mcp.search".to_string(),
                name: "MCP Search".to_string(),
                description: "Search MCP endpoint".to_string(),
                kind: AgentToolTargetKind::McpHttp,
                url: Some("https://mcp.example.com/mcp".to_string()),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "mcp.search",
                    "tool_call_id": "call_stale",
                    "snapshot_revision": 999,
                    "arguments": { "query": 123 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "snapshot_stale");
        assert_eq!(body["executed"], false);
        assert_eq!(body["agentAction"], "refresh_tools");
        assert_eq!(body["toolId"], "mcp.search");
        assert_eq!(body["toolCallId"], "call_stale");
        assert_eq!(body["events"]["completedEventId"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn mcp_streamable_policy_denied_does_not_dispatch_target() {
        let Some(fixture) = StreamableFixture::start(Vec::new()) else {
            return;
        };
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                kind: AgentToolTargetKind::McpStreamableHttp,
                url: Some(fixture.url().to_string()),
                allowlist: AgentToolAllowlistConfig {
                    agent_ids: vec!["registered-agent".to_string()],
                    ..AgentToolAllowlistConfig::default()
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "spoofed-agent",
                    "tool_id": "docs.search",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "tool_not_allowed"
        );
        assert_eq!(fixture.requests().len(), 0);
    }

    #[tokio::test]
    async fn mcp_streamable_budget_blocked_does_not_dispatch_target() {
        let Some(fixture) = StreamableFixture::start(Vec::new()) else {
            return;
        };
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            budget: AgentToolsBudgetConfig {
                max_tool_call_cost_micros: Some(1),
            },
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                kind: AgentToolTargetKind::McpStreamableHttp,
                url: Some(fixture.url().to_string()),
                rate_card: AgentToolRateCardConfig {
                    fixed_micros: 10_000,
                    currency: "USD".to_string(),
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "registered-agent",
                    "tool_id": "docs.search",
                    "tool_call_id": "call-budget",
                    "run_id": "run-budget",
                    "step_id": "step-budget",
                    "arguments": {"query": "refund"}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "blocked");
        assert_eq!(body["executed"], false);
        assert_eq!(body["cost"]["actualMicros"], 0);
        assert_eq!(body["cost"]["billable"], false);
        assert_eq!(body["policy"]["reason"], "budget_blocked");
        assert_eq!(body["error"]["code"], "budget_blocked");
        assert_eq!(fixture.requests().len(), 0);
    }

    #[tokio::test]
    async fn mcp_streamable_tools_list_does_not_initialize_target() {
        let Some(fixture) = StreamableFixture::start(Vec::new()) else {
            return;
        };
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                kind: AgentToolTargetKind::McpStreamableHttp,
                url: Some(fixture.url().to_string()),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(list_request(auth_context(Uuid::new_v4())))
            .await
            .expect("agent tools list response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["tools"][0]["toolId"], "docs.search");
        assert_eq!(fixture.requests().len(), 0);
    }

    #[tokio::test]
    async fn mcp_streamable_tools_list_returns_framework_safe_descriptor() {
        let Some(fixture) = StreamableFixture::start(Vec::new()) else {
            return;
        };
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![
                AgentToolTargetConfig {
                    tool_id: "docs.search".to_string(),
                    name: "Search docs".to_string(),
                    description: "Search product docs".to_string(),
                    kind: AgentToolTargetKind::McpStreamableHttp,
                    url: Some(fixture.url().to_string()),
                    rate_card: AgentToolRateCardConfig {
                        fixed_micros: 10_000,
                        currency: "USD".to_string(),
                    },
                    ..AgentToolTargetConfig::default()
                },
                AgentToolTargetConfig {
                    tool_id: "docs_search".to_string(),
                    name: "Search docs duplicate".to_string(),
                    description: "Search duplicate docs".to_string(),
                    kind: AgentToolTargetKind::McpStreamableHttp,
                    url: Some(fixture.url().to_string()),
                    ..AgentToolTargetConfig::default()
                },
            ],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(list_request(auth_context(Uuid::new_v4())))
            .await
            .expect("agent tools list response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let first = &body["tools"][0];
        let second = &body["tools"][1];
        assert_eq!(first["toolId"], "docs.search");
        assert_eq!(first["frameworkToolName"], "docs_search");
        assert_eq!(first["name"], "Search docs");
        assert_eq!(first["description"], "Search product docs");
        assert_eq!(first["metadata"]["targetKind"], "mcp-streamable-http");
        assert_eq!(first["metadata"]["targetId"], "docs.search");
        assert!(first["inputSchema"].is_object());
        assert!(first["costPolicy"].is_object());
        assert_ne!(first["frameworkToolName"], second["frameworkToolName"]);
        assert!(
            second["frameworkToolName"]
                .as_str()
                .is_some_and(|name| { name.starts_with("docs_search_") && name.len() <= 64 })
        );
        assert!(!body.to_string().contains("Mcp-Session-Id"));
        assert_eq!(fixture.requests().len(), 0);
    }

    #[tokio::test]
    async fn call_json_parse_error_remains_distinct_from_schema_mismatch() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            schema_validation_enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let json_error_response = service
            .call(raw_call_request(
                auth_context(Uuid::new_v4()),
                Bytes::from_static(br#"{"tool_id":"support.echo","arguments":"#),
            ))
            .await
            .expect("agent tools response");
        let schema_error_response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "arguments": { "message": 42 }
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(json_error_response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(schema_error_response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(json_error_response).await["error"]["code"],
            "invalid_json"
        );
        assert_eq!(
            response_json(schema_error_response).await["error"]["code"],
            "invalid_arguments"
        );
    }

    #[tokio::test]
    async fn call_success_is_not_failed_by_audit_sink_error() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.echo",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["status"], "completed");
        assert!(
            body["events"]["completedEventId"]
                .as_str()
                .is_some_and(|id| id.starts_with("evt_"))
        );
    }

    #[tokio::test]
    async fn high_risk_call_fails_closed_when_requested_audit_sink_fails() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.event_log_http_fallback_enabled = false;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.high_risk".to_string(),
                risk_level: "high".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.high_risk",
                    "tool_execution_id": "exec_existing",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response_json(response).await;
        assert_eq!(body["status"], "failed");
        assert!(
            body["toolExecutionId"]
                .as_str()
                .is_some_and(|id| id.starts_with("exec_"))
        );
        assert_ne!(body["toolExecutionId"], "exec_existing");
        assert_eq!(body["toolCallId"], serde_json::Value::Null);
        assert_eq!(body["cost"]["actualMicros"], 0);
        assert_eq!(body["cost"]["stage"], "waived");
        assert_eq!(body["policy"]["decision"], "blocked");
        assert_eq!(body["policy"]["reason"], "audit_unavailable");
        assert!(
            body["events"]["requestedEventId"]
                .as_str()
                .is_some_and(|id| id.starts_with("evt_"))
        );
        assert_eq!(body["events"]["completedEventId"], serde_json::Value::Null);
        assert_eq!(body["output"]["error"]["code"], "audit_unavailable");
        assert_eq!(body["output"]["error"]["retryable"], true);
        assert!(
            body["output"]["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("audit sink unavailable"))
        );
    }

    #[test]
    fn fail_closed_risk_levels_include_high_and_critical() {
        assert!(super::is_fail_closed_risk_level("high"));
        assert!(super::is_fail_closed_risk_level("critical"));
        assert!(super::is_fail_closed_risk_level(" Critical "));
        assert!(!super::is_fail_closed_risk_level("medium"));
        assert!(!super::is_fail_closed_risk_level("low"));
    }

    #[tokio::test]
    async fn unlisted_tool_call_returns_tool_not_allowed_403() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "support.hidden",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "tool_not_allowed"
        );
    }

    #[tokio::test]
    async fn empty_tool_id_call_returns_tool_id_required_400() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "tool_id": "",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "tool_id_required"
        );
    }

    #[tokio::test]
    async fn spoofed_agent_id_does_not_bypass_agent_allowlist() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.tools = AgentToolsConfig {
            enabled: true,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.agent_only".to_string(),
                allowlist: AgentToolAllowlistConfig {
                    agent_ids: vec!["agent-allowed".to_string()],
                    ..AgentToolAllowlistConfig::default()
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentToolsService::new(app.state);

        let response = service
            .call(call_request(
                auth_context(Uuid::new_v4()),
                serde_json::json!({
                    "agent_id": "agent-allowed",
                    "tool_id": "support.agent_only",
                    "arguments": {}
                }),
            ))
            .await
            .expect("agent tools response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "tool_not_allowed"
        );
    }

    fn list_request(auth_context: AuthContext) -> Request {
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/agent/tools/list")
            .body(Body::new(Full::new(Bytes::from_static(b"{}"))))
            .expect("agent tools list request");
        request.extensions_mut().insert(auth_context);
        request.extensions_mut().insert(RouteType::AgentTools {
            action: AgentToolsRouteAction::List,
        });
        request
    }

    fn call_request(auth_context: AuthContext, body: serde_json::Value) -> Request {
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/agent/tools/call")
            .body(Body::new(Full::new(Bytes::from(
                serde_json::to_vec(&body).expect("request body"),
            ))))
            .expect("agent tools call request");
        request.extensions_mut().insert(auth_context);
        request.extensions_mut().insert(RouteType::AgentTools {
            action: AgentToolsRouteAction::Call,
        });
        request
    }

    fn raw_call_request(auth_context: AuthContext, body: Bytes) -> Request {
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/agent/tools/call")
            .body(Body::new(Full::new(body)))
            .expect("agent tools raw call request");
        request.extensions_mut().insert(auth_context);
        request.extensions_mut().insert(RouteType::AgentTools {
            action: AgentToolsRouteAction::Call,
        });
        request
    }

    fn auth_context(org_id: Uuid) -> AuthContext {
        AuthContext {
            api_key: Secret::from("test-key".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(org_id),
            workspace_type: None,
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: "vk_test".to_string(),
            master_key_id: None,
            master_key_base_url: None,
            department_id: Uuid::nil(),
            entity_type: "member".to_string(),
            entity_id: Uuid::new_v4(),
            entity_name: "test-user".to_string(),
            registered_agent_name: None,
            body_ttl_days: 30,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }

    struct AgentLogHttpFixture {
        url: String,
        requests: Arc<Mutex<Vec<AgentLogHttpRequest>>>,
        _shutdown: oneshot::Sender<()>,
    }

    #[derive(Clone)]
    struct AgentLogHttpRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl AgentLogHttpRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    async fn spawn_agent_log_http_fixture(status_code: u16) -> AgentLogHttpFixture {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind agent log HTTP");
        let addr = listener.local_addr().expect("agent log HTTP addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let server_requests = requests.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.expect("agent log HTTP accept");
                        let requests = server_requests.clone();
                        tokio::spawn(handle_agent_log_http_connection(
                            stream,
                            requests,
                            status_code,
                        ));
                    }
                }
            }
        });
        AgentLogHttpFixture {
            url: format!("http://{addr}"),
            requests,
            _shutdown: shutdown_tx,
        }
    }

    async fn handle_agent_log_http_connection(
        mut stream: TcpStream,
        requests: Arc<Mutex<Vec<AgentLogHttpRequest>>>,
        status_code: u16,
    ) {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            buffer.extend_from_slice(&chunk[..read]);
            let Some(request) = parse_agent_log_http_request(&buffer) else {
                continue;
            };
            requests
                .lock()
                .expect("agent log HTTP request lock")
                .push(request);
            let reason = if (200..300).contains(&status_code) {
                "Accepted"
            } else {
                "Internal Server Error"
            };
            let response = format!("HTTP/1.1 {status_code} {reason}\r\nContent-Length: 0\r\n\r\n");
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    }

    fn parse_agent_log_http_request(buffer: &[u8]) -> Option<AgentLogHttpRequest> {
        let text = std::str::from_utf8(buffer).ok()?;
        let header_end = text.find("\r\n\r\n")?;
        let (head, body_with_separator) = text.split_at(header_end);
        let mut lines = head.split("\r\n");
        let request_line = lines.next()?;
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next()?.to_string();
        let path = request_parts.next()?.to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        let content_length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = header_end + "\r\n\r\n".len();
        if buffer.len() < body_start + content_length {
            return None;
        }
        let body = body_with_separator["\r\n\r\n".len()..]
            .get(..content_length)?
            .to_string();

        Some(AgentLogHttpRequest {
            method,
            path,
            headers,
            body,
        })
    }

    async fn wait_for_agent_log_requests(
        fixture: &AgentLogHttpFixture,
        expected_len: usize,
    ) -> Vec<AgentLogHttpRequest> {
        for _ in 0..50 {
            let requests = fixture
                .requests
                .lock()
                .expect("agent log HTTP request lock")
                .clone();
            if requests.len() >= expected_len {
                return requests;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        fixture
            .requests
            .lock()
            .expect("agent log HTTP request lock")
            .clone()
    }

    async fn wait_for_mcp_sse_requests(
        fixture: &McpSseFixture,
        expected_len: usize,
    ) -> Vec<crate::agent::tools::mcp_sse::test_support::RecordedMcpSseRequest> {
        for _ in 0..100 {
            let requests = fixture.requests();
            if requests.len() >= expected_len {
                return requests;
            }
            sleep(Duration::from_millis(10)).await;
        }
        fixture.requests()
    }

    struct JsonHttpFixture {
        url: String,
        requests: Arc<Mutex<usize>>,
        _shutdown: oneshot::Sender<()>,
    }

    async fn spawn_json_http_fixture() -> JsonHttpFixture {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind JSON HTTP");
        let addr = listener.local_addr().expect("JSON HTTP addr");
        let requests = Arc::new(Mutex::new(0_usize));
        let server_requests = requests.clone();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.expect("JSON HTTP accept");
                        let requests = server_requests.clone();
                        tokio::spawn(async move {
                            *requests.lock().expect("JSON HTTP request lock") += 1;
                            let mut buffer = [0_u8; 1024];
                            let _ = stream.read(&mut buffer).await;
                            let body = r#"{"ok":true}"#;
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                }
            }
        });
        JsonHttpFixture {
            url: format!("http://{addr}/tickets"),
            requests,
            _shutdown: shutdown_tx,
        }
    }

    fn json_http_request_count(fixture: &JsonHttpFixture) -> usize {
        *fixture.requests.lock().expect("JSON HTTP request lock")
    }

    struct PolicyFixture {
        endpoint: String,
        requests: Arc<Mutex<Vec<ValidateAgentPolicyRequest>>>,
    }

    async fn spawn_policy_service(response: ValidateAgentPolicyResponse) -> PolicyFixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind policy");
        let addr = listener.local_addr().expect("policy addr");
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let service = TestPolicyService {
            response,
            requests: requests.clone(),
        };
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(PolicyServiceServer::new(service))
                .serve_with_incoming(incoming)
                .await
                .expect("policy server");
        });
        let endpoint = format!("http://{addr}");
        for _ in 0..50 {
            if crate::content_filter::ContentFilterGrpcClient::connect(endpoint.clone())
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        PolicyFixture { endpoint, requests }
    }

    #[derive(Clone)]
    struct TestPolicyService {
        response: ValidateAgentPolicyResponse,
        requests: Arc<Mutex<Vec<ValidateAgentPolicyRequest>>>,
    }

    #[tonic::async_trait]
    impl PolicyService for TestPolicyService {
        async fn evaluate(
            &self,
            _request: GrpcRequest<EvaluateRequest>,
        ) -> Result<GrpcResponse<EvaluateResponse>, Status> {
            Ok(GrpcResponse::new(EvaluateResponse {
                allowed: true,
                reason: "allowed".to_string(),
                ..Default::default()
            }))
        }

        async fn evaluate_x402_inbound(
            &self,
            _request: GrpcRequest<X402InboundEvaluateRequest>,
        ) -> Result<GrpcResponse<X402InboundEvaluateResponse>, Status> {
            Ok(GrpcResponse::new(X402InboundEvaluateResponse {
                allowed: true,
                reason: "x402_endpoint_ok".to_string(),
                ..Default::default()
            }))
        }

        async fn validate_agent_policy(
            &self,
            request: GrpcRequest<ValidateAgentPolicyRequest>,
        ) -> Result<GrpcResponse<ValidateAgentPolicyResponse>, Status> {
            self.requests
                .lock()
                .expect("policy request lock")
                .push(request.into_inner());
            Ok(GrpcResponse::new(self.response.clone()))
        }
    }

    fn local_openapi_egress_policy() -> AgentToolEgressPolicyConfig {
        AgentToolEgressPolicyConfig {
            https_only: false,
            block_loopback: false,
            block_link_local: false,
            block_metadata_ip: false,
            block_private_network: false,
            allow_environment_proxy: false,
        }
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes();
        serde_json::from_slice(&body).expect("response body should be JSON")
    }
}
