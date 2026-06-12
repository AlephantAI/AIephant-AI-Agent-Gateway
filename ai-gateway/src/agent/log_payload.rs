use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    agent::{
        context::{AgentEventSourceTrust, AgentStepSource, AgentTrustLevel},
        event::AgentEventEnvelope,
    },
    types::extensions::AuthContext,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventLogPayload {
    pub version: String,
    pub event_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub virtual_key_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entity_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entity_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entity_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_agent_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_agent_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_agent_name_source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_agent_trust_level: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_run_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_step_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_parent_step_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alephant_graph_node: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handoff_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_cost_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_cost_currency: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_cost_source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_cost_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_cost_subtype: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_amount_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_billable: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_dedupe_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target_kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service_slug: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operation_id: String,
    pub event_type: String,
    pub event_source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event_phase: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_stage: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_mode: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event_source_trust: String,
    pub observed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub step_kind: String,
    pub step_source: String,
    pub step_confidence: String,
    pub agent_trust_level: String,
    pub context_conflict: bool,
    pub step_id_conflict: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub severity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_allowed: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_decision: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_blocked_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_snapshot_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sink_status: String,
    pub metadata: String,
}

impl From<&AgentEventEnvelope> for AgentEventLogPayload {
    fn from(envelope: &AgentEventEnvelope) -> Self {
        Self::from_envelope(envelope)
    }
}

impl AgentEventLogPayload {
    #[must_use]
    pub fn from_envelope(envelope: &AgentEventEnvelope) -> Self {
        let metadata = &envelope.metadata;
        let policy = metadata.get("policy");
        let trusted_gateway_tool_event = is_trusted_gateway_tool_event(envelope);
        let tool_billing = tool_billing_mirror(envelope, metadata);
        let operation_metadata = operation_metadata_mirror(metadata, trusted_gateway_tool_event);

        Self {
            version: envelope.version.clone(),
            event_id: envelope.event_id.clone(),
            workspace_id: envelope.workspace_id.clone(),
            workspace_type: String::new(),
            user_id: String::new(),
            virtual_key_id: envelope
                .virtual_key_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            entity_type: String::new(),
            entity_id: String::new(),
            entity_name: String::new(),
            alephant_agent_id: envelope
                .agent_uid
                .map(|id| id.to_string())
                .or_else(|| envelope.agent_id_external.clone())
                .unwrap_or_default(),
            alephant_agent_name: envelope.alephant_agent_name.clone().unwrap_or_default(),
            alephant_agent_name_source: envelope
                .alephant_agent_name_source
                .clone()
                .unwrap_or_default(),
            alephant_agent_trust_level: envelope
                .alephant_agent_trust_level
                .clone()
                .unwrap_or_default(),
            alephant_run_id: envelope.run_id.clone().unwrap_or_default(),
            alephant_step_id: envelope.step_id.clone().unwrap_or_default(),
            alephant_parent_step_id: envelope.parent_step_id.clone().unwrap_or_default(),
            alephant_graph_node: envelope.graph_node.clone().unwrap_or_default(),
            tool_call_id: envelope.tool_call_id.clone().unwrap_or_default(),
            handoff_id: envelope.handoff_id.clone().unwrap_or_default(),
            tool_name: metadata_string(metadata, "tool_name")
                .or_non_empty()
                .unwrap_or_else(|| envelope.name.clone().unwrap_or_default()),
            tool_execution_id: tool_billing
                .tool_execution_id
                .or_non_empty()
                .or_else(|| {
                    trusted_gateway_tool_event
                        .then(|| metadata_string(metadata, "toolExecutionId").or_non_empty())
                        .flatten()
                })
                .unwrap_or_default(),
            tool_cost_micros: tool_billing.tool_cost_micros,
            tool_cost_currency: tool_billing.tool_cost_currency,
            tool_cost_source: tool_billing.tool_cost_source,
            billing_cost_type: tool_billing.billing_cost_type,
            billing_cost_subtype: tool_billing.billing_cost_subtype,
            billing_status: tool_billing.billing_status,
            billing_reason: tool_billing.billing_reason,
            billing_amount_micros: tool_billing.billing_amount_micros,
            billing_currency: tool_billing.billing_currency,
            billing_billable: tool_billing.billing_billable,
            billing_dedupe_key: tool_billing.billing_dedupe_key,
            target_kind: operation_metadata.target_kind,
            service_slug: operation_metadata.service_slug,
            operation_id: operation_metadata.operation_id,
            event_type: envelope.event_type.clone(),
            event_source: envelope.event_source.as_str().to_string(),
            event_phase: envelope.event_phase.as_str().to_string(),
            policy_stage: envelope.policy_stage.as_str().to_string(),
            policy_mode: envelope.policy_mode.as_str().to_string(),
            event_source_trust: envelope.event_source_trust.as_str().to_string(),
            observed_at: envelope.observed_at,
            event_time: Some(envelope.timestamp.unwrap_or(envelope.observed_at)),
            sequence: envelope.sequence,
            step_kind: envelope
                .step_kind
                .map(|kind| kind.as_str().to_string())
                .unwrap_or_default(),
            step_source: envelope.step_source.as_str().to_string(),
            step_confidence: envelope.step_confidence.as_str().to_string(),
            agent_trust_level: envelope.trust_level.as_str().to_string(),
            context_conflict: envelope.context_conflict,
            step_id_conflict: envelope.step_id_conflict,
            attempt: envelope.attempt,
            input_hash: envelope.input_hash.clone().unwrap_or_default(),
            status: metadata_string(metadata, "status"),
            severity: metadata_string(metadata, "severity"),
            message: metadata_string(metadata, "message"),
            duration_ms: metadata_u64(metadata, "duration_ms"),
            latency_ms: metadata_u64(metadata, "latency_ms"),
            request_id: metadata_string(metadata, "request_id"),
            model: metadata_string(metadata, "model"),
            provider: metadata_string(metadata, "provider"),
            cost: metadata_f64(metadata, "cost"),
            policy_allowed: policy
                .and_then(|value| value.get("allowed"))
                .and_then(|value| value.as_bool()),
            policy_decision: policy_decision_string(metadata, policy),
            policy_reason: nested_metadata_string(policy, "reason"),
            policy_blocked_by: nested_metadata_string(policy, "blocked_by"),
            policy_id: nested_metadata_string(policy, "policy_id"),
            policy_scope: nested_metadata_string(policy, "policy_scope"),
            policy_snapshot_revision: policy
                .and_then(|value| value.get("snapshot_revision"))
                .and_then(|value| value.as_i64()),
            sink_status: metadata_string(metadata, "sinkStatus"),
            metadata: serde_json::to_string(metadata).unwrap_or_else(|_| "{}".to_string()),
        }
    }

    #[must_use]
    pub fn from_envelope_with_auth(envelope: &AgentEventEnvelope, auth_ctx: &AuthContext) -> Self {
        let mut payload = Self::from_envelope(envelope);
        payload.workspace_type = auth_ctx.workspace_type.clone().unwrap_or_default();
        payload.user_id = auth_ctx.user_id.to_string();
        payload.entity_type = auth_ctx.entity_type.clone();
        payload.entity_id = if auth_ctx.entity_id.is_nil() {
            String::new()
        } else {
            auth_ctx.entity_id.to_string()
        };
        payload.entity_name = auth_ctx.entity_name.clone();
        if auth_ctx.entity_type.eq_ignore_ascii_case("agent") && !auth_ctx.entity_id.is_nil() {
            payload.alephant_agent_id = auth_ctx.entity_id.to_string();
            payload.agent_trust_level = AgentTrustLevel::AuthBound.as_str().to_string();
        }
        payload
    }
}

#[derive(Default)]
struct ToolBillingMirror {
    tool_execution_id: String,
    tool_cost_micros: Option<u64>,
    tool_cost_currency: String,
    tool_cost_source: String,
    billing_cost_type: String,
    billing_cost_subtype: String,
    billing_status: String,
    billing_reason: String,
    billing_amount_micros: Option<u64>,
    billing_currency: String,
    billing_billable: Option<bool>,
    billing_dedupe_key: String,
}

#[derive(Default)]
struct OperationMetadataMirror {
    target_kind: String,
    service_slug: String,
    operation_id: String,
}

fn operation_metadata_mirror(
    metadata: &serde_json::Value,
    trusted_gateway_tool_event: bool,
) -> OperationMetadataMirror {
    if !trusted_gateway_tool_event {
        return OperationMetadataMirror::default();
    }

    let gateway = metadata.get("gateway").filter(|value| value.is_object());

    OperationMetadataMirror {
        target_kind: metadata_string(metadata, "targetKind")
            .or_non_empty()
            .or_else(|| nested_metadata_string(gateway, "targetKind").or_non_empty())
            .unwrap_or_default(),
        service_slug: metadata_string(metadata, "serviceSlug")
            .or_non_empty()
            .or_else(|| nested_metadata_string(gateway, "serviceSlug").or_non_empty())
            .unwrap_or_default(),
        operation_id: metadata_string(metadata, "operationId")
            .or_non_empty()
            .or_else(|| nested_metadata_string(gateway, "operationId").or_non_empty())
            .unwrap_or_default(),
    }
}

fn tool_billing_mirror(
    envelope: &AgentEventEnvelope,
    metadata: &serde_json::Value,
) -> ToolBillingMirror {
    if !is_gateway_billing_tool_event(envelope) {
        return ToolBillingMirror::default();
    }

    let tool_execution_id = metadata_string(metadata, "toolExecutionId");
    if tool_execution_id.is_empty() {
        return ToolBillingMirror::default();
    }

    let Some(billing) = metadata.get("billing").filter(|value| value.is_object()) else {
        return ToolBillingMirror::default();
    };

    ToolBillingMirror {
        tool_execution_id,
        tool_cost_micros: metadata_u64(metadata, "toolCostMicros"),
        tool_cost_currency: metadata_string(metadata, "toolCostCurrency"),
        tool_cost_source: metadata_string(billing, "pricingSource"),
        billing_cost_type: metadata_string(billing, "costType"),
        billing_cost_subtype: metadata_string(billing, "costSubtype"),
        billing_status: metadata_string(billing, "status"),
        billing_reason: metadata_string(billing, "reason"),
        billing_amount_micros: metadata_u64(billing, "amountMicros"),
        billing_currency: metadata_string(billing, "currency"),
        billing_billable: metadata_bool(billing, "billable"),
        billing_dedupe_key: metadata_string(billing, "dedupeKey"),
    }
}

fn is_gateway_billing_tool_event(envelope: &AgentEventEnvelope) -> bool {
    is_trusted_gateway_tool_event(envelope)
        && matches!(
            envelope.event_type.as_str(),
            "tool.result.received"
                | "tool.call.failed"
                | "tool.call.timeout"
                | "tool.policy.denied"
                | "tool.policy.blocked"
                | "tool.approval.rejected"
                | "tool.approval.expired"
                | "tool.call.replayed"
        )
}

fn is_trusted_gateway_tool_event(envelope: &AgentEventEnvelope) -> bool {
    envelope.billing_mirror_trusted
        && envelope.step_source == AgentStepSource::Gateway
        && matches!(
            envelope.event_source_trust,
            AgentEventSourceTrust::GatewayObserved | AgentEventSourceTrust::GatewayExecuted
        )
}

trait NonEmptyString {
    fn or_non_empty(self) -> Option<String>;
}

impl NonEmptyString for String {
    fn or_non_empty(self) -> Option<String> {
        if self.is_empty() { None } else { Some(self) }
    }
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> String {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn nested_metadata_string(metadata: Option<&serde_json::Value>, key: &str) -> String {
    metadata
        .and_then(|value| value.get(key))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn policy_decision_string(
    metadata: &serde_json::Value,
    policy: Option<&serde_json::Value>,
) -> String {
    nested_metadata_string(policy, "decision")
        .or_non_empty()
        .or_else(|| nested_metadata_string(policy, "policyDecision").or_non_empty())
        .or_else(|| metadata_string(metadata, "policyDecision").or_non_empty())
        .unwrap_or_default()
}

fn metadata_u64(metadata: &serde_json::Value, key: &str) -> Option<u64> {
    metadata.get(key).and_then(|value| value.as_u64())
}

fn metadata_bool(metadata: &serde_json::Value, key: &str) -> Option<bool> {
    metadata.get(key).and_then(|value| value.as_bool())
}

fn metadata_f64(metadata: &serde_json::Value, key: &str) -> Option<f64> {
    metadata.get(key).and_then(|value| value.as_f64())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::agent::{
        context::{
            AgentConfidence, AgentEventPhase, AgentEventSourceTrust, AgentPolicyMode,
            AgentPolicyStage, AgentStepKind, AgentStepSource, AgentTrustLevel,
        },
        event::{AgentEventEnvelope, AgentEventSource},
    };

    #[test]
    fn serializes_complete_envelope_as_camel_case_log_payload() {
        let virtual_key_id = Uuid::from_u128(1);
        let agent_uid = Uuid::from_u128(2);
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let event_time = Utc.with_ymd_and_hms(2026, 5, 30, 11, 59, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-1".to_string(),
            event_type: "tool.call.completed".to_string(),
            event_source: AgentEventSource::LangGraph,
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at,
            timestamp: Some(event_time),
            name: Some("fallback-tool".to_string()),
            alephant_agent_name: Some("Support Bot".to_string()),
            alephant_agent_name_source: Some("virtual_key_label".to_string()),
            alephant_agent_trust_level: Some("auth_bound".to_string()),
            workspace_id: "workspace-1".to_string(),
            virtual_key_id: Some(virtual_key_id),
            agent_id_external: Some("external-agent".to_string()),
            agent_uid: Some(agent_uid),
            run_id: Some("run-1".to_string()),
            step_id: Some("step-1".to_string()),
            parent_step_id: Some("parent-step-1".to_string()),
            tool_call_id: Some("tool-call-1".to_string()),
            handoff_id: Some("handoff-1".to_string()),
            graph_node: Some("graph-node-1".to_string()),
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::High,
            trust_level: AgentTrustLevel::AuthBound,
            context_conflict: true,
            step_id_conflict: true,
            attempt: Some(2),
            input_hash: Some("input-hash-1".to_string()),
            metadata: json!({
                "tool_name": "metadata-tool",
                "status": "completed",
                "severity": "info",
                "message": "done",
                "duration_ms": 123,
                "latency_ms": 456,
                "request_id": "req-1",
                "model": "model-1",
                "provider": "provider-1",
                "cost": 0.12,
                "workspace_type": "organization",
                "user_id": "user-1",
                "entity_type": "agent",
                "entity_id": "entity-1",
                "entity_name": "Entity One",
                "policy": {
                    "allowed": false,
                    "reason": "blocked by rule",
                    "blocked_by": "tool",
                    "policy_id": "policy-1",
                    "policy_scope": "workspace",
                    "snapshot_revision": 42
                }
            }),
            billing_mirror_trusted: false,
        };

        let payload = AgentEventLogPayload::from(&envelope);
        let value = serde_json::to_value(&payload).unwrap();

        assert_eq!(value["eventId"], "evt-1");
        assert_eq!(value["version"], "2026-05-27");
        assert_eq!(value["handoffId"], "handoff-1");
        assert_eq!(value["contextConflict"], true);
        assert_eq!(value["stepIdConflict"], true);
        assert_eq!(value["workspaceId"], "workspace-1");
        assert_eq!(value["virtualKeyId"], virtual_key_id.to_string());
        for key in [
            "workspaceType",
            "userId",
            "entityType",
            "entityId",
            "entityName",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
        assert_eq!(value["alephantAgentId"], agent_uid.to_string());
        assert_eq!(value["alephantAgentName"], "Support Bot");
        assert_eq!(value["alephantAgentNameSource"], "virtual_key_label");
        assert_eq!(value["alephantAgentTrustLevel"], "auth_bound");
        assert_eq!(value["alephantRunId"], "run-1");
        assert_eq!(value["alephantStepId"], "step-1");
        assert_eq!(value["alephantParentStepId"], "parent-step-1");
        assert_eq!(value["eventType"], "tool.call.completed");
        assert_eq!(value["eventSource"], "langgraph");
        assert_eq!(value["stepKind"], "tool_call");
        assert_eq!(value["stepSource"], "runtime");
        assert_eq!(value["stepConfidence"], "high");
        assert_eq!(value["agentTrustLevel"], "auth_bound");
        assert_eq!(value["toolCallId"], "tool-call-1");
        assert_eq!(value["toolName"], "metadata-tool");
        assert_eq!(value["status"], "completed");
        assert_eq!(value["severity"], "info");
        assert_eq!(value["policyAllowed"], false);
        assert_eq!(value["policyReason"], "blocked by rule");
        assert_eq!(value["policyBlockedBy"], "tool");
        assert_eq!(value["policyId"], "policy-1");
        assert_eq!(value["policyScope"], "workspace");
        assert_eq!(value["policySnapshotRevision"], 42);
        assert_eq!(
            value["observedAt"],
            serde_json::to_value(observed_at).unwrap()
        );
        assert_eq!(
            value["eventTime"],
            serde_json::to_value(event_time).unwrap()
        );
        assert!(value.get("rawEvent").is_none());
        assert_eq!(payload.metadata, envelope.metadata.to_string());
    }

    #[test]
    fn skips_empty_optional_strings_but_keeps_required_fields() {
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-2".to_string(),
            event_type: "step.started".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-2".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: None,
            step_id: None,
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: None,
            step_source: AgentStepSource::Unknown,
            step_confidence: AgentConfidence::Unknown,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({}),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        for key in [
            "virtualKeyId",
            "alephantAgentId",
            "alephantRunId",
            "alephantStepId",
            "alephantParentStepId",
            "toolCallId",
            "toolName",
            "stepKind",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }

        for key in [
            "eventId",
            "workspaceId",
            "eventType",
            "eventSource",
            "observedAt",
            "stepSource",
            "stepConfidence",
            "agentTrustLevel",
            "metadata",
        ] {
            assert!(value.get(key).is_some(), "{key} should be present");
        }
    }

    #[test]
    fn log_payload_uses_observed_at_as_event_time_when_timestamp_missing() {
        let observed_at = Utc.with_ymd_and_hms(2026, 6, 4, 10, 0, 1).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-observed-only".to_string(),
            event_type: "tool.call.observed".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::After,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::GatewayObserved,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-observed".to_string(),
            virtual_key_id: None,
            agent_id_external: Some("agent-observed".to_string()),
            agent_uid: None,
            run_id: Some("run-observed".to_string()),
            step_id: Some("step-observed".to_string()),
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Gateway,
            step_confidence: AgentConfidence::Medium,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({}),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        assert_eq!(
            value["observedAt"],
            serde_json::to_value(observed_at).unwrap()
        );
        assert_eq!(
            value["eventTime"],
            serde_json::to_value(observed_at).unwrap()
        );
    }

    #[test]
    fn tool_name_falls_back_to_envelope_name() {
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-3".to_string(),
            event_type: "tool.call.started".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at,
            timestamp: None,
            name: Some("fallback-tool".to_string()),
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-3".to_string(),
            virtual_key_id: None,
            agent_id_external: Some("external-agent".to_string()),
            agent_uid: None,
            run_id: None,
            step_id: None,
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::Medium,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({}),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        assert_eq!(value["toolName"], "fallback-tool");
        assert_eq!(value["alephantAgentId"], "external-agent");
    }

    #[test]
    fn log_payload_includes_policy_phase_source_trust_and_sink_fields() {
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-4".to_string(),
            event_type: "tool.call.started".to_string(),
            event_source: AgentEventSource::CrewAi,
            event_phase: AgentEventPhase::Before,
            policy_stage: AgentPolicyStage::PreAction,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: Some(42),
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-4".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: None,
            step_id: None,
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: None,
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::Medium,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({
                "policy": {
                    "allowed": true,
                    "decision": "allowed"
                },
                "sinkStatus": "sent"
            }),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        assert_eq!(value["eventSource"], "crewai");
        assert_eq!(value["eventSourceTrust"], "self_reported");
        assert_eq!(value["eventPhase"], "before");
        assert_eq!(value["policyStage"], "pre_action");
        assert_eq!(value["policyMode"], "audit");
        assert_eq!(value["sequence"], 42);
        assert_eq!(value["policyDecision"], "allowed");
        assert_eq!(value["sinkStatus"], "sent");
    }

    #[test]
    fn log_payload_uses_top_level_policy_decision_fallback() {
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-5".to_string(),
            event_type: "policy.audit.unavailable".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-5".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: None,
            step_id: None,
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: None,
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::Medium,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({
                "policyDecision": "unavailable"
            }),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        assert_eq!(value["policyDecision"], "unavailable");
    }

    #[test]
    fn log_payload_uses_nested_policy_decision_fallback() {
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-6".to_string(),
            event_type: "policy.audit.completed".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::Unknown,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-6".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: None,
            step_id: None,
            parent_step_id: None,
            tool_call_id: None,
            handoff_id: None,
            graph_node: None,
            step_kind: None,
            step_source: AgentStepSource::Runtime,
            step_confidence: AgentConfidence::Medium,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({
                "policy": {
                    "allowed": true,
                    "policyDecision": "allowed"
                }
            }),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        assert_eq!(value["policyDecision"], "allowed");
    }

    #[test]
    fn gateway_tool_billing_snapshot_mirrors_top_level_fields() {
        let observed_at = Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-tool-billing".to_string(),
            event_type: "tool.result.received".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::After,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::GatewayObserved,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-tool-billing".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: Some("run-tool-billing".to_string()),
            step_id: Some("step-tool-billing".to_string()),
            parent_step_id: None,
            tool_call_id: Some("tool-call-billing".to_string()),
            handoff_id: None,
            graph_node: None,
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Gateway,
            step_confidence: AgentConfidence::High,
            trust_level: AgentTrustLevel::AuthBound,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({
                "toolExecutionId": "exec-1",
                "toolCostMicros": 1234,
                "toolCostCurrency": "USD",
                "billing": {
                    "costType": "tool",
                    "costSubtype": "tool",
                    "status": "settled",
                    "amountMicros": 1234,
                    "currency": "USD",
                    "billable": true,
                    "dedupeKey": "tool_execution:exec-1",
                    "pricingSource": "rate_card"
                }
            }),
            billing_mirror_trusted: true,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        assert_eq!(value["toolExecutionId"], "exec-1");
        assert_eq!(value["toolCostMicros"], 1234);
        assert_eq!(value["toolCostCurrency"], "USD");
        assert_eq!(value["billingCostType"], "tool");
        assert_eq!(value["billingCostSubtype"], "tool");
        assert_eq!(value["billingStatus"], "settled");
        assert_eq!(value["billingAmountMicros"], 1234);
        assert_eq!(value["billingCurrency"], "USD");
        assert_eq!(value["billingBillable"], true);
        assert_eq!(value["billingDedupeKey"], "tool_execution:exec-1");
        assert_eq!(value["toolCostSource"], "rate_card");
    }

    #[test]
    fn self_reported_tool_event_does_not_mirror_billing_fields() {
        let observed_at = Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-self-reported-tool-billing".to_string(),
            event_type: "tool.result.received".to_string(),
            event_source: AgentEventSource::LangGraph,
            event_phase: AgentEventPhase::After,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::SelfReported,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-self-reported-tool-billing".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: Some("run-self-reported-tool-billing".to_string()),
            step_id: Some("step-self-reported-tool-billing".to_string()),
            parent_step_id: None,
            tool_call_id: Some("tool-call-self-reported-billing".to_string()),
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
            metadata: json!({
                "toolExecutionId": "tool-exec-self-reported",
                "toolCostMicros": 1234,
                "toolCostCurrency": "USD",
                "billing": {
                    "costType": "tool",
                    "costSubtype": "tool",
                    "status": "settled",
                    "amountMicros": 1234,
                    "currency": "USD",
                    "billable": true,
                    "dedupeKey": "tool_execution:tool-exec-self-reported",
                    "pricingSource": "rate_card"
                }
            }),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        for key in [
            "toolExecutionId",
            "toolCostMicros",
            "toolCostCurrency",
            "billingCostType",
            "billingCostSubtype",
            "billingStatus",
            "billingAmountMicros",
            "billingCurrency",
            "billingBillable",
            "billingDedupeKey",
            "toolCostSource",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
    }

    #[test]
    fn spoofed_gateway_tool_event_does_not_mirror_billing_fields() {
        let observed_at = Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-spoofed-tool-billing".to_string(),
            event_type: "tool.result.received".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::After,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::GatewayObserved,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-spoofed-tool-billing".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: Some("run-spoofed-tool-billing".to_string()),
            step_id: Some("step-spoofed-tool-billing".to_string()),
            parent_step_id: None,
            tool_call_id: Some("tool-call-spoofed-billing".to_string()),
            handoff_id: None,
            graph_node: None,
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Gateway,
            step_confidence: AgentConfidence::High,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({
                "toolExecutionId": "tool-exec-spoofed",
                "toolCostMicros": 1234,
                "toolCostCurrency": "USD",
                "billing": {
                    "costType": "tool",
                    "costSubtype": "tool",
                    "status": "settled",
                    "amountMicros": 1234,
                    "currency": "USD",
                    "billable": true,
                    "dedupeKey": "tool_execution:tool-exec-spoofed",
                    "pricingSource": "rate_card"
                }
            }),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        for key in [
            "toolExecutionId",
            "toolCostMicros",
            "toolCostCurrency",
            "billingCostType",
            "billingCostSubtype",
            "billingStatus",
            "billingAmountMicros",
            "billingCurrency",
            "billingBillable",
            "billingDedupeKey",
            "toolCostSource",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
    }

    #[test]
    fn provider_observed_tool_event_does_not_mirror_billing_fields() {
        let observed_at = Utc.with_ymd_and_hms(2026, 6, 9, 12, 0, 0).unwrap();
        let envelope = AgentEventEnvelope {
            version: "2026-05-27".to_string(),
            event_id: "evt-provider-observed-tool-billing".to_string(),
            event_type: "tool.call.observed".to_string(),
            event_source: AgentEventSource::Alephant,
            event_phase: AgentEventPhase::After,
            policy_stage: AgentPolicyStage::AuditOnly,
            policy_mode: AgentPolicyMode::Audit,
            event_source_trust: AgentEventSourceTrust::GatewayObserved,
            sequence: None,
            observed_at,
            timestamp: None,
            name: None,
            alephant_agent_name: None,
            alephant_agent_name_source: None,
            alephant_agent_trust_level: None,
            workspace_id: "workspace-provider-observed-tool-billing".to_string(),
            virtual_key_id: None,
            agent_id_external: None,
            agent_uid: None,
            run_id: Some("run-provider-observed-tool-billing".to_string()),
            step_id: Some("step-provider-observed-tool-billing".to_string()),
            parent_step_id: None,
            tool_call_id: Some("tool-call-provider-observed-billing".to_string()),
            handoff_id: None,
            graph_node: None,
            step_kind: Some(AgentStepKind::ToolCall),
            step_source: AgentStepSource::Gateway,
            step_confidence: AgentConfidence::Medium,
            trust_level: AgentTrustLevel::SelfReported,
            context_conflict: false,
            step_id_conflict: false,
            attempt: None,
            input_hash: None,
            metadata: json!({
                "toolExecutionId": "tool-exec-provider-observed",
                "toolCostMicros": 1234,
                "toolCostCurrency": "USD",
                "billing": {
                    "costType": "tool",
                    "costSubtype": "tool",
                    "status": "settled",
                    "amountMicros": 1234,
                    "currency": "USD",
                    "billable": true,
                    "dedupeKey": "tool_execution:tool-exec-provider-observed",
                    "pricingSource": "rate_card"
                }
            }),
            billing_mirror_trusted: false,
        };

        let value = serde_json::to_value(AgentEventLogPayload::from(&envelope)).unwrap();

        for key in [
            "toolExecutionId",
            "toolCostMicros",
            "toolCostCurrency",
            "billingCostType",
            "billingCostSubtype",
            "billingStatus",
            "billingAmountMicros",
            "billingCurrency",
            "billingBillable",
            "billingDedupeKey",
            "toolCostSource",
        ] {
            assert!(value.get(key).is_none(), "{key} should be omitted");
        }
    }

    #[test]
    fn deserializes_camel_case_log_payload() {
        let observed_at = Utc.with_ymd_and_hms(2026, 5, 30, 12, 0, 0).unwrap();
        let payload: AgentEventLogPayload = serde_json::from_value(json!({
            "version": "2026-05-27",
            "eventId": "evt-deserialize",
            "workspaceId": "workspace-deserialize",
            "eventType": "step.completed",
            "eventSource": "alephant",
            "observedAt": observed_at,
            "stepSource": "runtime",
            "stepConfidence": "medium",
            "agentTrustLevel": "self_reported",
            "handoffId": "handoff-deserialize",
            "contextConflict": true,
            "stepIdConflict": true,
            "metadata": "{\"status\":\"completed\"}"
        }))
        .unwrap();

        assert_eq!(payload.event_id, "evt-deserialize");
        assert_eq!(payload.workspace_id, "workspace-deserialize");
        assert_eq!(payload.event_type, "step.completed");
        assert_eq!(payload.event_source, "alephant");
        assert_eq!(payload.event_phase, "");
        assert_eq!(payload.policy_stage, "");
        assert_eq!(payload.policy_mode, "");
        assert_eq!(payload.event_source_trust, "");
        assert_eq!(payload.observed_at, observed_at);
        assert_eq!(payload.sequence, None);
        assert_eq!(payload.step_source, "runtime");
        assert_eq!(payload.step_confidence, "medium");
        assert_eq!(payload.agent_trust_level, "self_reported");
        assert_eq!(payload.policy_decision, "");
        assert_eq!(payload.sink_status, "");
        assert_eq!(payload.metadata, "{\"status\":\"completed\"}");

        let value = serde_json::to_value(payload).unwrap();
        assert_eq!(value["version"], "2026-05-27");
        assert_eq!(value["handoffId"], "handoff-deserialize");
        assert_eq!(value["contextConflict"], true);
        assert_eq!(value["stepIdConflict"], true);
    }
}
