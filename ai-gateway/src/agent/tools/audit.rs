use chrono::Utc;
use uuid::Uuid;

use super::types::{ToolCallRequest, ToolCallResponse, ToolCost, ToolExecutionStatus};
use crate::{
    agent::{
        context::{
            AgentConfidence, AgentContext, AgentEventPhase, AgentEventSourceTrust, AgentPolicyMode,
            AgentPolicyStage, AgentStepKind, AgentStepSource, AgentTrustLevel,
        },
        event::{AgentEventEnvelope, AgentEventSource},
        name::resolve_agent_name,
    },
    types::extensions::AuthContext,
};

pub fn tool_execution_completed_event(
    auth: &AuthContext,
    header_context: Option<&AgentContext>,
    request: &ToolCallRequest,
    response: &ToolCallResponse,
) -> AgentEventEnvelope {
    tool_execution_completed_event_with_optional_sequence(
        auth,
        header_context,
        request,
        response,
        None,
    )
}

pub fn tool_execution_completed_event_with_sequence(
    auth: &AuthContext,
    header_context: Option<&AgentContext>,
    request: &ToolCallRequest,
    response: &ToolCallResponse,
    sequence: u64,
) -> AgentEventEnvelope {
    tool_execution_completed_event_with_optional_sequence(
        auth,
        header_context,
        request,
        response,
        Some(sequence),
    )
}

fn tool_execution_completed_event_with_optional_sequence(
    auth: &AuthContext,
    header_context: Option<&AgentContext>,
    request: &ToolCallRequest,
    response: &ToolCallResponse,
    sequence: Option<u64>,
) -> AgentEventEnvelope {
    base_tool_event(
        auth,
        header_context,
        request,
        "tool.result.received",
        AgentEventPhase::After,
        AgentPolicyStage::AuditOnly,
        Some(AgentStepKind::ToolResult),
        sequence,
        tool_execution_metadata(request, response),
    )
}

pub fn tool_call_requested_event(
    auth: &AuthContext,
    header_context: Option<&AgentContext>,
    request: &ToolCallRequest,
    tool_execution_id: &str,
    sequence: u64,
    operation_metadata: serde_json::Value,
) -> AgentEventEnvelope {
    let mut metadata = serde_json::json!({
        "status": "requested",
        "severity": "info",
        "message": "agent tool call requested",
        "tool_id": request.tool_id,
        "tool_name": request.tool_id,
        "toolExecutionId": tool_execution_id,
        "tool_execution_id": tool_execution_id,
        "toolCallId": request.tool_call_id,
        "executed": false,
        "failureStage": "",
    });
    merge_object_metadata(&mut metadata, operation_metadata);

    base_tool_event(
        auth,
        header_context,
        request,
        "tool.call.requested",
        AgentEventPhase::Before,
        AgentPolicyStage::PreAction,
        Some(AgentStepKind::ToolCall),
        Some(sequence),
        metadata,
    )
}

pub fn tool_policy_blocked_event(
    auth: &AuthContext,
    header_context: Option<&AgentContext>,
    request: &ToolCallRequest,
    tool_execution_id: &str,
    sequence: u64,
    reason: &str,
) -> AgentEventEnvelope {
    base_tool_event(
        auth,
        header_context,
        request,
        "tool.policy.blocked",
        AgentEventPhase::Before,
        AgentPolicyStage::PreAction,
        Some(AgentStepKind::ToolCall),
        Some(sequence),
        serde_json::json!({
            "status": "blocked",
            "severity": "error",
            "message": "agent tool call blocked by policy",
            "tool_id": request.tool_id,
            "tool_name": request.tool_id,
            "toolExecutionId": tool_execution_id,
            "toolCallId": request.tool_call_id,
            "executed": false,
            "failureStage": "policy",
            "failureCode": reason,
            "costStage": "waived",
        }),
    )
}

pub fn base_tool_event(
    auth: &AuthContext,
    header_context: Option<&AgentContext>,
    request: &ToolCallRequest,
    event_type: &str,
    event_phase: AgentEventPhase,
    policy_stage: AgentPolicyStage,
    step_kind: Option<AgentStepKind>,
    sequence: Option<u64>,
    metadata: serde_json::Value,
) -> AgentEventEnvelope {
    let observed_at = Utc::now();
    let agent_is_auth_bound =
        auth.entity_type.eq_ignore_ascii_case("agent") && !auth.entity_id.is_nil();
    let resolved_agent_name = resolve_agent_name(
        auth.registered_agent_name.as_deref(),
        request.agent_name.as_deref(),
        header_context.and_then(|ctx| ctx.agent_name.as_deref()),
    );
    let agent_trust_level = if agent_is_auth_bound {
        AgentTrustLevel::AuthBound
    } else {
        resolved_agent_name
            .trust_level
            .unwrap_or(AgentTrustLevel::SelfReported)
    };
    let agent_id_external = first_nonempty_owned(
        request.agent_id.as_deref(),
        header_context.and_then(|ctx| ctx.agent_id_external.as_deref()),
    );
    let run_id = first_nonempty_owned(
        request.run_id.as_deref(),
        header_context.and_then(|ctx| ctx.run_id.as_deref()),
    );
    let step_id = first_nonempty_owned(
        request.step_id.as_deref(),
        header_context.and_then(|ctx| ctx.step_id.as_deref()),
    );
    let parent_step_id = first_nonempty_owned(
        request.parent_step_id.as_deref(),
        header_context.and_then(|ctx| ctx.parent_step_id.as_deref()),
    );
    let tool_call_id = first_nonempty_owned(
        request.tool_call_id.as_deref(),
        header_context.and_then(|ctx| ctx.tool_call_id.as_deref()),
    );

    AgentEventEnvelope {
        version: "2026-06-05".to_string(),
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        event_type: event_type.to_string(),
        event_source: AgentEventSource::Alephant,
        event_phase,
        policy_stage,
        policy_mode: AgentPolicyMode::Audit,
        event_source_trust: AgentEventSourceTrust::GatewayExecuted,
        sequence,
        observed_at,
        timestamp: Some(observed_at),
        name: Some(request.tool_id.clone()),
        alephant_agent_name: resolved_agent_name.name,
        alephant_agent_name_source: resolved_agent_name.source.map(str::to_string),
        alephant_agent_trust_level: Some(agent_trust_level.as_str().to_string()),
        workspace_id: auth.org_id.to_string(),
        virtual_key_id: auth.virtual_key_id,
        agent_id_external,
        agent_uid: agent_is_auth_bound.then_some(auth.entity_id),
        run_id,
        step_id,
        parent_step_id,
        tool_call_id,
        handoff_id: None,
        graph_node: None,
        step_kind,
        step_source: AgentStepSource::Gateway,
        step_confidence: AgentConfidence::High,
        trust_level: agent_trust_level,
        context_conflict: false,
        step_id_conflict: false,
        attempt: None,
        input_hash: None,
        metadata,
        billing_mirror_trusted: true,
    }
}

fn first_nonempty_owned(first: Option<&str>, second: Option<&str>) -> Option<String> {
    first
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| second.map(str::trim).filter(|value| !value.is_empty()))
        .map(str::to_string)
}

fn tool_execution_metadata(
    request: &ToolCallRequest,
    response: &ToolCallResponse,
) -> serde_json::Value {
    let billing_status = response
        .gateway_metadata
        .as_ref()
        .and_then(|metadata| metadata.billing_status.as_deref())
        .map(canonical_billing_status)
        .unwrap_or_else(|| tool_billing_status(response));
    let billing_reason = response
        .gateway_metadata
        .as_ref()
        .and_then(|metadata| metadata.billing_reason.as_ref())
        .map_or_else(
            || {
                if billing_status == "settled" {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(response.billing.reason.clone())
                }
            },
            |reason| serde_json::Value::String(reason.clone()),
        );

    serde_json::json!({
        "status": response.status.as_str(),
        "severity": "info",
        "message": "agent tool execution completed",
        "tool_id": request.tool_id,
        "tool_name": request.tool_id,
        "toolExecutionId": response.tool_execution_id,
        "toolCallId": response.tool_call_id,
        "toolCostMicros": response.cost.amount_micros,
        "toolCostCurrency": response.cost.currency,
        "cost": micros_to_units(&response.cost),
        "policy": {
            "allowed": response.policy.allowed,
            "decision": response.policy.decision,
            "reason": response.policy.reason,
        },
        "gateway": gateway_tool_metadata(response),
        "billing": {
            "version": "2026-06-09",
            "costType": "tool",
            "costSubtype": "tool",
            "status": billing_status,
            "billable": response.billing.billable,
            "amountMicros": response.billing.cost_micros,
            "costMicros": response.billing.cost_micros,
            "currency": response.billing.currency,
            "pricingSource": response.cost.source,
            "pricingRevision": 0,
            "dedupeKey": response.billing.dedupe_key,
            "observedKind": "runtime_confirmed",
            "chargeOnFailure": false,
            "reason": billing_reason,
        },
    })
}

fn gateway_tool_metadata(response: &ToolCallResponse) -> serde_json::Value {
    let Some(gateway) = response.gateway_metadata.as_ref() else {
        return serde_json::json!({});
    };

    let mut metadata = serde_json::json!({
        "targetKind": gateway.target_kind,
        "executionSource": gateway.execution_source,
        "targetId": gateway.target_id,
        "targetHash": gateway.target_hash,
        "authRevision": gateway.auth_revision,
        "cacheHit": gateway.cache_hit,
        "reinitialized": gateway.reinitialized,
        "protocolVersion": gateway.protocol_version,
        "sseUsed": gateway.sse_used,
        "failureClass": gateway.failure_class,
        "blockedBeforeDispatch": gateway.blocked_before_dispatch,
        "latencyMs": gateway.latency_ms,
    });

    insert_optional(&mut metadata, "serviceSlug", &gateway.service_slug);
    insert_optional(&mut metadata, "operationId", &gateway.operation_id);
    insert_optional(&mut metadata, "operationSlug", &gateway.operation_slug);
    insert_optional(&mut metadata, "httpMethod", &gateway.http_method);
    insert_optional_value(&mut metadata, "httpStatus", gateway.http_status);
    insert_optional_value(&mut metadata, "requestBytes", gateway.request_bytes);
    insert_optional_value(&mut metadata, "responseBytes", gateway.response_bytes);
    insert_optional_value(&mut metadata, "targetRevision", gateway.target_revision);
    insert_optional(&mut metadata, "schemaHash", &gateway.schema_hash);
    insert_optional_value(
        &mut metadata,
        "rateCardRevision",
        gateway.rate_card_revision,
    );
    insert_optional(&mut metadata, "billingStatus", &gateway.billing_status);
    insert_optional(&mut metadata, "billingReason", &gateway.billing_reason);
    insert_optional_value(&mut metadata, "executed", gateway.executed);
    insert_optional(&mut metadata, "failureStage", &gateway.failure_stage);

    metadata
}

fn insert_optional(metadata: &mut serde_json::Value, key: &'static str, value: &Option<String>) {
    if let Some(value) = value {
        metadata[key] = serde_json::Value::String(value.clone());
    }
}

fn insert_optional_value<T>(metadata: &mut serde_json::Value, key: &'static str, value: Option<T>)
where
    T: Into<serde_json::Value>,
{
    if let Some(value) = value {
        metadata[key] = value.into();
    }
}

fn merge_object_metadata(metadata: &mut serde_json::Value, extra: serde_json::Value) {
    let (Some(metadata), Some(extra)) = (metadata.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (key, value) in extra {
        metadata.insert(key.clone(), value.clone());
    }
}

fn micros_to_units(cost: &ToolCost) -> f64 {
    cost.amount_micros as f64 / 1_000_000.0
}

fn tool_billing_status(response: &ToolCallResponse) -> &'static str {
    match response.status {
        ToolExecutionStatus::Completed if response.cost.amount_micros > 0 => "settled",
        ToolExecutionStatus::Completed
        | ToolExecutionStatus::Denied
        | ToolExecutionStatus::Blocked
        | ToolExecutionStatus::Failed => "waived",
        ToolExecutionStatus::Timeout => "pending",
    }
}

fn canonical_billing_status(status: &str) -> &str {
    match status {
        "actual" | "billable" => "settled",
        status => status,
    }
}

impl ToolExecutionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Denied => "denied",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::tools::types::{
            ToolBillingOverride, ToolExecutionErrorEnvelope, ToolGatewayMetadata, ToolPolicySummary,
        },
        types::{extensions::AuthContext, org::OrgId, secret::Secret, user::UserId},
    };

    #[test]
    fn requested_event_uses_tool_call_requested_type() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: Some("exec-1".to_string()),
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };

        let event =
            tool_call_requested_event(&auth, None, &request, "exec-1", 7, serde_json::json!({}));

        assert_eq!(event.event_type, "tool.call.requested");
        assert_eq!(event.event_phase, AgentEventPhase::Before);
        assert_eq!(event.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(event.sequence, Some(7));
        assert_eq!(event.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(event.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(event.metadata["toolExecutionId"], "exec-1");
        assert_eq!(event.metadata["status"], "requested");
        assert_eq!(event.metadata["executed"], false);
        assert_eq!(event.metadata["failureStage"], "");
    }

    #[test]
    fn blocked_event_is_terminal_and_not_executed() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };

        let event =
            tool_policy_blocked_event(&auth, None, &request, "exec-1", 8, "audit_unavailable");

        assert_eq!(event.event_type, "tool.policy.blocked");
        assert_eq!(event.event_phase, AgentEventPhase::Before);
        assert_eq!(event.policy_stage, AgentPolicyStage::PreAction);
        assert_eq!(event.sequence, Some(8));
        assert_eq!(event.step_kind, Some(AgentStepKind::ToolCall));
        assert_eq!(event.metadata["toolExecutionId"], "exec-1");
        assert_eq!(event.metadata["status"], "blocked");
        assert_eq!(event.metadata["executed"], false);
        assert_eq!(event.metadata["failureStage"], "policy");
        assert_eq!(event.metadata["failureCode"], "audit_unavailable");
        assert_eq!(event.metadata["costStage"], "waived");
    }

    #[test]
    fn tool_execution_event_maps_to_agent_event_envelope() {
        let mut auth = auth_context(Uuid::new_v4());
        auth.registered_agent_name = Some("Support Bot".to_string());
        let request = ToolCallRequest {
            agent_id: Some("external-agent".to_string()),
            run_id: Some("run-1".to_string()),
            step_id: Some("step-2".to_string()),
            tool_call_id: Some("call-1".to_string()),
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Completed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: None,
            gateway_metadata: None,
            billing: ToolBillingOverride {
                reason: "success".to_string(),
                billable: true,
                cost_micros: 4200,
                currency: "USD".to_string(),
                dedupe_key: "exec-1".to_string(),
            },
            cost: ToolCost {
                amount_micros: 4200,
                currency: "USD".to_string(),
                source: "rate_card".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: Default::default(),
        };

        let event = tool_execution_completed_event(&auth, None, &request, &response);

        assert!(event.event_id.starts_with("evt_"));
        assert_eq!(event.event_type, "tool.result.received");
        assert_eq!(event.sequence, None);
        assert_eq!(event.event_phase, AgentEventPhase::After);
        assert_eq!(event.step_kind, Some(AgentStepKind::ToolResult));
        assert_eq!(event.step_source, AgentStepSource::Gateway);
        assert_eq!(event.agent_id_external.as_deref(), Some("external-agent"));
        assert_eq!(event.run_id.as_deref(), Some("run-1"));
        assert_eq!(event.step_id.as_deref(), Some("step-2"));
        assert_eq!(event.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(event.alephant_agent_name.as_deref(), Some("Support Bot"));
        assert_eq!(
            event.alephant_agent_name_source.as_deref(),
            Some("virtual_key_label")
        );
        assert_eq!(event.metadata["tool_id"], "support.echo");
        assert_eq!(event.metadata["tool_name"], "support.echo");
        assert_eq!(event.metadata["toolExecutionId"], "exec-1");
        assert_eq!(event.metadata["toolCallId"], "call-1");
        assert_eq!(event.metadata["toolCostMicros"], 4200);
        assert_eq!(event.metadata["toolCostCurrency"], "USD");
        assert_eq!(event.metadata["cost"], 0.0042);
        assert_eq!(event.metadata["policy"]["allowed"], true);
        assert_eq!(event.metadata["billing"]["version"], "2026-06-09");
        assert_eq!(event.metadata["billing"]["costType"], "tool");
        assert_eq!(event.metadata["billing"]["costSubtype"], "tool");
        assert_eq!(event.metadata["billing"]["status"], "settled");
        assert_eq!(event.metadata["billing"]["billable"], true);
        assert_eq!(event.metadata["billing"]["amountMicros"], 4200);
        assert_eq!(event.metadata["billing"]["currency"], "USD");
        assert_eq!(event.metadata["billing"]["pricingSource"], "rate_card");
        assert_eq!(event.metadata["billing"]["pricingRevision"], 0);
        assert_eq!(event.metadata["billing"]["dedupeKey"], "exec-1");
        assert_eq!(
            event.metadata["billing"]["observedKind"],
            "runtime_confirmed"
        );
        assert_eq!(event.metadata["billing"]["chargeOnFailure"], false);
        assert!(event.metadata["billing"]["reason"].is_null());
    }

    #[test]
    fn failed_tool_execution_event_marks_billing_waived() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Failed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: None,
            gateway_metadata: None,
            billing: ToolBillingOverride {
                reason: "failed".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "exec-1".to_string(),
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
            events: Default::default(),
        };

        let event = tool_execution_completed_event(&auth, None, &request, &response);

        assert_eq!(event.metadata["status"], "failed");
        assert_eq!(event.metadata["toolCostMicros"], 0);
        assert_eq!(event.metadata["toolCostCurrency"], "USD");
        assert_eq!(event.metadata["cost"], 0.0);
        assert_eq!(event.metadata["billing"]["costSubtype"], "tool");
        assert_eq!(event.metadata["billing"]["status"], "waived");
        assert_eq!(event.metadata["billing"]["billable"], false);
        assert_eq!(event.metadata["billing"]["amountMicros"], 0);
        assert_eq!(event.metadata["billing"]["reason"], "failed");
    }

    #[test]
    fn timeout_tool_execution_event_marks_billing_pending() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Timeout,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: None,
            gateway_metadata: None,
            billing: ToolBillingOverride {
                reason: "timeout".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "exec-1".to_string(),
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
            events: Default::default(),
        };

        let event = tool_execution_completed_event(&auth, None, &request, &response);

        assert_eq!(event.metadata["status"], "timeout");
        assert_eq!(event.metadata["toolCostMicros"], 0);
        assert_eq!(event.metadata["toolCostCurrency"], "USD");
        assert_eq!(event.metadata["cost"], 0.0);
        assert_eq!(event.metadata["billing"]["costSubtype"], "tool");
        assert_eq!(event.metadata["billing"]["status"], "pending");
        assert_eq!(event.metadata["billing"]["billable"], false);
        assert_eq!(event.metadata["billing"]["amountMicros"], 0);
        assert_eq!(event.metadata["billing"]["reason"], "timeout");
    }

    #[test]
    fn streamable_http_gateway_metadata_does_not_leak_session_or_auth() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Failed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({
                "error": {
                    "code": "mcp_sse_parse_error",
                    "retryable": false,
                    "message": "stream parse failed"
                }
            }),
            error: Some(ToolExecutionErrorEnvelope {
                code: "mcp_sse_parse_error".to_string(),
                message: "stream parse failed".to_string(),
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
                latency_ms: Some(42),
                ..ToolGatewayMetadata::default()
            }),
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "waived".to_string(),
            },
            billing: ToolBillingOverride {
                reason: "mcp_sse_parse_error".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "run-1:step-1:call-1:exec-1".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: Default::default(),
        };

        let event = tool_execution_completed_event(&auth, None, &request, &response);
        let metadata_text = event.metadata.to_string();
        let output_text = response.output.to_string();

        assert_eq!(
            event.metadata["gateway"]["targetKind"],
            "mcp-streamable-http"
        );
        assert_eq!(event.metadata["gateway"]["targetId"], "docs.search");
        assert_eq!(
            event.metadata["gateway"]["failureClass"],
            "mcp_sse_parse_error"
        );
        assert_eq!(event.metadata["gateway"]["sseUsed"], true);
        assert_eq!(event.metadata["gateway"]["latencyMs"], 42);
        assert_eq!(event.metadata["billing"]["status"], "waived");
        assert_eq!(event.metadata["billing"]["billable"], false);
        assert_eq!(event.metadata["billing"]["amountMicros"], 0);
        assert_eq!(event.metadata["billing"]["costMicros"], 0);
        assert_eq!(event.metadata["billing"]["reason"], "mcp_sse_parse_error");
        assert!(!output_text.contains("must-not-leak"));
        assert!(!output_text.contains("sessionId"));
        assert!(!output_text.contains("Mcp-Session-Id"));
        assert!(!output_text.contains("authorization"));
        assert!(!output_text.contains("rawSse"));
        assert!(!output_text.contains("upstream"));
        assert!(!metadata_text.contains("must-not-leak"));
        assert!(!metadata_text.contains("sessionId"));
        assert!(!metadata_text.contains("Mcp-Session-Id"));
        assert!(!metadata_text.contains("authorization"));
        assert!(!metadata_text.contains("rawSse"));
        assert!(!metadata_text.contains("upstream"));
    }

    #[test]
    fn mcp_sse_terminal_event_does_not_leak_endpoint_or_session() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_id: "docs.search".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Timeout,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({
                "error": {
                    "code": "mcp_sse_idle_timeout",
                    "retryable": true,
                    "message": "MCP SSE target timed out"
                }
            }),
            error: Some(ToolExecutionErrorEnvelope {
                code: "mcp_sse_idle_timeout".to_string(),
                message: "MCP SSE target timed out".to_string(),
                retryable: true,
            }),
            gateway_metadata: Some(ToolGatewayMetadata {
                execution_source: "gateway_executed".to_string(),
                target_kind: "mcp-sse".to_string(),
                target_id: "docs.search".to_string(),
                target_hash: "sha256:abc".to_string(),
                auth_revision: "0/static".to_string(),
                cache_hit: false,
                reinitialized: false,
                protocol_version: Some("2025-06-18".to_string()),
                sse_used: true,
                failure_class: Some("mcp_sse_idle_timeout".to_string()),
                blocked_before_dispatch: false,
                latency_ms: Some(250),
                billing_status: Some("waived".to_string()),
                billing_reason: Some("timeout".to_string()),
                executed: Some(true),
                failure_stage: Some("runtime".to_string()),
                ..ToolGatewayMetadata::default()
            }),
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "waived".to_string(),
            },
            billing: ToolBillingOverride {
                reason: "timeout".to_string(),
                billable: false,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "tool_execution:exec-1".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: Default::default(),
        };

        let event =
            tool_execution_completed_event_with_sequence(&auth, None, &request, &response, 2);
        let metadata = event.metadata.to_string();

        assert!(metadata.contains("mcp-sse"));
        assert!(metadata.contains("sha256:abc"));
        assert!(!metadata.contains("Authorization"));
        assert!(!metadata.contains("Cookie"));
        assert!(!metadata.contains("/message"));
        assert!(!metadata.contains("session"));
    }

    #[test]
    fn openapi_completed_event_includes_gateway_and_billing_metadata() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_id: "billing.create_invoice".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Completed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-openapi".to_string(),
            output: serde_json::json!({"ok": true}),
            error: None,
            gateway_metadata: Some(ToolGatewayMetadata {
                execution_source: "gateway_executed".to_string(),
                target_kind: "openapi".to_string(),
                target_id: "billing.create_invoice".to_string(),
                target_hash: "sha256:target".to_string(),
                auth_revision: "0/static".to_string(),
                cache_hit: false,
                reinitialized: false,
                protocol_version: None,
                sse_used: false,
                failure_class: Some("none".to_string()),
                blocked_before_dispatch: false,
                latency_ms: Some(37),
                service_slug: Some("billing".to_string()),
                operation_id: Some("createInvoice".to_string()),
                operation_slug: Some("create-invoice".to_string()),
                http_method: Some("POST".to_string()),
                http_status: Some(201),
                request_bytes: Some(128),
                response_bytes: Some(256),
                target_revision: Some(17),
                schema_hash: Some("sha256:schema".to_string()),
                rate_card_revision: Some(23),
                billing_status: Some("actual".to_string()),
                billing_reason: Some("openapi_2xx".to_string()),
                executed: Some(true),
                failure_stage: Some("".to_string()),
            }),
            cost: ToolCost {
                amount_micros: 4200,
                currency: "USD".to_string(),
                source: "rate_card".to_string(),
            },
            billing: ToolBillingOverride {
                reason: "openapi_2xx".to_string(),
                billable: true,
                cost_micros: 4200,
                currency: "USD".to_string(),
                dedupe_key: "tool_execution:exec-openapi".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: Default::default(),
        };

        let event = tool_execution_completed_event(&auth, None, &request, &response);

        assert_eq!(event.metadata["gateway"]["targetKind"], "openapi");
        assert_eq!(event.metadata["gateway"]["serviceSlug"], "billing");
        assert_eq!(event.metadata["gateway"]["operationId"], "createInvoice");
        assert_eq!(event.metadata["gateway"]["httpStatus"], 201);
        assert_eq!(event.metadata["gateway"]["latencyMs"], 37);
        assert_eq!(event.metadata["gateway"]["failureClass"], "none");
        assert_eq!(event.metadata["gateway"]["targetHash"], "sha256:target");
        assert_eq!(event.metadata["gateway"]["schemaHash"], "sha256:schema");
        assert_eq!(event.metadata["gateway"]["rateCardRevision"], 23);
        assert_eq!(event.metadata["gateway"]["billingStatus"], "actual");
        assert_eq!(event.metadata["billing"]["status"], "settled");
        assert_eq!(event.metadata["billing"]["reason"], "openapi_2xx");
        assert_eq!(
            event.metadata["billing"]["dedupeKey"],
            "tool_execution:exec-openapi"
        );
    }

    #[test]
    fn completed_event_with_sequence_uses_timeline_order() {
        let auth = auth_context(Uuid::new_v4());
        let request = ToolCallRequest {
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: Some("exec-1".to_string()),
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Completed,
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: None,
            gateway_metadata: None,
            billing: ToolBillingOverride {
                reason: "success".to_string(),
                billable: true,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "exec-1".to_string(),
            },
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "rate_card".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: Default::default(),
        };

        let requested =
            tool_call_requested_event(&auth, None, &request, "exec-1", 1, serde_json::json!({}));
        let completed =
            tool_execution_completed_event_with_sequence(&auth, None, &request, &response, 2);

        assert_eq!(requested.sequence, Some(1));
        assert_eq!(completed.sequence, Some(2));
        assert_eq!(completed.event_type, "tool.result.received");
        assert_eq!(completed.metadata["toolExecutionId"], "exec-1");
    }

    #[test]
    fn tool_execution_event_falls_back_to_header_agent_context() {
        let auth = auth_context(Uuid::new_v4());
        let header_context = AgentContext {
            agent_id_external: Some("header-agent".to_string()),
            agent_name: Some("Header Bot".to_string()),
            run_id: Some("header-run".to_string()),
            step_id: Some("header-step".to_string()),
            parent_step_id: Some("header-parent".to_string()),
            tool_call_id: Some("header-call".to_string()),
            ..AgentContext::default()
        };
        let request = ToolCallRequest {
            tool_id: "support.echo".to_string(),
            ..ToolCallRequest::default()
        };
        let response = ToolCallResponse {
            status: ToolExecutionStatus::Completed,
            tool_call_id: Some("response-call".to_string()),
            tool_execution_id: "exec-1".to_string(),
            output: serde_json::json!({}),
            error: None,
            gateway_metadata: None,
            billing: ToolBillingOverride {
                reason: "success".to_string(),
                billable: true,
                cost_micros: 0,
                currency: "USD".to_string(),
                dedupe_key: "exec-1".to_string(),
            },
            cost: ToolCost {
                amount_micros: 0,
                currency: "USD".to_string(),
                source: "rate_card".to_string(),
            },
            policy: ToolPolicySummary {
                allowed: true,
                decision: "allowed".to_string(),
                reason: "tool_allowed".to_string(),
            },
            events: Default::default(),
        };

        let event =
            tool_execution_completed_event(&auth, Some(&header_context), &request, &response);

        assert_eq!(event.agent_id_external.as_deref(), Some("header-agent"));
        assert_eq!(event.run_id.as_deref(), Some("header-run"));
        assert_eq!(event.step_id.as_deref(), Some("header-step"));
        assert_eq!(event.parent_step_id.as_deref(), Some("header-parent"));
        assert_eq!(event.tool_call_id.as_deref(), Some("header-call"));
        assert_eq!(event.alephant_agent_name.as_deref(), Some("Header Bot"));
        assert_eq!(
            event.alephant_agent_name_source.as_deref(),
            Some("self_reported_header")
        );
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
}
