use std::{
    convert::Infallible,
    task::{Context, Poll},
};

use bytes::Bytes;
use chrono::Utc;
use http::{Method, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use tower::Service;
use uuid::Uuid;

use crate::{
    agent::{
        adapter::adapt_agent_event,
        context::{
            AgentConfidence, AgentContext, AgentEventPhase, AgentPolicyMode,
            AgentPolicyStage, AgentStepSource, AgentTrustLevel,
        },
        event::{
            AgentEventEnvelope, AgentEventInput, AgentEventSource,
            AgentEventsRequest, AgentEventsResponse, AgentPolicyDecision,
        },
        headers::parse_agent_context_from_headers,
        name::resolve_agent_name,
        policy::{
            AgentPolicyError, attach_policy_decision_to_metadata,
            compact_policy_decision_metadata, skipped_audit_decision,
            validate_agent_policy,
        },
        redaction::redact_metadata,
        sink::emit_agent_event,
        step_state::{
            StepConflictDecision, StepFingerprintInput, detect_step_conflict,
            step_fingerprint, step_state_key,
        },
    },
    app_state::AppState,
    config::agent::{AgentConflictAction, AgentMetadataRedaction},
    types::{
        body::Body,
        extensions::{AuthContext, RequestContext},
        request::Request,
        response::Response,
    },
};

#[derive(Debug, Clone)]
pub struct AgentEventsService {
    app_state: AppState,
}

impl AgentEventsService {
    #[must_use]
    pub const fn new(app_state: AppState) -> Self {
        Self { app_state }
    }
}

impl Service<Request> for AgentEventsService {
    type Response = Response;
    type Error = Infallible;
    type Future = futures::future::BoxFuture<
        'static,
        Result<Self::Response, Self::Error>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let app_state = self.app_state.clone();
        Box::pin(async move {
            Ok(handle_agent_events(app_state, req)
                .await
                .unwrap_or_else(error_response))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentServiceError {
    #[error("agent gateway is disabled")]
    Disabled,
    #[error("agent event route only supports POST")]
    MethodNotAllowed,
    #[error("agent event batch is too large")]
    BatchTooLarge,
    #[error("agent event payload is too large")]
    PayloadTooLarge,
    #[error("agent event metadata is too large")]
    MetadataTooLarge,
    #[error("agent event payload is invalid")]
    InvalidJson,
    #[error("agent event authentication context is missing")]
    MissingAuth,
    #[error("agent event sink failed")]
    SinkFailed,
    #[error("agent step conflict")]
    StepConflict,
    #[error("agent context conflict")]
    ContextConflict,
    #[error("agent policy unavailable")]
    PolicyUnavailable,
}

impl AgentServiceError {
    const fn code(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::BatchTooLarge => "batch_too_large",
            Self::PayloadTooLarge => "payload_too_large",
            Self::MetadataTooLarge => "metadata_too_large",
            Self::InvalidJson => "invalid_json",
            Self::MissingAuth => "missing_auth",
            Self::SinkFailed => "sink_failed",
            Self::StepConflict => "step_conflict",
            Self::ContextConflict => "context_conflict",
            Self::PolicyUnavailable => "policy_unavailable",
        }
    }
}

pub fn validate_event_limits(
    event_count: usize,
    payload_bytes: usize,
    max_batch_events: usize,
    max_event_bytes: usize,
) -> Result<(), AgentServiceError> {
    if event_count > max_batch_events {
        return Err(AgentServiceError::BatchTooLarge);
    }
    if payload_bytes > max_event_bytes {
        return Err(AgentServiceError::PayloadTooLarge);
    }
    Ok(())
}

fn validate_metadata_limit(
    metadata: &serde_json::Value,
    max_metadata_bytes: usize,
) -> Result<(), AgentServiceError> {
    let metadata_bytes = serde_json::to_vec(metadata)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    if metadata_bytes > max_metadata_bytes {
        return Err(AgentServiceError::MetadataTooLarge);
    }
    Ok(())
}

fn should_validate_policy_for_event(envelope: &AgentEventEnvelope) -> bool {
    matches!(envelope.event_phase, AgentEventPhase::Before)
        && matches!(envelope.policy_stage, AgentPolicyStage::PreAction)
        && matches!(
            envelope.step_confidence,
            AgentConfidence::Medium | AgentConfidence::High
        )
}

fn attach_policy_unavailable_audit(envelope: &mut AgentEventEnvelope) {
    envelope.event_type = "policy_unavailable".to_string();
    if !envelope.metadata.is_object() {
        let original = std::mem::take(&mut envelope.metadata);
        envelope.metadata = serde_json::json!({ "value": original });
    }
    let obj = envelope
        .metadata
        .as_object_mut()
        .expect("metadata should be object");
    preserve_original_field(obj, "policy", "policy_original");
    preserve_original_field(obj, "status", "status_original");
    preserve_original_field(obj, "severity", "severity_original");
    obj.insert("status".to_string(), serde_json::json!("failed"));
    obj.insert("severity".to_string(), serde_json::json!("error"));
    obj.insert(
        "policy".to_string(),
        serde_json::json!({
            "allowed": null,
            "policyDecision": "unavailable",
            "reason": "policy_unavailable",
            "blocked_by": "agent.policy.unavailable",
            "policy_id": "",
            "policy_scope": "AGENT_POLICY_SCOPE_NONE",
            "snapshot_revision": 0
        }),
    );
}

fn preserve_original_field(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    source_key: &str,
    original_key: &str,
) {
    if let Some(existing) = obj.remove(source_key) {
        let key = available_original_key(obj, original_key);
        obj.insert(key, existing);
    }
}

fn available_original_key(
    obj: &serde_json::Map<String, serde_json::Value>,
    original_key: &str,
) -> String {
    if !obj.contains_key(original_key) {
        return original_key.to_string();
    }

    let mut index = 1_u32;
    loop {
        let key = format!("{original_key}_{index}");
        if !obj.contains_key(&key) {
            return key;
        }
        index += 1;
    }
}

fn compact_policy_unavailable_audit() -> serde_json::Value {
    serde_json::json!({
        "status": "failed",
        "severity": "error",
        "metadata_truncated": true,
        "metadata_truncation_reason": "agent_policy_unavailable_metadata_limit",
        "policy": {
            "allowed": null,
            "policyDecision": "unavailable",
            "reason": "policy_unavailable",
            "blocked_by": "agent.policy.unavailable",
            "policy_id": "",
            "policy_scope": "AGENT_POLICY_SCOPE_NONE",
            "snapshot_revision": 0
        }
    })
}

async fn handle_agent_events(
    app_state: AppState,
    req: Request,
) -> Result<Response, AgentServiceError> {
    let cfg = &app_state.config().agent;
    if !cfg.enabled {
        return Err(AgentServiceError::Disabled);
    }
    if req.method() != Method::POST {
        return Err(AgentServiceError::MethodNotAllowed);
    }
    let header_agent_context = if cfg.allow_header_context {
        parse_agent_context_from_headers(
            req.headers(),
            cfg.max_header_value_bytes,
        )
    } else {
        None
    };

    let (parts, body) = req.into_parts();
    let auth_ctx = auth_context_from_extensions(&parts.extensions)
        .ok_or(AgentServiceError::MissingAuth)?;

    let body = Limited::new(body, cfg.max_event_bytes)
        .collect()
        .await
        .map_err(|_| AgentServiceError::PayloadTooLarge)?
        .to_bytes();
    let request: AgentEventsRequest = serde_json::from_slice(&body)
        .map_err(|_| AgentServiceError::InvalidJson)?;
    let events = request.into_sourced_events();
    validate_event_limits(
        events.len(),
        body.len(),
        cfg.max_batch_events,
        cfg.max_event_bytes,
    )?;

    let mut accepted = 0;
    let mut decisions = Vec::with_capacity(events.len());
    for sourced_event in events {
        let event =
            adapt_agent_event(sourced_event.source, sourced_event.event);
        validate_metadata_limit(&event.metadata, cfg.max_metadata_bytes)?;
        let context_conflict = check_context_conflict(
            header_agent_context.as_ref(),
            &event,
            cfg.context_conflict_action,
        )?;
        let mut envelope = normalize_event(
            &event,
            auth_ctx,
            cfg.metadata_redaction,
            header_agent_context.as_ref(),
            cfg.policy_mode,
        );
        envelope.context_conflict = context_conflict;
        let step_conflict =
            check_step_conflict(&app_state, auth_ctx, &envelope).await?;
        envelope.step_id_conflict = matches!(
            step_conflict,
            StepConflictDecision::ConflictWarn
                | StepConflictDecision::ConflictStrict
        );
        if matches!(step_conflict, StepConflictDecision::ConflictStrict) {
            return Err(AgentServiceError::StepConflict);
        }

        let decision = if should_validate_policy_for_event(&envelope) {
            match validate_agent_policy(&app_state, auth_ctx, &envelope).await {
                Ok(decision) => decision,
                Err(AgentPolicyError::Unavailable) => {
                    attach_policy_unavailable_audit(&mut envelope);
                    attach_sink_status_to_metadata(
                        &mut envelope.metadata,
                        "sent",
                    );
                    if validate_metadata_limit(
                        &envelope.metadata,
                        cfg.max_metadata_bytes,
                    )
                    .is_err()
                    {
                        envelope.metadata = compact_policy_unavailable_audit();
                        attach_sink_status_to_metadata(
                            &mut envelope.metadata,
                            "sent",
                        );
                        validate_metadata_limit(
                            &envelope.metadata,
                            cfg.max_metadata_bytes,
                        )?;
                    }
                    emit_agent_event(&app_state, auth_ctx, &envelope)
                        .await
                        .map_err(|err| {
                            tracing::error!(
                                error = %err,
                                event_id = %envelope.event_id,
                                event_type = %envelope.event_type,
                                workspace_id = %envelope.workspace_id,
                                "failed to emit agent event"
                            );
                            AgentServiceError::SinkFailed
                        })?;
                    return Err(AgentServiceError::PolicyUnavailable);
                }
            }
        } else {
            skipped_audit_decision(&envelope)
        };
        let decision = with_sink_status(decision, "sent");
        attach_policy_decision_to_metadata(&mut envelope.metadata, &decision);
        attach_sink_status_to_metadata(
            &mut envelope.metadata,
            &decision.sink_status,
        );
        if let Err(err) =
            validate_metadata_limit(&envelope.metadata, cfg.max_metadata_bytes)
        {
            if decision.allowed {
                return Err(err);
            }
            envelope.metadata = compact_policy_decision_metadata(&decision);
            attach_sink_status_to_metadata(
                &mut envelope.metadata,
                &decision.sink_status,
            );
            validate_metadata_limit(
                &envelope.metadata,
                cfg.max_metadata_bytes,
            )?;
        }

        emit_agent_event(&app_state, auth_ctx, &envelope)
            .await
            .map_err(|err| {
                tracing::error!(
                    error = %err,
                    event_id = %envelope.event_id,
                    event_type = %envelope.event_type,
                    workspace_id = %envelope.workspace_id,
                    "failed to emit agent event"
                );
                AgentServiceError::SinkFailed
            })?;
        accepted += 1;
        decisions.push(decision);
    }

    let allowed = decisions.iter().all(|decision| decision.allowed);
    json_response(
        StatusCode::ACCEPTED,
        &AgentEventsResponse {
            accepted,
            rejected: 0,
            allowed,
            decisions,
        },
    )
}

fn with_sink_status(
    mut decision: AgentPolicyDecision,
    sink_status: &'static str,
) -> AgentPolicyDecision {
    decision.sink_status = sink_status.to_string();
    decision
}

fn attach_sink_status_to_metadata(
    metadata: &mut serde_json::Value,
    sink_status: &str,
) {
    if !metadata.is_object() {
        let original = std::mem::take(metadata);
        *metadata = serde_json::json!({ "value": original });
    }
    let obj = metadata
        .as_object_mut()
        .expect("metadata should be object after wrapping");
    obj.insert("sinkStatus".to_string(), serde_json::json!(sink_status));
}

fn auth_context_from_extensions(
    extensions: &http::Extensions,
) -> Option<&AuthContext> {
    extensions.get::<AuthContext>().or_else(|| {
        extensions
            .get::<std::sync::Arc<RequestContext>>()
            .and_then(|req_ctx| req_ctx.auth_context.as_ref())
    })
}

fn normalize_event(
    event: &AgentEventInput,
    auth_ctx: &AuthContext,
    redaction: AgentMetadataRedaction,
    header_context: Option<&AgentContext>,
    policy_mode: AgentPolicyMode,
) -> AgentEventEnvelope {
    let mut metadata = if matches!(redaction, AgentMetadataRedaction::Basic) {
        redact_metadata(event.metadata.clone())
    } else {
        event.metadata.clone()
    };
    let resolved_agent_name = resolve_agent_name(
        auth_ctx.registered_agent_name.as_deref(),
        event.agent_name.as_deref(),
        header_context.and_then(|ctx| ctx.agent_name.as_deref()),
    );
    if let Some(conflict) = resolved_agent_name.conflict.as_ref() {
        attach_agent_name_conflict_metadata(&mut metadata, conflict);
    }

    AgentEventEnvelope {
        version: event.version.clone(),
        event_id: event
            .event_id
            .clone()
            .unwrap_or_else(|| format!("evt_{}", Uuid::now_v7())),
        event_type: event.event_type.clone(),
        event_source: event.source.unwrap_or(AgentEventSource::Unknown),
        event_phase: event.event_phase,
        policy_stage: event.policy_stage,
        policy_mode,
        event_source_trust: event.event_source_trust,
        sequence: event.sequence,
        observed_at: Utc::now(),
        timestamp: event.timestamp,
        name: event.name.clone(),
        alephant_agent_name: resolved_agent_name.name,
        alephant_agent_name_source: resolved_agent_name
            .source
            .map(str::to_string),
        alephant_agent_trust_level: resolved_agent_name
            .trust_level
            .map(|trust_level| trust_level.as_str().to_string()),
        workspace_id: auth_ctx.org_id.to_string(),
        virtual_key_id: auth_ctx.virtual_key_id,
        agent_id_external: event.agent_id_external.clone(),
        agent_uid: None,
        run_id: event.run_id.clone(),
        step_id: event.step_id.clone(),
        parent_step_id: event.parent_step_id.clone(),
        tool_call_id: event.tool_call_id.clone(),
        handoff_id: event.handoff_id.clone(),
        graph_node: event.graph_node.clone(),
        step_kind: event.step_kind,
        step_source: if matches!(event.step_source, AgentStepSource::Unknown) {
            AgentStepSource::Heuristic
        } else {
            event.step_source
        },
        step_confidence: if matches!(
            event.step_confidence,
            AgentConfidence::Unknown
        ) {
            AgentConfidence::Low
        } else {
            event.step_confidence
        },
        trust_level: resolved_agent_name
            .trust_level
            .unwrap_or(AgentTrustLevel::SelfReported),
        context_conflict: false,
        step_id_conflict: false,
        attempt: event.attempt,
        input_hash: event.input_hash.clone(),
        metadata,
    }
}

fn attach_agent_name_conflict_metadata(
    metadata: &mut serde_json::Value,
    conflict: &crate::agent::name::AgentNameConflict,
) {
    if !metadata.is_object() {
        let original = std::mem::take(metadata);
        *metadata = serde_json::json!({ "value": original });
    }
    let obj = metadata
        .as_object_mut()
        .expect("metadata should be object after wrapping");
    obj.insert(
        "registeredAgentName".to_string(),
        serde_json::json!(conflict.registered_agent_name),
    );
    obj.insert(
        "selfReportedAgentName".to_string(),
        serde_json::json!(conflict.self_reported_agent_name),
    );
    obj.insert(
        "selfReportedAgentNameSource".to_string(),
        serde_json::json!(conflict.self_reported_agent_name_source),
    );
    obj.insert("agentNameConflict".to_string(), serde_json::json!(true));
}

fn check_context_conflict(
    header_context: Option<&AgentContext>,
    event: &AgentEventInput,
    action: AgentConflictAction,
) -> Result<bool, AgentServiceError> {
    if matches!(action, AgentConflictAction::Disabled) {
        return Ok(false);
    }
    let Some(header_context) = header_context else {
        return Ok(false);
    };

    let conflict = context_fields_conflict(header_context, event);
    if conflict && matches!(action, AgentConflictAction::Strict) {
        return Err(AgentServiceError::ContextConflict);
    }
    Ok(conflict)
}

fn context_fields_conflict(
    header_context: &AgentContext,
    event: &AgentEventInput,
) -> bool {
    optional_str_conflicts(
        header_context.agent_id_external.as_deref(),
        event.agent_id_external.as_deref(),
    ) || optional_str_conflicts(
        header_context.run_id.as_deref(),
        event.run_id.as_deref(),
    ) || optional_str_conflicts(
        header_context.step_id.as_deref(),
        event.step_id.as_deref(),
    ) || optional_str_conflicts(
        header_context.parent_step_id.as_deref(),
        event.parent_step_id.as_deref(),
    ) || optional_str_conflicts(
        header_context.tool_call_id.as_deref(),
        event.tool_call_id.as_deref(),
    ) || optional_str_conflicts(
        header_context.handoff_id.as_deref(),
        event.handoff_id.as_deref(),
    ) || optional_str_conflicts(
        header_context.graph_node.as_deref(),
        event.graph_node.as_deref(),
    ) || optional_value_conflicts(header_context.step_kind, event.step_kind)
        || (header_context.step_source != AgentStepSource::Unknown
            && event.step_source != AgentStepSource::Unknown
            && header_context.step_source != event.step_source)
}

fn optional_str_conflicts(left: Option<&str>, right: Option<&str>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

fn optional_value_conflicts<T: PartialEq>(
    left: Option<T>,
    right: Option<T>,
) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left != right)
}

async fn check_step_conflict(
    app_state: &AppState,
    auth_ctx: &AuthContext,
    envelope: &AgentEventEnvelope,
) -> Result<StepConflictDecision, AgentServiceError> {
    let (Some(run_id), Some(step_id)) =
        (envelope.run_id.as_deref(), envelope.step_id.as_deref())
    else {
        return Ok(StepConflictDecision::NoConflict);
    };
    let agent_identity = envelope
        .agent_uid
        .map(|id| id.to_string())
        .or_else(|| envelope.agent_id_external.clone())
        .or_else(|| auth_ctx.virtual_key_id.map(|id| format!("vk:{id}")))
        .unwrap_or_else(|| "unknown".to_string());
    let input = StepFingerprintInput {
        parent_step_id: envelope.parent_step_id.clone(),
        step_kind: envelope.step_kind,
        graph_node: envelope.graph_node.clone(),
        tool_call_id: envelope.tool_call_id.clone(),
        attempt: envelope.attempt,
        input_hash: envelope.input_hash.clone(),
    };
    let key = step_state_key(
        *auth_ctx.org_id.as_ref(),
        &agent_identity,
        run_id,
        step_id,
    );
    let fingerprint = step_fingerprint(&input);
    detect_step_conflict(
        app_state.redis().map(std::sync::Arc::as_ref),
        &key,
        &fingerprint,
        app_state.config().agent.event_ttl_seconds,
        app_state.config().agent.step_conflict_action,
    )
    .await
    .map_err(|_| AgentServiceError::SinkFailed)
}

fn error_response(error: AgentServiceError) -> Response {
    let status = match error {
        AgentServiceError::Disabled => StatusCode::NOT_FOUND,
        AgentServiceError::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        AgentServiceError::BatchTooLarge
        | AgentServiceError::PayloadTooLarge
        | AgentServiceError::MetadataTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        AgentServiceError::InvalidJson => StatusCode::BAD_REQUEST,
        AgentServiceError::MissingAuth => StatusCode::UNAUTHORIZED,
        AgentServiceError::SinkFailed
        | AgentServiceError::PolicyUnavailable => StatusCode::BAD_GATEWAY,
        AgentServiceError::StepConflict
        | AgentServiceError::ContextConflict => StatusCode::CONFLICT,
    };
    let body = serde_json::json!({
        "error": {
            "type": "agent_gateway_error",
            "code": error.code(),
            "message": error.to_string(),
        }
    });
    json_response(status, &body).expect("JSON error response should build")
}

fn json_response<T: serde::Serialize>(
    status: StatusCode,
    value: &T,
) -> Result<Response, AgentServiceError> {
    let body = serde_json::to_vec(value)
        .map_err(|_| AgentServiceError::InvalidJson)?;
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::new(Full::new(Bytes::from(body))))
        .map_err(|_| AgentServiceError::InvalidJson)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    };
    use tonic::{Request as GrpcRequest, Response as GrpcResponse, Status};
    use uuid::Uuid;

    use super::*;
    use crate::{
        agent::context::{
            AgentConfidence, AgentEventPhase, AgentEventSourceTrust,
            AgentPolicyStage, AgentStepKind, AgentStepSource,
        },
        config::{Config, agent::AgentConflictAction},
        policy_proto::{
            AgentPolicyScope, EvaluateRequest, EvaluateResponse,
            ValidateAgentPolicyRequest, ValidateAgentPolicyResponse,
            X402InboundEvaluateRequest, X402InboundEvaluateResponse,
            policy_service_server::{PolicyService, PolicyServiceServer},
        },
        types::{org::OrgId, secret::Secret, user::UserId},
    };

    #[test]
    fn reject_reason_for_batch_limits() {
        assert_eq!(
            validate_event_limits(101, 1024, 100, 65_536),
            Err(AgentServiceError::BatchTooLarge)
        );
        assert_eq!(
            validate_event_limits(1, 65_537, 100, 65_536),
            Err(AgentServiceError::PayloadTooLarge)
        );
        assert_eq!(validate_event_limits(1, 1024, 100, 65_536), Ok(()));
    }

    #[test]
    fn reject_reason_for_metadata_limits() {
        let metadata = json!({ "value": "12345" });

        assert_eq!(
            validate_metadata_limit(&metadata, 5),
            Err(AgentServiceError::MetadataTooLarge)
        );
        assert_eq!(validate_metadata_limit(&metadata, 32), Ok(()));
    }

    #[test]
    fn auth_context_from_extensions_prefers_direct_auth_context() {
        let direct_org_id = Uuid::new_v4();
        let fallback_org_id = Uuid::new_v4();
        let mut extensions = http::Extensions::new();
        extensions.insert(std::sync::Arc::new(request_context_with_auth(
            auth_context(fallback_org_id),
        )));
        extensions.insert(auth_context(direct_org_id));

        let auth_ctx = auth_context_from_extensions(&extensions)
            .expect("direct auth context should be present");

        assert_eq!(*auth_ctx.org_id.as_ref(), direct_org_id);
    }

    #[test]
    fn auth_context_from_extensions_accepts_direct_auth_context() {
        let org_id = Uuid::new_v4();
        let mut extensions = http::Extensions::new();
        extensions.insert(auth_context(org_id));

        let auth_ctx = auth_context_from_extensions(&extensions)
            .expect("direct auth context should be present");

        assert_eq!(*auth_ctx.org_id.as_ref(), org_id);
    }

    #[test]
    fn auth_context_from_extensions_falls_back_to_request_context() {
        let org_id = Uuid::new_v4();
        let mut extensions = http::Extensions::new();
        extensions.insert(std::sync::Arc::new(request_context_with_auth(
            auth_context(org_id),
        )));

        let auth_ctx = auth_context_from_extensions(&extensions)
            .expect("request context auth should be present");

        assert_eq!(*auth_ctx.org_id.as_ref(), org_id);
    }

    #[test]
    fn auth_context_from_extensions_returns_none_when_missing() {
        let extensions = http::Extensions::new();

        assert!(auth_context_from_extensions(&extensions).is_none());
    }

    #[tokio::test]
    async fn error_response_uses_stable_code_for_batch_too_large() {
        let response = error_response(AgentServiceError::BatchTooLarge);

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "batch_too_large"
        );
    }

    #[tokio::test]
    async fn error_response_uses_stable_code_for_method_not_allowed() {
        let response = error_response(AgentServiceError::MethodNotAllowed);

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "method_not_allowed"
        );
    }

    #[tokio::test]
    async fn error_response_uses_stable_code_for_context_conflict() {
        let response = error_response(AgentServiceError::ContextConflict);

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "context_conflict"
        );
    }

    #[tokio::test]
    async fn error_response_uses_stable_code_for_policy_unavailable() {
        let response = error_response(AgentServiceError::PolicyUnavailable);

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response_json(response).await["error"]["code"],
            "policy_unavailable"
        );
    }

    #[test]
    fn context_conflict_warn_marks_mismatched_header_and_event() {
        let header_context = AgentContext {
            agent_id_external: Some("header-agent".to_string()),
            run_id: Some("run-1".to_string()),
            ..AgentContext::default()
        };
        let mut event = event_input();
        event.agent_id_external = Some("payload-agent".to_string());
        event.run_id = Some("run-1".to_string());

        let conflict = check_context_conflict(
            Some(&header_context),
            &event,
            AgentConflictAction::Warn,
        )
        .expect("warn conflict should not reject");

        assert!(conflict);
    }

    #[test]
    fn context_conflict_strict_rejects_mismatched_header_and_event() {
        let header_context = AgentContext {
            run_id: Some("run-from-header".to_string()),
            ..AgentContext::default()
        };
        let mut event = event_input();
        event.run_id = Some("run-from-payload".to_string());

        assert_eq!(
            check_context_conflict(
                Some(&header_context),
                &event,
                AgentConflictAction::Strict,
            ),
            Err(AgentServiceError::ContextConflict)
        );
    }

    #[test]
    fn context_conflict_disabled_ignores_mismatch() {
        let header_context = AgentContext {
            step_id: Some("header-step".to_string()),
            ..AgentContext::default()
        };
        let mut event = event_input();
        event.step_id = Some("payload-step".to_string());

        assert_eq!(
            check_context_conflict(
                Some(&header_context),
                &event,
                AgentConflictAction::Disabled,
            ),
            Ok(false)
        );
    }

    #[test]
    fn normalize_event_keeps_unknown_events_conservative() {
        let auth_ctx = auth_context(Uuid::new_v4());
        let mut event = event_input();
        event.step_source = AgentStepSource::Unknown;
        event.step_confidence = AgentConfidence::Unknown;

        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        );

        assert_eq!(envelope.event_source, AgentEventSource::Unknown);
        assert_eq!(envelope.step_source, AgentStepSource::Heuristic);
        assert_eq!(envelope.step_confidence, AgentConfidence::Low);
        assert_eq!(envelope.policy_stage, AgentPolicyStage::AuditOnly);
    }

    #[test]
    fn normalize_event_propagates_policy_mode() {
        let auth_ctx = auth_context(Uuid::new_v4());
        let event = event_input();

        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Enforce,
        );

        assert_eq!(envelope.policy_mode, AgentPolicyMode::Enforce);
    }

    #[test]
    fn should_validate_policy_only_for_pre_action_confident_events() {
        let mut envelope = envelope_for_policy_gate();
        envelope.event_phase = AgentEventPhase::Before;
        envelope.policy_stage = AgentPolicyStage::PreAction;
        envelope.policy_mode = AgentPolicyMode::Audit;
        envelope.step_confidence = AgentConfidence::High;
        assert!(should_validate_policy_for_event(&envelope));

        envelope.event_phase = AgentEventPhase::After;
        assert!(!should_validate_policy_for_event(&envelope));

        envelope.event_phase = AgentEventPhase::Before;
        envelope.policy_stage = AgentPolicyStage::AuditOnly;
        assert!(!should_validate_policy_for_event(&envelope));

        envelope.policy_stage = AgentPolicyStage::PreAction;
        envelope.step_confidence = AgentConfidence::Low;
        assert!(!should_validate_policy_for_event(&envelope));

        envelope.step_confidence = AgentConfidence::Medium;
        assert!(should_validate_policy_for_event(&envelope));
    }

    #[test]
    fn normalize_event_preserves_name() {
        let auth_ctx = auth_context(Uuid::new_v4());
        let mut event = event_input();
        event.name = Some("search".to_string());

        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        );

        assert_eq!(envelope.name.as_deref(), Some("search"));
    }

    #[test]
    fn normalize_event_prefers_registered_agent_name_and_records_conflict() {
        let mut auth_ctx = auth_context(Uuid::new_v4());
        auth_ctx.registered_agent_name = Some("Registered Bot".to_string());
        let mut event = event_input();
        event.agent_name = Some("Payload Bot".to_string());
        let header_context = AgentContext {
            agent_name: Some("Header Bot".to_string()),
            ..AgentContext::default()
        };

        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            Some(&header_context),
            AgentPolicyMode::Audit,
        );

        assert_eq!(
            envelope.alephant_agent_name.as_deref(),
            Some("Registered Bot")
        );
        assert_eq!(
            envelope.alephant_agent_name_source.as_deref(),
            Some("virtual_key_label")
        );
        assert_eq!(
            envelope.alephant_agent_trust_level.as_deref(),
            Some("auth_bound")
        );
        assert_eq!(envelope.trust_level, AgentTrustLevel::AuthBound);
        assert_eq!(envelope.metadata["registeredAgentName"], "Registered Bot");
        assert_eq!(envelope.metadata["selfReportedAgentName"], "Payload Bot");
        assert_eq!(
            envelope.metadata["selfReportedAgentNameSource"],
            "self_reported_event"
        );
        assert_eq!(envelope.metadata["agentNameConflict"], true);
    }

    #[test]
    fn normalize_event_redacts_basic_metadata() {
        let auth_ctx = auth_context(Uuid::new_v4());
        let mut event = event_input();
        event.metadata = json!({
            "authorization": "Bearer secret",
            "safe": "value"
        });

        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Basic,
            None,
            AgentPolicyMode::Audit,
        );

        assert_eq!(envelope.metadata["authorization"], "[redacted]");
        assert_eq!(envelope.metadata["safe"], "value");
    }

    #[test]
    fn normalize_event_preserves_metadata_when_redaction_disabled() {
        let auth_ctx = auth_context(Uuid::new_v4());
        let mut event = event_input();
        event.metadata = json!({
            "authorization": "Bearer secret",
            "safe": "value"
        });

        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        );

        assert_eq!(envelope.metadata["authorization"], "Bearer secret");
        assert_eq!(envelope.metadata["safe"], "value");
    }

    #[tokio::test]
    async fn check_step_conflict_returns_no_conflict_when_run_or_step_missing()
    {
        let app = crate::app::build_test_app(Config::default())
            .await
            .expect("build app");
        let auth_ctx = auth_context(Uuid::new_v4());
        let event = event_input();
        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        );

        let decision = check_step_conflict(&app.state, &auth_ctx, &envelope)
            .await
            .expect("missing identifiers should not fail");

        assert_eq!(decision, StepConflictDecision::NoConflict);
    }

    #[tokio::test]
    async fn check_step_conflict_returns_no_conflict_without_redis() {
        let app = crate::app::build_test_app(Config::default())
            .await
            .expect("build app");
        let auth_ctx = auth_context(Uuid::new_v4());
        let mut event = event_input();
        event.run_id = Some("run-1".to_string());
        event.step_id = Some("step-1".to_string());
        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        );

        let decision = check_step_conflict(&app.state, &auth_ctx, &envelope)
            .await
            .expect("missing redis should not fail");

        assert_eq!(decision, StepConflictDecision::NoConflict);
    }

    #[tokio::test]
    async fn check_step_conflict_respects_disabled_action() {
        let mut config = Config::default();
        config.agent.step_conflict_action = AgentConflictAction::Disabled;
        let app = crate::app::build_test_app(config).await.expect("build app");
        let auth_ctx = auth_context(Uuid::new_v4());
        let mut event = event_input();
        event.run_id = Some("run-1".to_string());
        event.step_id = Some("step-1".to_string());
        let envelope = normalize_event(
            &event,
            &auth_ctx,
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        );

        let decision = check_step_conflict(&app.state, &auth_ctx, &envelope)
            .await
            .expect("disabled action should not fail");

        assert_eq!(decision, StepConflictDecision::Disabled);
    }

    #[tokio::test]
    async fn agent_events_allows_tool_call_when_policy_disabled() {
        let redis = spawn_redis_fixture().await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = false;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(policy_validating_event_input()))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        assert_eq!(body["accepted"], 1);
        assert_eq!(body["rejected"], 0);
        assert_eq!(body["allowed"], true);
        assert_eq!(body["decisions"][0]["allowed"], true);
        assert_eq!(body["decisions"][0]["reason"], "policy_disabled_allowed");
    }

    #[tokio::test]
    async fn agent_events_response_includes_per_event_decision_and_sink_status()
    {
        let redis = spawn_redis_fixture().await;
        let mut config = agent_enabled_config();
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut event = event_input();
        event.source = Some(AgentEventSource::Unknown);
        event.event_type = "custom.event".to_string();
        event.agent_id_external = Some("agent-1".to_string());
        event.run_id = Some("run-1".to_string());
        event.step_id = Some("step-1".to_string());
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(event))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        assert_eq!(body["accepted"], 1);
        assert_eq!(body["decisions"][0]["eventType"], "unknown");
        assert_eq!(body["decisions"][0]["policyDecision"], "skipped");
        assert_eq!(body["decisions"][0]["policyStage"], "audit_only");
        assert_eq!(body["decisions"][0]["sinkStatus"], "sent");
    }

    #[tokio::test]
    async fn agent_events_fallback_to_http_when_redis_is_unavailable() {
        let fixture = spawn_agent_log_http_fixture(202).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = false;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint =
            format!("{}/v1/log/agent-event", fixture.url);
        config.agent.event_log_http_auth_token =
            Secret::from("agent-token".to_string());
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(policy_validating_event_input()))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let request = fixture
            .requests
            .lock()
            .expect("agent log request lock")
            .last()
            .cloned()
            .expect("agent event should be sent to HTTP fallback");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/log/agent-event");
        assert_eq!(request.header("authorization"), Some("Bearer agent-token"));
        assert!(request.body.contains("\"eventId\":\"evt-test\""));
        assert!(request.body.contains("\"alephantRunId\":\"run-1\""));
        assert!(!request.body.contains("\"event_id\""));
    }

    #[tokio::test]
    async fn agent_events_populates_trusted_auth_fields_in_event_log_payload() {
        let redis = spawn_redis_fixture().await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = false;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut auth_ctx = auth_context(Uuid::new_v4());
        let user_id = auth_ctx.user_id.to_string();
        let entity_id = Uuid::new_v4();
        auth_ctx.workspace_type = Some("enterprise".to_string());
        auth_ctx.entity_type = "agent".to_string();
        auth_ctx.entity_id = entity_id;
        auth_ctx.entity_name = "Support Bot".to_string();
        auth_ctx.registered_agent_name =
            Some("Registered Support Bot".to_string());
        let mut event = event_input();
        event.agent_name = Some("Payload Bot".to_string());
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request_with_auth(event, auth_ctx))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("agent event should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        assert_eq!(emitted["workspaceType"], "enterprise");
        assert_eq!(emitted["userId"], user_id);
        assert_eq!(emitted["entityType"], "agent");
        assert_eq!(emitted["entityId"], entity_id.to_string());
        assert_eq!(emitted["entityName"], "Support Bot");
        assert_eq!(emitted["alephantAgentId"], entity_id.to_string());
        assert_eq!(emitted["alephantAgentName"], "Registered Support Bot");
        assert_eq!(emitted["alephantAgentNameSource"], "virtual_key_label");
        assert_eq!(emitted["alephantAgentTrustLevel"], "auth_bound");
        assert_eq!(emitted["agentTrustLevel"], "auth_bound");
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["registeredAgentName"], "Registered Support Bot");
        assert_eq!(metadata["selfReportedAgentName"], "Payload Bot");
        assert_eq!(metadata["agentNameConflict"], true);
    }

    #[tokio::test]
    async fn agent_events_accepts_and_emits_policy_deny_decision() {
        let redis = spawn_redis_fixture().await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            allowed: false,
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            reason_message: "Tool denied.".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await =
            Some(Arc::new(client));
        let mut event = policy_validating_event_input();
        event.metadata = json!({
            "model": "gpt-4o",
            "provider": "openai",
            "tool_name": "search",
            "policy": {
                "allowed": true,
                "reason": "client-forged"
            },
            "safe": "value"
        });
        let auth_ctx = auth_context(Uuid::new_v4());
        let workspace_id = auth_ctx.org_id.to_string();
        let virtual_key_id =
            auth_ctx.virtual_key_id.expect("virtual key id for test");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request_with_auth(event, auth_ctx))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        assert_eq!(body["accepted"], 1);
        assert_eq!(body["rejected"], 0);
        assert_eq!(body["allowed"], false);
        assert_eq!(body["decisions"][0]["allowed"], false);
        assert_eq!(body["decisions"][0]["reason"], "agent_tool_denied");

        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("agent event should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        assert_eq!(emitted["eventId"], "evt-test");
        assert_eq!(emitted["workspaceId"], workspace_id);
        assert_eq!(emitted["alephantRunId"], "run-1");
        assert_eq!(emitted["alephantStepId"], "step-1");
        assert_eq!(emitted["policyAllowed"], false);
        assert_eq!(emitted["policyReason"], "agent_tool_denied");
        assert!(emitted.get("event_id").is_none());
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["reason"], "agent_tool_denied");
        assert_eq!(metadata["policy"]["blocked_by"], "agent.policy.tool");
        assert_eq!(
            metadata["policy"]["policy_scope"],
            "AGENT_POLICY_SCOPE_AGENT"
        );
        assert_eq!(metadata["policy_original"]["reason"], "client-forged");
        assert_eq!(metadata["safe"], "value");

        let policy_request = policy
            .requests
            .lock()
            .expect("policy request lock")
            .last()
            .cloned()
            .expect("agent policy request should be captured");
        assert_eq!(policy_request.agent_id, format!("vk:{virtual_key_id}"));
        assert_eq!(policy_request.virtual_key_id, virtual_key_id.to_string());
        assert_eq!(policy_request.run_id, "run-1");
        assert_eq!(policy_request.step_id, "step-1");
        assert_eq!(policy_request.tool_name, "search");
        let policy_metadata: serde_json::Value =
            serde_json::from_slice(&policy_request.metadata)
                .expect("policy metadata JSON");
        assert_eq!(policy_metadata["model"], "gpt-4o");
        assert_eq!(policy_metadata["provider"], "openai");
    }

    #[tokio::test]
    async fn agent_events_audit_only_stage_skips_policy_and_emits_log() {
        let redis = spawn_redis_fixture().await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            allowed: false,
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            reason_message: "Tool denied.".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await =
            Some(Arc::new(client));
        let mut event = event_input();
        event.event_type = "step.completed".to_string();
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(event))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        assert_eq!(body["accepted"], 1);
        assert_eq!(body["rejected"], 0);
        assert_eq!(body["allowed"], true);
        assert_eq!(body["decisions"][0]["allowed"], true);
        assert_eq!(
            body["decisions"][0]["reason"],
            "policy_skipped_audit_event"
        );
        assert!(
            policy
                .requests
                .lock()
                .expect("policy request lock")
                .is_empty(),
            "audit-only events should not call policy"
        );

        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("audit-only event should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        assert_eq!(emitted["eventType"], "step.completed");
        assert_eq!(emitted["policyAllowed"], true);
        assert_eq!(emitted["policyReason"], "policy_skipped_audit_event");
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["policy"]["reason"], "policy_skipped_audit_event");
    }

    #[tokio::test]
    async fn unknown_low_confidence_event_skips_policy_but_emits_log() {
        let redis = spawn_redis_fixture().await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            allowed: false,
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            reason_message: "Tool denied.".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await =
            Some(Arc::new(client));
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(unknown_low_confidence_agent_events_request())
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        assert_eq!(body["accepted"], 1);
        assert_eq!(body["rejected"], 0);
        assert_eq!(body["allowed"], true);
        assert_eq!(body["decisions"][0]["allowed"], true);
        assert_eq!(body["decisions"][0]["policyDecision"], "skipped");
        assert_eq!(body["decisions"][0]["policyStage"], "audit_only");
        assert!(
            policy
                .requests
                .lock()
                .expect("policy request lock")
                .is_empty(),
            "low-confidence audit events should not call policy"
        );

        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("low-confidence event should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        assert_eq!(emitted["eventType"], "unknown");
        assert_eq!(emitted["alephantRunId"], "run-unknown");
        assert_eq!(emitted["alephantStepId"], "step-unknown");
        assert_eq!(emitted["policyDecision"], "skipped");
        assert_eq!(emitted["policyReason"], "policy_skipped_audit_event");
        assert_eq!(emitted["sinkStatus"], "sent");
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["rawEventType"], "custom.event");
        assert_eq!(metadata["policy"]["policyDecision"], "skipped");
        assert_eq!(metadata["sinkStatus"], "sent");
    }

    #[tokio::test]
    async fn agent_events_emits_policy_unavailable_audit_before_returning_error()
     {
        let redis = spawn_redis_fixture().await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(policy_validating_event_input()))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "policy_unavailable");

        let payloads = redis.xadd_payloads.lock().expect("xadd payload lock");
        assert_eq!(payloads.len(), 1);
        let emitted: serde_json::Value =
            serde_json::from_str(&payloads[0]).expect("emitted event JSON");
        assert_eq!(emitted["eventType"], "policy_unavailable");
        assert_eq!(emitted["status"], "failed");
        assert_eq!(emitted["severity"], "error");
        assert_eq!(emitted["policyReason"], "policy_unavailable");
        assert_eq!(emitted["policyBlockedBy"], "agent.policy.unavailable");
        assert_eq!(emitted["policyScope"], "AGENT_POLICY_SCOPE_NONE");
        assert_eq!(emitted["policySnapshotRevision"], 0);
        assert_eq!(emitted["sinkStatus"], "sent");
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["policy"]["policyDecision"], "unavailable");
        assert_eq!(metadata["policy"]["reason"], "policy_unavailable");
        assert_eq!(
            metadata["policy"]["blocked_by"],
            "agent.policy.unavailable"
        );
        assert_eq!(
            metadata["policy"]["policy_scope"],
            "AGENT_POLICY_SCOPE_NONE"
        );
        assert_eq!(metadata["policy"]["snapshot_revision"], 0);
        assert_eq!(metadata["sinkStatus"], "sent");
    }

    #[tokio::test]
    async fn agent_events_preserves_client_metadata_when_policy_unavailable_audit_is_emitted()
     {
        let redis = spawn_redis_fixture().await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut event = policy_validating_event_input();
        event.metadata = json!({
            "policy": { "reason": "client-forged" },
            "status": "client-status",
            "severity": "client-severity"
        });
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(event))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "policy_unavailable");

        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("agent event should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["policy"]["reason"], "policy_unavailable");
        assert_eq!(metadata["policy_original"]["reason"], "client-forged");
        assert_eq!(metadata["status_original"], "client-status");
        assert_eq!(metadata["severity_original"], "client-severity");
    }

    #[tokio::test]
    async fn agent_events_policy_unavailable_returns_metadata_too_large_when_compact_audit_exceeds_metadata_limit()
     {
        let redis = spawn_redis_fixture().await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.max_metadata_bytes = 2;
        config.policy.enabled = true;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(policy_validating_event_input()))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "metadata_too_large");
        let payloads = redis.xadd_payloads.lock().expect("xadd payload lock");
        assert!(payloads.is_empty());
    }

    #[tokio::test]
    async fn agent_events_returns_sink_failed_when_policy_unavailable_audit_cannot_be_sent()
     {
        let fixture = spawn_agent_log_http_fixture(500).await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.policy.enabled = true;
        config.request_log.log_queue_redis_url = None;
        config.agent.event_log_http_fallback_enabled = true;
        config.agent.event_log_http_endpoint =
            format!("{}/v1/log/agent-event", fixture.url);
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(policy_validating_event_input()))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "sink_failed");
        let request = fixture
            .requests
            .lock()
            .expect("agent log request lock")
            .last()
            .cloned()
            .expect("policy unavailable audit should be sent to HTTP fallback");
        assert!(
            request
                .body
                .contains("\"eventType\":\"policy_unavailable\"")
        );
    }

    #[tokio::test]
    async fn agent_events_revalidates_metadata_size_after_policy_attachment() {
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.max_metadata_bytes = 20;
        config.policy.enabled = false;
        let app = crate::app::build_test_app(config).await.expect("build app");
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(event_input()))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "metadata_too_large");
    }

    #[tokio::test]
    async fn agent_events_emits_compact_deny_audit_when_policy_metadata_exceeds_limit()
     {
        let redis = spawn_redis_fixture().await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            allowed: false,
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            policy_id: "policy-compact".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.max_metadata_bytes = 900;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await =
            Some(Arc::new(client));
        let mut event = policy_validating_event_input();
        event.metadata = json!({
            "tool_name": "search",
            "blob": "x".repeat(820)
        });
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(event))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response_json(response).await;
        assert_eq!(body["accepted"], 1);
        assert_eq!(body["allowed"], false);
        assert_eq!(body["decisions"][0]["reason"], "agent_tool_denied");

        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("compact deny audit should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        assert_eq!(emitted["eventId"], "evt-test");
        assert_eq!(emitted["alephantRunId"], "run-1");
        assert_eq!(emitted["alephantStepId"], "step-1");
        assert_eq!(emitted["policyAllowed"], false);
        assert_eq!(emitted["policyReason"], "agent_tool_denied");
        assert_eq!(emitted["policyBlockedBy"], "agent.policy.tool");
        assert_eq!(emitted["policyId"], "policy-compact");
        assert!(emitted.get("event_id").is_none());
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        assert_eq!(metadata["metadata_truncated"], true);
        assert_eq!(
            metadata["metadata_truncation_reason"],
            "agent_policy_metadata_limit"
        );
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["reason"], "agent_tool_denied");
        assert_eq!(metadata["policy"]["policy_id"], "policy-compact");
        assert!(metadata.get("blob").is_none());
    }

    #[tokio::test]
    async fn agent_events_truncates_verbose_policy_fields_in_compact_deny_audit()
     {
        let redis = spawn_redis_fixture().await;
        let policy = spawn_policy_service(ValidateAgentPolicyResponse {
            allowed: false,
            reason: "r".repeat(5_000),
            blocked_by: "b".repeat(5_000),
            policy_id: "p".repeat(5_000),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        })
        .await;
        let mut config = Config::default();
        config.agent.enabled = true;
        config.agent.max_metadata_bytes = 900;
        config.policy.enabled = true;
        config.policy.grpc_endpoint = policy.endpoint;
        config.request_log.log_queue_redis_url =
            Some(redis.endpoint.parse().expect("redis url"));
        let app = crate::app::build_test_app(config).await.expect("build app");
        let client = crate::content_filter::ContentFilterGrpcClient::connect(
            app.state.config().policy.grpc_endpoint.clone(),
        )
        .await
        .expect("policy grpc client");
        *app.state.0.content_filter.reconnect_lock().write().await =
            Some(Arc::new(client));
        let mut event = policy_validating_event_input();
        event.metadata = json!({
            "tool_name": "search",
            "blob": "x".repeat(820)
        });
        let mut service = AgentEventsService::new(app.state);
        let response = service
            .call(agent_events_request(event))
            .await
            .expect("agent events response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let payload = redis
            .xadd_payloads
            .lock()
            .expect("xadd payload lock")
            .last()
            .cloned()
            .expect("compact deny audit should be emitted to redis");
        let emitted: serde_json::Value =
            serde_json::from_str(&payload).expect("emitted event JSON");
        assert_eq!(emitted["eventId"], "evt-test");
        assert_eq!(emitted["alephantRunId"], "run-1");
        assert_eq!(emitted["alephantStepId"], "step-1");
        assert_eq!(emitted["policyAllowed"], false);
        assert!(
            emitted["policyReason"]
                .as_str()
                .expect("policyReason")
                .len()
                <= 128
        );
        assert!(
            emitted["policyBlockedBy"]
                .as_str()
                .expect("policyBlockedBy")
                .len()
                <= 128
        );
        assert!(emitted["policyId"].as_str().expect("policyId").len() <= 128);
        assert_eq!(
            emitted["policyReason"]
                .as_str()
                .expect("policyReason")
                .len(),
            128
        );
        assert_eq!(
            emitted["policyBlockedBy"]
                .as_str()
                .expect("policyBlockedBy")
                .len(),
            128
        );
        assert_eq!(emitted["policyId"].as_str().expect("policyId").len(), 128);
        assert!(emitted.get("event_id").is_none());
        let metadata: serde_json::Value = serde_json::from_str(
            emitted["metadata"].as_str().expect("metadata JSON string"),
        )
        .expect("metadata should be JSON");
        let metadata_len =
            serde_json::to_vec(&metadata).expect("metadata JSON").len();
        assert!(metadata_len <= 900);
        assert_eq!(
            metadata["policy"]["reason"].as_str().expect("reason").len(),
            128
        );
        assert_eq!(
            metadata["policy"]["blocked_by"]
                .as_str()
                .expect("blocked_by")
                .len(),
            128
        );
        assert_eq!(
            metadata["policy"]["policy_id"]
                .as_str()
                .expect("policy_id")
                .len(),
            128
        );
    }

    fn request_context_with_auth(auth_context: AuthContext) -> RequestContext {
        RequestContext {
            router_config: None,
            auth_context: Some(auth_context),
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
            agent_context: None,
        }
    }

    fn agent_enabled_config() -> Config {
        let mut config = Config::default();
        config.agent.enabled = true;
        config
    }

    fn auth_context(org_id: Uuid) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(org_id),
            workspace_type: None,
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: "vk-test".to_string(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: None,
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            registered_agent_name: None,
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }

    fn agent_events_request(event: AgentEventInput) -> Request {
        agent_events_request_with_auth(event, auth_context(Uuid::new_v4()))
    }

    fn agent_events_request_with_auth(
        event: AgentEventInput,
        auth_context: AuthContext,
    ) -> Request {
        let body = serde_json::to_vec(&json!({
            "events": [{
                "source": event.source.map(|source| source.as_str()),
                "version": event.version,
                "event_id": event.event_id,
                "type": event.event_type,
                "event_phase": event.event_phase.as_str(),
                "policy_stage": event.policy_stage.as_str(),
                "agent_name": event.agent_name,
                "agent_id": event.agent_id_external,
                "run_id": event.run_id,
                "step_id": event.step_id,
                "parent_step_id": event.parent_step_id,
                "tool_call_id": event.tool_call_id,
                "graph_node": event.graph_node,
                "step_kind": "tool_call",
                "step_source": "runtime",
                "step_confidence": event.step_confidence.as_str(),
                "attempt": event.attempt,
                "input_hash": event.input_hash,
                "metadata": event.metadata
            }]
        }))
        .expect("agent events JSON");
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/agent/events")
            .body(Body::new(Full::new(Bytes::from(body))))
            .expect("agent events request");
        request.extensions_mut().insert(auth_context);
        request
    }

    fn unknown_low_confidence_agent_events_request() -> Request {
        let body = serde_json::to_vec(&json!({
            "events": [{
                "version": "2026-05-27",
                "event_id": "evt-unknown-low",
                "source": "unknown",
                "type": "custom.event",
                "agent_id": "coding-agent",
                "run_id": "run-unknown",
                "step_id": "step-unknown",
                "event_phase": "before",
                "policy_stage": "audit_only",
                "step_kind": "tool_call",
                "step_source": "runtime",
                "step_confidence": "low",
                "metadata": {}
            }]
        }))
        .expect("agent events JSON");
        let mut request = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/agent/events")
            .body(Body::new(Full::new(Bytes::from(body))))
            .expect("agent events request");
        request
            .extensions_mut()
            .insert(auth_context(Uuid::new_v4()));
        request
    }

    fn event_input() -> AgentEventInput {
        AgentEventInput {
            source: None,
            framework: None,
            version: "2026-05-27".to_string(),
            event_id: Some("evt-test".to_string()),
            event_type: "step.started".to_string(),
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            name: None,
            agent_name: None,
            agent_id_external: Some("coding-agent".to_string()),
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            parent_step_id: Some("parent-1".to_string()),
            tool_call_id: Some("tool-1".to_string()),
            handoff_id: None,
            graph_node: Some("node-1".to_string()),
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::High,
            attempt: Some(1),
            input_hash: Some("sha256:test".to_string()),
            timestamp: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            raw_fields: serde_json::Map::new(),
        }
    }

    fn policy_validating_event_input() -> AgentEventInput {
        let mut event = event_input();
        event.source = Some(AgentEventSource::Alephant);
        event.event_phase = AgentEventPhase::Before;
        event.policy_stage = AgentPolicyStage::PreAction;
        event.step_confidence = AgentConfidence::High;
        event
    }

    fn envelope_for_policy_gate() -> AgentEventEnvelope {
        normalize_event(
            &event_input(),
            &auth_context(Uuid::new_v4()),
            AgentMetadataRedaction::Disabled,
            None,
            AgentPolicyMode::Audit,
        )
    }

    struct GrpcFixture {
        endpoint: String,
        requests: Arc<Mutex<Vec<ValidateAgentPolicyRequest>>>,
    }

    async fn spawn_policy_service(
        response: ValidateAgentPolicyResponse,
    ) -> GrpcFixture {
        let listener =
            TcpListener::bind("127.0.0.1:0").await.expect("bind policy");
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
            if crate::content_filter::ContentFilterGrpcClient::connect(
                endpoint.clone(),
            )
            .await
            .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        GrpcFixture { endpoint, requests }
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

    struct RedisFixture {
        endpoint: String,
        xadd_payloads: Arc<Mutex<Vec<String>>>,
        _shutdown: oneshot::Sender<()>,
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

    async fn spawn_agent_log_http_fixture(
        status_code: u16,
    ) -> AgentLogHttpFixture {
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
            let response = format!(
                "HTTP/1.1 {status_code} {reason}\r\nContent-Length: 0\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    }

    fn parse_agent_log_http_request(
        buffer: &[u8],
    ) -> Option<AgentLogHttpRequest> {
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

    async fn spawn_redis_fixture() -> RedisFixture {
        let listener =
            TcpListener::bind("127.0.0.1:0").await.expect("bind redis");
        let addr = listener.local_addr().expect("redis addr");
        let xadd_payloads = Arc::new(Mutex::new(Vec::new()));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let server_payloads = xadd_payloads.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.expect("redis accept");
                        let payloads = server_payloads.clone();
                        tokio::spawn(handle_redis_connection(stream, payloads));
                    }
                }
            }
        });
        RedisFixture {
            endpoint: format!("redis://{addr}"),
            xadd_payloads,
            _shutdown: shutdown_tx,
        }
    }

    async fn handle_redis_connection(
        mut stream: TcpStream,
        payloads: Arc<Mutex<Vec<String>>>,
    ) {
        let mut buffer = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let read = match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => read,
            };
            buffer.extend_from_slice(&chunk[..read]);

            while let Some((command, consumed)) = parse_resp_command(&buffer) {
                buffer.drain(..consumed);
                if command
                    .first()
                    .is_some_and(|name| name.eq_ignore_ascii_case("XADD"))
                    && let Some(payload) = command.get(4)
                {
                    payloads
                        .lock()
                        .expect("xadd payload lock")
                        .push(payload.clone());
                }
                let response = redis_response(&command);
                if stream.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
        }
    }

    fn redis_response(command: &[String]) -> String {
        match command
            .first()
            .map(|command| command.to_ascii_uppercase())
            .as_deref()
        {
            Some("GET") => "$-1\r\n".to_string(),
            Some("PING") => "+PONG\r\n".to_string(),
            Some("XADD") => "$3\r\n0-1\r\n".to_string(),
            Some("CLIENT" | "HELLO" | "SET" | "EXPIRE") => {
                "+OK\r\n".to_string()
            }
            _ => "+OK\r\n".to_string(),
        }
    }

    fn parse_resp_command(buffer: &[u8]) -> Option<(Vec<String>, usize)> {
        if buffer.first().copied()? != b'*' {
            return None;
        }
        let (count_line, mut index) = read_line(buffer, 1)?;
        let count = std::str::from_utf8(count_line)
            .ok()?
            .parse::<usize>()
            .ok()?;
        let mut parts = Vec::with_capacity(count);

        for _ in 0..count {
            if buffer.get(index).copied()? != b'$' {
                return None;
            }
            let (len_line, next_index) = read_line(buffer, index + 1)?;
            index = next_index;
            let len =
                std::str::from_utf8(len_line).ok()?.parse::<usize>().ok()?;
            if buffer.len() < index + len + 2 {
                return None;
            }
            let part = std::str::from_utf8(&buffer[index..index + len])
                .ok()?
                .to_string();
            parts.push(part);
            index += len;
            if buffer.get(index..index + 2)? != b"\r\n" {
                return None;
            }
            index += 2;
        }

        Some((parts, index))
    }

    fn read_line(buffer: &[u8], start: usize) -> Option<(&[u8], usize)> {
        let end = buffer[start..]
            .windows(2)
            .position(|window| window == b"\r\n")?
            + start;
        Some((&buffer[start..end], end + 2))
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
