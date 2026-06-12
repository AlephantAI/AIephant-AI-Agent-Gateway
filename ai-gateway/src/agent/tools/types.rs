use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolListRequest {
    pub source: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub run_id: Option<String>,
    pub framework: Option<String>,
    #[serde(default, alias = "responseMode")]
    pub response_mode: Option<String>,
    #[serde(default, alias = "includeUnavailable")]
    pub include_unavailable: bool,
    #[serde(default, alias = "adapterCapabilities")]
    pub adapter_capabilities: serde_json::Value,
    pub capabilities: serde_json::Value,
    pub metadata: serde_json::Value,
}

impl Default for ToolListRequest {
    fn default() -> Self {
        Self {
            source: None,
            agent_id: None,
            agent_name: None,
            run_id: None,
            framework: None,
            response_mode: None,
            include_unavailable: false,
            adapter_capabilities: serde_json::json!({}),
            capabilities: serde_json::json!({}),
            metadata: serde_json::json!({}),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolListResponse {
    pub snapshot_revision: u64,
    pub policy_revision: u64,
    pub snapshot_source: String,
    pub tools: Vec<ToolDescriptor>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub tool_id: String,
    pub framework_tool_name: String,
    pub metadata: ToolDescriptorMetadata,
    pub upstream_tool_name: String,
    pub name_sanitization_version: String,
    pub mapping_revision: u64,
    pub snapshot_revision: u64,
    pub policy_revision: u64,
    pub display_name: String,
    pub safe_model_description: String,
    pub name: String,
    pub description: String,
    pub tool_version: u64,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub schema_version: String,
    pub schema_hash: String,
    pub risk_level: String,
    pub approval_mode: String,
    pub timeout_ms: u64,
    pub availability: ToolAvailability,
    pub cost_policy: ToolCostPolicy,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptorMetadata {
    pub target_kind: String,
    pub target_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_card_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolAvailability {
    pub catalog_status: String,
    pub policy_preview: String,
    pub runtime_status: String,
    pub visibility: String,
    pub source: String,
    pub reason_code: String,
    pub reason: String,
    pub may_become_available: bool,
    pub remediation: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCostPolicy {
    pub pricing_type: String,
    pub fixed_micros: u64,
    pub source: String,
    pub currency: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolCallRequest {
    pub source: Option<String>,
    pub agent_id: Option<String>,
    #[serde(default, alias = "agentName")]
    pub agent_name: Option<String>,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub parent_step_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_execution_id: Option<String>,
    pub parallel_group_id: Option<String>,
    pub tool_id: String,
    pub arguments: serde_json::Value,
    #[serde(default, alias = "snapshotRevision")]
    pub snapshot_revision: Option<i64>,
    #[serde(default, alias = "schemaHash")]
    pub schema_hash: Option<String>,
    #[serde(default, alias = "toolVersion")]
    pub tool_version: Option<i64>,
    #[serde(default, alias = "targetHash")]
    pub target_hash: Option<String>,
    #[serde(default, alias = "targetRevision")]
    pub target_revision: Option<i64>,
    pub timeout_ms: Option<u64>,
    pub idempotency_key: Option<String>,
}

impl Default for ToolCallRequest {
    fn default() -> Self {
        Self {
            source: None,
            agent_id: None,
            agent_name: None,
            run_id: None,
            step_id: None,
            parent_step_id: None,
            tool_call_id: None,
            tool_execution_id: None,
            parallel_group_id: None,
            tool_id: String::new(),
            arguments: serde_json::json!({}),
            snapshot_revision: None,
            schema_hash: None,
            tool_version: None,
            target_hash: None,
            target_revision: None,
            timeout_ms: None,
            idempotency_key: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolCallResponse {
    pub status: ToolExecutionStatus,
    pub tool_call_id: Option<String>,
    pub tool_execution_id: String,
    pub output: serde_json::Value,
    pub error: Option<ToolExecutionErrorEnvelope>,
    pub gateway_metadata: Option<ToolGatewayMetadata>,
    pub billing: ToolBillingOverride,
    pub cost: ToolCost,
    pub policy: ToolPolicySummary,
    pub events: ToolExecutionEvents,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Completed,
    Denied,
    Blocked,
    Failed,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionErrorEnvelope {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolBillingOverride {
    pub reason: String,
    pub billable: bool,
    pub cost_micros: u64,
    pub currency: String,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolGatewayMetadata {
    pub execution_source: String,
    pub target_kind: String,
    pub target_id: String,
    pub target_hash: String,
    pub auth_revision: String,
    pub cache_hit: bool,
    pub reinitialized: bool,
    pub protocol_version: Option<String>,
    pub sse_used: bool,
    pub failure_class: Option<String>,
    pub blocked_before_dispatch: bool,
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_card_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolCost {
    pub amount_micros: u64,
    pub currency: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolPolicySummary {
    pub allowed: bool,
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct ToolExecutionEvents {
    pub started_event_id: Option<String>,
    pub completed_event_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ToolCallRequest, ToolListRequest};

    #[test]
    fn tool_call_request_accepts_camel_case_agent_name() {
        let request: ToolCallRequest = serde_json::from_value(serde_json::json!({
            "agentName": "External Bot",
            "tool_id": "support.echo",
            "arguments": {}
        }))
        .expect("tool call request");

        assert_eq!(request.agent_name.as_deref(), Some("External Bot"));
    }

    #[test]
    fn tool_call_request_accepts_guard_fields_with_camel_case_aliases() {
        let request: ToolCallRequest = serde_json::from_value(serde_json::json!({
            "tool_id": "support.echo",
            "arguments": {},
            "snapshotRevision": 7,
            "schemaHash": "sha256:input",
            "toolVersion": 3
        }))
        .expect("tool call request");

        assert_eq!(request.snapshot_revision, Some(7));
        assert_eq!(request.schema_hash.as_deref(), Some("sha256:input"));
        assert_eq!(request.tool_version, Some(3));
    }

    #[test]
    fn tool_call_request_accepts_openapi_target_guard_fields() {
        let request: ToolCallRequest = serde_json::from_value(serde_json::json!({
            "tool_id": "support.echo",
            "arguments": {},
            "targetHash": "sha256:target",
            "targetRevision": 5
        }))
        .expect("tool call request");

        assert_eq!(request.target_hash.as_deref(), Some("sha256:target"));
        assert_eq!(request.target_revision, Some(5));
    }

    #[test]
    fn tool_list_request_accepts_adapter_fields() {
        let request: ToolListRequest = serde_json::from_value(serde_json::json!({
            "agent_id": "agent-1",
            "framework": "langgraph",
            "responseMode": "agent_tool_compatible",
            "includeUnavailable": true,
            "adapterCapabilities": {"supportsApprovalResume": true}
        }))
        .expect("tool list request");

        assert_eq!(request.framework.as_deref(), Some("langgraph"));
        assert_eq!(
            request.response_mode.as_deref(),
            Some("agent_tool_compatible")
        );
        assert!(request.include_unavailable);
        assert_eq!(request.adapter_capabilities["supportsApprovalResume"], true);
    }
}
