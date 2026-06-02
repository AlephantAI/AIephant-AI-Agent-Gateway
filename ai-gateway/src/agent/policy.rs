use std::time::Duration;

use crate::{
    agent::{
        context::AgentStepKind,
        event::{AgentEventEnvelope, AgentPolicyDecision, AgentPolicyIssueDto},
    },
    app_state::AppState,
    policy_proto::{
        AgentPolicyIssue, AgentPolicyPhase, AgentPolicyRuntimeContext, AgentPolicyScope,
        ValidateAgentPolicyRequest, ValidateAgentPolicyResponse,
    },
    types::extensions::AuthContext,
};

pub const POLICY_DISABLED_ALLOWED_REASON: &str = "policy_disabled_allowed";
pub const POLICY_SKIPPED_AUDIT_EVENT_REASON: &str = "policy_skipped_audit_event";
const COMPACT_POLICY_FIELD_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentPolicyError {
    #[error("agent policy unavailable")]
    Unavailable,
}

pub fn agent_policy_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms)
}

pub async fn validate_agent_policy(
    app_state: &AppState,
    auth: &AuthContext,
    envelope: &AgentEventEnvelope,
) -> Result<AgentPolicyDecision, AgentPolicyError> {
    let cfg = app_state.config();
    if !cfg.policy.enabled {
        return Ok(disabled_allowed_decision(envelope));
    }

    let Some(client) = app_state.content_filter_client().await else {
        return unavailable_result(app_state, envelope, "agent policy client not initialised");
    };

    let mut inner = client.inner();
    let req = build_validate_agent_policy_request(auth, envelope);
    let call = inner.validate_agent_policy(req);
    let result =
        tokio::time::timeout(agent_policy_timeout(cfg.agent.policy_timeout_ms), call).await;

    match result {
        Ok(Ok(response)) => Ok(decision_from_policy_response(
            envelope,
            response.into_inner(),
        )),
        Ok(Err(status)) => unavailable_status_result(app_state, envelope, status),
        Err(_elapsed) => {
            unavailable_result(app_state, envelope, "agent policy validation timed out")
        }
    }
}

fn unavailable_status_result(
    _app_state: &AppState,
    _envelope: &AgentEventEnvelope,
    status: tonic::Status,
) -> Result<AgentPolicyDecision, AgentPolicyError> {
    let code = status.code();
    tracing::warn!(
        grpc_code = ?code,
        "agent policy unavailable; denying agent event gate"
    );
    Err(AgentPolicyError::Unavailable)
}

fn unavailable_result(
    _app_state: &AppState,
    _envelope: &AgentEventEnvelope,
    message: &str,
) -> Result<AgentPolicyDecision, AgentPolicyError> {
    tracing::warn!(%message, "agent policy unavailable; denying agent event gate");
    Err(AgentPolicyError::Unavailable)
}

#[must_use]
pub fn phase_for_step_kind(kind: Option<AgentStepKind>) -> AgentPolicyPhase {
    match kind {
        Some(AgentStepKind::Planning | AgentStepKind::Reasoning | AgentStepKind::LlmCall) => {
            AgentPolicyPhase::ModelRequest
        }
        Some(AgentStepKind::ToolCall) => AgentPolicyPhase::ToolCall,
        Some(
            AgentStepKind::ToolResult
            | AgentStepKind::Handoff
            | AgentStepKind::Approval
            | AgentStepKind::Checkpoint
            | AgentStepKind::FinalAnswer
            | AgentStepKind::Retry
            | AgentStepKind::ErrorRecovery
            | AgentStepKind::Unknown,
        )
        | None => AgentPolicyPhase::RunUpdate,
    }
}

#[must_use]
pub fn build_validate_agent_policy_request(
    auth: &AuthContext,
    envelope: &AgentEventEnvelope,
) -> ValidateAgentPolicyRequest {
    let agent_id = policy_agent_id(auth, envelope);
    let runtime = runtime_context_from_metadata(&envelope.metadata);
    let tool_name = metadata_string(&envelope.metadata, "tool_name");
    let tool_name = if tool_name.is_empty() {
        envelope.name.clone().unwrap_or_default()
    } else {
        tool_name
    };
    let metadata = serde_json::to_vec(&envelope.metadata).unwrap_or_default();

    ValidateAgentPolicyRequest {
        workspace_id: auth.org_id.to_string(),
        request_id: envelope.event_id.clone(),
        agent_id,
        department_id: if auth.department_id.is_nil() {
            String::new()
        } else {
            auth.department_id.to_string()
        },
        run_id: envelope.run_id.clone().unwrap_or_default(),
        phase: phase_for_step_kind(envelope.step_kind) as i32,
        model: metadata_string(&envelope.metadata, "model"),
        provider: metadata_string(&envelope.metadata, "provider"),
        tool_name,
        estimated_cost_cents: metadata_i64(&envelope.metadata, "estimated_cost_cents"),
        runtime,
        locale: String::new(),
        user_id: auth.user_id.to_string(),
        virtual_key_id: auth
            .virtual_key_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        step_id: envelope.step_id.clone().unwrap_or_default(),
        metadata,
        entity_type: auth.entity_type.clone(),
        entity_id: if auth.entity_id.is_nil() {
            String::new()
        } else {
            auth.entity_id.to_string()
        },
    }
}

fn policy_agent_id(auth: &AuthContext, envelope: &AgentEventEnvelope) -> String {
    if auth.entity_type.eq_ignore_ascii_case("agent") && !auth.entity_id.is_nil() {
        return auth.entity_id.to_string();
    }
    envelope
        .agent_uid
        .map(|id| id.to_string())
        .or_else(|| auth.virtual_key_id.map(|id| format!("vk:{id}")))
        .or_else(|| envelope.agent_id_external.clone())
        .unwrap_or_default()
}

#[must_use]
pub fn decision_from_policy_response(
    envelope: &AgentEventEnvelope,
    response: ValidateAgentPolicyResponse,
) -> AgentPolicyDecision {
    let policy_decision = if response.allowed {
        "allowed"
    } else {
        "denied"
    };
    AgentPolicyDecision {
        event_id: envelope.event_id.clone(),
        event_type: envelope.event_type.clone(),
        run_id: envelope.run_id.clone(),
        step_id: envelope.step_id.clone(),
        allowed: response.allowed,
        policy_decision: policy_decision.to_string(),
        policy_stage: envelope.policy_stage.as_str().to_string(),
        sink_status: String::new(),
        reason: response.reason,
        blocked_by: response.blocked_by,
        route_hint: response.route_hint,
        snapshot_revision: response.snapshot_revision,
        reason_message: response.reason_message,
        policy_id: response.policy_id,
        policy_scope: AgentPolicyScope::try_from(response.policy_scope)
            .unwrap_or(AgentPolicyScope::Unspecified)
            .as_str_name()
            .to_string(),
        violations: response.violations.into_iter().map(issue_to_dto).collect(),
        warnings: response.warnings.into_iter().map(issue_to_dto).collect(),
    }
}

#[must_use]
pub fn disabled_allowed_decision(envelope: &AgentEventEnvelope) -> AgentPolicyDecision {
    AgentPolicyDecision {
        event_id: envelope.event_id.clone(),
        event_type: envelope.event_type.clone(),
        run_id: envelope.run_id.clone(),
        step_id: envelope.step_id.clone(),
        allowed: true,
        policy_decision: "allowed".to_string(),
        policy_stage: envelope.policy_stage.as_str().to_string(),
        sink_status: String::new(),
        reason: POLICY_DISABLED_ALLOWED_REASON.to_string(),
        blocked_by: String::new(),
        route_hint: String::new(),
        snapshot_revision: 0,
        reason_message: String::new(),
        policy_id: String::new(),
        policy_scope: AgentPolicyScope::None.as_str_name().to_string(),
        violations: Vec::new(),
        warnings: Vec::new(),
    }
}

#[must_use]
pub fn skipped_audit_decision(envelope: &AgentEventEnvelope) -> AgentPolicyDecision {
    AgentPolicyDecision {
        event_id: envelope.event_id.clone(),
        event_type: envelope.event_type.clone(),
        run_id: envelope.run_id.clone(),
        step_id: envelope.step_id.clone(),
        allowed: true,
        policy_decision: "skipped".to_string(),
        policy_stage: envelope.policy_stage.as_str().to_string(),
        sink_status: String::new(),
        reason: POLICY_SKIPPED_AUDIT_EVENT_REASON.to_string(),
        blocked_by: String::new(),
        route_hint: String::new(),
        snapshot_revision: 0,
        reason_message: String::new(),
        policy_id: String::new(),
        policy_scope: AgentPolicyScope::None.as_str_name().to_string(),
        violations: Vec::new(),
        warnings: Vec::new(),
    }
}

pub fn attach_policy_decision_to_metadata(
    metadata: &mut serde_json::Value,
    decision: &AgentPolicyDecision,
) {
    if !metadata.is_object() {
        let original = std::mem::take(metadata);
        *metadata = serde_json::json!({ "value": original });
    }
    let obj = metadata
        .as_object_mut()
        .expect("metadata is forced to object above");
    if let Some(existing) = obj.remove("policy") {
        let key = available_policy_original_key(obj);
        obj.insert(key.to_string(), existing);
    }
    obj.insert(
        "policy".to_string(),
        serde_json::json!({
            "allowed": decision.allowed,
            "decision": decision.policy_decision,
            "policyDecision": decision.policy_decision,
            "reason": decision.reason,
            "blocked_by": decision.blocked_by,
            "route_hint": decision.route_hint,
            "policy_id": decision.policy_id,
            "policy_scope": decision.policy_scope,
            "snapshot_revision": decision.snapshot_revision
        }),
    );
}

#[must_use]
pub fn compact_policy_decision_metadata(decision: &AgentPolicyDecision) -> serde_json::Value {
    serde_json::json!({
        "metadata_truncated": true,
        "metadata_truncation_reason": "agent_policy_metadata_limit",
        "policy": {
            "allowed": decision.allowed,
            "decision": decision.policy_decision,
            "policyDecision": decision.policy_decision,
            "reason": truncate_policy_field(&decision.reason),
            "blocked_by": truncate_policy_field(&decision.blocked_by),
            "policy_id": truncate_policy_field(&decision.policy_id),
            "policy_scope": decision.policy_scope,
            "snapshot_revision": decision.snapshot_revision
        }
    })
}

fn truncate_policy_field(value: &str) -> String {
    value.chars().take(COMPACT_POLICY_FIELD_CHARS).collect()
}

fn available_policy_original_key(obj: &serde_json::Map<String, serde_json::Value>) -> String {
    for key in ["policy_original", "policy_client_original"] {
        if !obj.contains_key(key) {
            return key.to_string();
        }
    }

    let mut index = 1_u32;
    loop {
        let key = format!("policy_client_original_{index}");
        if !obj.contains_key(&key) {
            return key;
        }
        index += 1;
    }
}

fn issue_to_dto(issue: AgentPolicyIssue) -> AgentPolicyIssueDto {
    AgentPolicyIssueDto {
        field: issue.field,
        reason: issue.reason,
        blocked_by: issue.blocked_by,
        reason_message: issue.reason_message,
        actual: issue.actual,
        expected: issue.expected,
        actual_value: issue.actual_value,
        expected_value: issue.expected_value,
        unit: issue.unit,
        operator: issue.operator,
    }
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> String {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn metadata_i64(metadata: &serde_json::Value, key: &str) -> Option<i64> {
    metadata.get(key).and_then(serde_json::Value::as_i64)
}

fn metadata_i32(metadata: &serde_json::Value, key: &str) -> Option<i32> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn runtime_context_from_metadata(
    metadata: &serde_json::Value,
) -> Option<AgentPolicyRuntimeContext> {
    let runtime = AgentPolicyRuntimeContext {
        request_count: metadata_i32(metadata, "request_count"),
        duration_sec: metadata_i32(metadata, "duration_sec"),
        retry_count: metadata_i32(metadata, "retry_count"),
    };
    if runtime.request_count.is_none()
        && runtime.duration_sec.is_none()
        && runtime.retry_count.is_none()
    {
        None
    } else {
        Some(runtime)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::{
        agent::{
            context::{
                AgentConfidence, AgentEventPhase, AgentEventSourceTrust, AgentPolicyMode,
                AgentPolicyStage, AgentStepKind, AgentStepSource, AgentTrustLevel,
            },
            event::AgentEventSource,
        },
        config::{Config, policy::OnUnavailable},
        types::{org::OrgId, secret::Secret, user::UserId},
    };

    #[test]
    fn maps_step_kind_to_agent_policy_phase() {
        assert_eq!(
            phase_for_step_kind(Some(AgentStepKind::Planning)),
            AgentPolicyPhase::ModelRequest
        );
        assert_eq!(
            phase_for_step_kind(Some(AgentStepKind::Reasoning)),
            AgentPolicyPhase::ModelRequest
        );
        assert_eq!(
            phase_for_step_kind(Some(AgentStepKind::LlmCall)),
            AgentPolicyPhase::ModelRequest
        );
        assert_eq!(
            phase_for_step_kind(Some(AgentStepKind::ToolCall)),
            AgentPolicyPhase::ToolCall
        );
        assert_eq!(
            phase_for_step_kind(Some(AgentStepKind::ToolResult)),
            AgentPolicyPhase::RunUpdate
        );
        assert_eq!(
            phase_for_step_kind(Some(AgentStepKind::Unknown)),
            AgentPolicyPhase::RunUpdate
        );
        assert_eq!(phase_for_step_kind(None), AgentPolicyPhase::RunUpdate);
    }

    #[test]
    fn builds_validate_agent_policy_request_from_auth_and_envelope() {
        let workspace_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let virtual_key_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let department_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let auth = auth_context(workspace_id, virtual_key_id, department_id);
        let envelope = envelope(json!({
            "model": "gpt-4o",
            "provider": "openai",
            "tool_name": "search",
            "estimated_cost_cents": 7,
            "request_count": 3,
            "duration_sec": 12,
            "retry_count": 1
        }));

        let req = build_validate_agent_policy_request(&auth, &envelope);

        assert_eq!(req.workspace_id, workspace_id.to_string());
        assert_eq!(req.request_id, "evt-1");
        assert_eq!(req.agent_id, format!("vk:{virtual_key_id}"));
        assert_eq!(req.department_id, department_id.to_string());
        assert_eq!(req.run_id, "run-1");
        assert_eq!(req.step_id, "step-1");
        assert_eq!(req.phase, AgentPolicyPhase::ToolCall as i32);
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.provider, "openai");
        assert_eq!(req.tool_name, "search");
        assert_eq!(req.estimated_cost_cents, Some(7));
        let runtime = req.runtime.expect("runtime context");
        assert_eq!(runtime.request_count, Some(3));
        assert_eq!(runtime.duration_sec, Some(12));
        assert_eq!(runtime.retry_count, Some(1));
        assert_eq!(req.locale, "");
        assert_eq!(req.user_id, auth.user_id.to_string());
        assert_eq!(req.virtual_key_id, virtual_key_id.to_string());
        let metadata: serde_json::Value =
            serde_json::from_slice(&req.metadata).expect("metadata JSON");
        assert_eq!(metadata["model"], "gpt-4o");
        assert_eq!(metadata["provider"], "openai");
    }

    #[test]
    fn auth_bound_agent_identity_wins_over_self_reported_agent_id() {
        let workspace_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let virtual_key_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let department_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let agent_entity_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
        let mut auth = auth_context(workspace_id, virtual_key_id, department_id);
        auth.entity_type = "agent".to_string();
        auth.entity_id = agent_entity_id;
        let mut envelope = envelope(json!({}));
        envelope.agent_id_external = Some("spoofed-agent".to_string());

        let req = build_validate_agent_policy_request(&auth, &envelope);

        assert_eq!(req.agent_id, agent_entity_id.to_string());
        assert_eq!(req.entity_type, "agent");
        assert_eq!(req.entity_id, agent_entity_id.to_string());
    }

    #[test]
    fn falls_back_to_envelope_name_for_tool_name() {
        let workspace_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let virtual_key_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let department_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let auth = auth_context(workspace_id, virtual_key_id, department_id);
        let mut envelope = envelope(json!({
            "model": "gpt-4o",
            "provider": "openai"
        }));
        envelope.name = Some("search".to_string());

        let req = build_validate_agent_policy_request(&auth, &envelope);

        assert_eq!(req.tool_name, "search");
    }

    #[test]
    fn converts_policy_response_to_decision() {
        let response = ValidateAgentPolicyResponse {
            allowed: false,
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            route_hint: "stop".to_string(),
            snapshot_revision: 9,
            reason_message: "Tool call blocked.".to_string(),
            policy_id: "policy-1".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            violations: vec![AgentPolicyIssue {
                field: "tool_name".to_string(),
                reason: "not_allowed".to_string(),
                blocked_by: "agent.policy.tool".to_string(),
                reason_message: "Tool not allowed.".to_string(),
                actual: "shell".to_string(),
                expected: vec!["search".to_string()],
                actual_value: Some(2.0),
                expected_value: Some(1.0),
                unit: "count".to_string(),
                operator: "lte".to_string(),
            }],
            warnings: Vec::new(),
        };

        let envelope = envelope(json!({}));
        let decision = decision_from_policy_response(&envelope, response);

        assert!(!decision.allowed);
        assert_eq!(decision.event_id, "evt-1");
        assert_eq!(decision.event_type, "tool.call");
        assert_eq!(decision.run_id.as_deref(), Some("run-1"));
        assert_eq!(decision.step_id.as_deref(), Some("step-1"));
        assert_eq!(decision.policy_decision, "denied");
        assert_eq!(decision.policy_stage, "audit_only");
        assert_eq!(decision.sink_status, "");
        assert_eq!(decision.policy_scope, "AGENT_POLICY_SCOPE_AGENT");
        assert_eq!(decision.violations[0].expected, vec!["search"]);
    }

    #[test]
    fn denied_policy_response_keeps_allowed_false_for_sdk_gate() {
        let response = ValidateAgentPolicyResponse {
            allowed: false,
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            reason_message: "Tool denied.".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        };

        let envelope = envelope(json!({}));
        let decision = decision_from_policy_response(&envelope, response);

        assert!(!decision.allowed);
        assert_eq!(decision.reason, "agent_tool_denied");
        assert_eq!(decision.blocked_by, "agent.policy.tool");
        assert_eq!(decision.reason_message, "Tool denied.");
    }

    #[test]
    fn skipped_audit_decision_uses_envelope_stage_and_event() {
        let mut envelope = envelope(json!({}));
        envelope.event_type = "custom.event".to_string();
        envelope.policy_stage = AgentPolicyStage::AuditOnly;

        let decision = skipped_audit_decision(&envelope);

        assert!(decision.allowed);
        assert_eq!(decision.event_type, "custom.event");
        assert_eq!(decision.policy_decision, "skipped");
        assert_eq!(decision.policy_stage, "audit_only");
        assert_eq!(decision.sink_status, "");
        assert_eq!(decision.reason, POLICY_SKIPPED_AUDIT_EVENT_REASON);
    }

    #[test]
    fn attaches_policy_decision_without_trusting_client_policy_key() {
        let mut metadata = json!({
            "policy": { "allowed": true, "reason": "client-forged" },
            "safe": "value"
        });
        let decision = denied_policy_decision();

        attach_policy_decision_to_metadata(&mut metadata, &decision);

        assert_eq!(metadata["safe"], "value");
        assert_eq!(metadata["policy_original"]["reason"], "client-forged");
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["decision"], "denied");
        assert_eq!(metadata["policy"]["policyDecision"], "denied");
        assert_eq!(metadata["policy"]["reason"], "agent_tool_denied");
        assert_eq!(metadata["policy"]["blocked_by"], "agent.policy.tool");
        assert_eq!(metadata["policy"]["route_hint"], "stop");
        assert_eq!(metadata["policy"]["policy_id"], "policy-1");
        assert_eq!(
            metadata["policy"]["policy_scope"],
            "AGENT_POLICY_SCOPE_AGENT"
        );
        assert_eq!(metadata["policy"]["snapshot_revision"], 123);
    }

    #[test]
    fn preserves_non_object_metadata_under_value_when_attaching_policy() {
        let mut metadata = json!(["client", "metadata"]);
        let decision = denied_policy_decision();

        attach_policy_decision_to_metadata(&mut metadata, &decision);

        assert_eq!(metadata["value"], json!(["client", "metadata"]));
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["reason"], "agent_tool_denied");
    }

    #[test]
    fn preserves_existing_policy_original_when_client_policy_collides() {
        let mut metadata = json!({
            "policy": { "allowed": true, "reason": "client-forged" },
            "policy_original": { "reason": "already-present" },
            "safe": "value"
        });
        let decision = denied_policy_decision();

        attach_policy_decision_to_metadata(&mut metadata, &decision);

        assert_eq!(metadata["safe"], "value");
        assert_eq!(metadata["policy_original"]["reason"], "already-present");
        assert_eq!(
            metadata["policy_client_original"]["reason"],
            "client-forged"
        );
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["reason"], "agent_tool_denied");
    }

    #[test]
    fn finds_unused_policy_original_key_without_overwriting_client_metadata() {
        let mut metadata = json!({
            "policy": { "allowed": true, "reason": "client-forged" },
            "policy_original": { "reason": "already-present" },
            "policy_client_original": { "reason": "also-present" },
            "safe": "value"
        });
        let decision = denied_policy_decision();

        attach_policy_decision_to_metadata(&mut metadata, &decision);

        assert_eq!(metadata["safe"], "value");
        assert_eq!(metadata["policy_original"]["reason"], "already-present");
        assert_eq!(metadata["policy_client_original"]["reason"], "also-present");
        assert_eq!(
            metadata["policy_client_original_1"]["reason"],
            "client-forged"
        );
        assert_eq!(metadata["policy"]["allowed"], false);
        assert_eq!(metadata["policy"]["reason"], "agent_tool_denied");
    }

    #[test]
    fn attached_policy_excludes_verbose_decision_fields() {
        let mut metadata = json!({});
        let decision = AgentPolicyDecision {
            reason_message: "Tool blocked.".to_string(),
            violations: vec![AgentPolicyIssueDto {
                field: "tool_name".to_string(),
                reason: "not_allowed".to_string(),
                blocked_by: "agent.policy.tool".to_string(),
                reason_message: "Tool not allowed.".to_string(),
                actual: "shell".to_string(),
                expected: vec!["search".to_string()],
                actual_value: Some(2.0),
                expected_value: Some(1.0),
                unit: "count".to_string(),
                operator: "lte".to_string(),
            }],
            warnings: vec![AgentPolicyIssueDto {
                field: "model".to_string(),
                reason: "expensive".to_string(),
                blocked_by: "agent.policy.model".to_string(),
                reason_message: "Model is expensive.".to_string(),
                actual: "gpt-4".to_string(),
                expected: vec!["gpt-4o-mini".to_string()],
                actual_value: None,
                expected_value: None,
                unit: String::new(),
                operator: String::new(),
            }],
            ..denied_policy_decision()
        };

        attach_policy_decision_to_metadata(&mut metadata, &decision);

        let policy = metadata["policy"].as_object().expect("policy object");
        assert!(!policy.contains_key("reason_message"));
        assert!(!policy.contains_key("violations"));
        assert!(!policy.contains_key("warnings"));
    }

    #[tokio::test]
    async fn validate_agent_policy_allows_when_policy_disabled() {
        let mut config = Config::default();
        config.policy.enabled = false;
        config.policy.on_unavailable = OnUnavailable::Deny;
        let app = crate::app::build_test_app(config).await.expect("build app");
        let auth = default_auth_context();
        let envelope = envelope(json!({}));

        let decision = validate_agent_policy(&app.state, &auth, &envelope)
            .await
            .unwrap();

        assert!(decision.allowed);
        assert_eq!(decision.reason, POLICY_DISABLED_ALLOWED_REASON);
    }

    #[tokio::test]
    async fn validate_agent_policy_denies_missing_client_even_when_policy_unavailable_allows() {
        let mut config = Config::default();
        config.policy.enabled = true;
        config.policy.on_unavailable = OnUnavailable::Allow;
        let app = crate::app::build_test_app(config).await.expect("build app");
        let auth = default_auth_context();
        let envelope = envelope(json!({}));

        let err = validate_agent_policy(&app.state, &auth, &envelope)
            .await
            .unwrap_err();

        assert_eq!(err, AgentPolicyError::Unavailable);
    }

    #[tokio::test]
    async fn validate_agent_policy_denies_missing_client_when_denied() {
        let mut config = Config::default();
        config.policy.enabled = true;
        config.policy.on_unavailable = OnUnavailable::Deny;
        let app = crate::app::build_test_app(config).await.expect("build app");
        let auth = default_auth_context();
        let envelope = envelope(json!({}));

        let err = validate_agent_policy(&app.state, &auth, &envelope)
            .await
            .unwrap_err();

        assert_eq!(err, AgentPolicyError::Unavailable);
    }

    fn denied_policy_decision() -> AgentPolicyDecision {
        AgentPolicyDecision {
            event_id: "evt-1".to_string(),
            event_type: "tool.call".to_string(),
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            allowed: false,
            policy_decision: "denied".to_string(),
            policy_stage: "audit_only".to_string(),
            sink_status: String::new(),
            reason: "agent_tool_denied".to_string(),
            blocked_by: "agent.policy.tool".to_string(),
            route_hint: "stop".to_string(),
            snapshot_revision: 123,
            reason_message: "Tool blocked.".to_string(),
            policy_id: "policy-1".to_string(),
            policy_scope: "AGENT_POLICY_SCOPE_AGENT".to_string(),
            violations: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn default_auth_context() -> AuthContext {
        let workspace_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let virtual_key_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let department_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        auth_context(workspace_id, virtual_key_id, department_id)
    }

    fn auth_context(workspace_id: Uuid, virtual_key_id: Uuid, department_id: Uuid) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(workspace_id),
            workspace_type: None,
            virtual_key_id: Some(virtual_key_id),
            virtual_key_prefix: "vk-test".to_string(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: None,
            department_id,
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            registered_agent_name: None,
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }

    fn envelope(metadata: serde_json::Value) -> AgentEventEnvelope {
        AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-1".to_string(),
            event_type: "tool.call".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at: Utc::now(),
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "ignored".to_string(),
            virtual_key_id: None,
            agent_id_external: Some("coding-agent".to_string()),
            agent_uid: None,
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::High,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata,
        }
    }
}
