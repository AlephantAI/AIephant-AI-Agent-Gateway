use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Completed,
    Replayed,
    Denied,
    Blocked,
    ApprovalRequired,
    SnapshotStale,
    Failed,
    Timeout,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterResponseMode {
    AgentToolCompatible,
    #[default]
    HttpStrict,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentAction {
    None,
    RefreshTools,
    WaitForApproval,
    ChooseAlternativeTool,
    AskUser,
    RetryAfter,
    Stop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CostStage {
    Estimated,
    Reserved,
    Settled,
    Released,
    Waived,
    Billed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallCost {
    pub stage: CostStage,
    pub estimated_micros: u64,
    pub reserved_micros: u64,
    pub actual_micros: u64,
    pub currency: String,
    pub billable: bool,
    pub rate_card_revision: i64,
    pub charge_on_failure: bool,
}

impl Default for ToolCallCost {
    fn default() -> Self {
        Self {
            stage: CostStage::Waived,
            estimated_micros: 0,
            reserved_micros: 0,
            actual_micros: 0,
            currency: "USD".to_string(),
            billable: false,
            rate_card_revision: 0,
            charge_on_failure: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolPolicyEnvelope {
    pub decision: String,
    pub policy_id: String,
    pub reason: String,
    pub blocked_by: String,
    pub policy_revision: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolErrorEnvelope {
    pub code: String,
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolEventIds {
    pub requested_event_id: Option<String>,
    pub policy_event_id: Option<String>,
    pub completed_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEnvelope {
    pub status: ToolExecutionStatus,
    pub executed: bool,
    pub tool_execution_id: String,
    pub tool_id: String,
    pub run_id: Option<String>,
    pub step_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub snapshot_revision: i64,
    pub snapshot_source: String,
    pub policy_revision: i64,
    pub policy: ToolPolicyEnvelope,
    pub cost: ToolCallCost,
    pub events: ToolEventIds,
    pub agent_action: AgentAction,
    pub visible_to_user: bool,
    pub user_message: String,
    pub developer_message: String,
    pub admin_message: String,
    pub output: serde_json::Value,
    pub error: Option<ToolErrorEnvelope>,
    pub approval: Option<serde_json::Value>,
}

impl ToolCallEnvelope {
    pub fn completed(
        tool_execution_id: String,
        tool_id: String,
        output: serde_json::Value,
    ) -> Self {
        Self {
            status: ToolExecutionStatus::Completed,
            executed: true,
            tool_execution_id,
            tool_id,
            run_id: None,
            step_id: None,
            tool_call_id: None,
            snapshot_revision: 0,
            snapshot_source: String::new(),
            policy_revision: 0,
            policy: ToolPolicyEnvelope {
                decision: "allow".to_string(),
                reason: "tool_allowed".to_string(),
                ..ToolPolicyEnvelope::default()
            },
            cost: ToolCallCost::default(),
            events: ToolEventIds::default(),
            agent_action: AgentAction::None,
            visible_to_user: false,
            user_message: String::new(),
            developer_message: String::new(),
            admin_message: String::new(),
            output,
            error: None,
            approval: None,
        }
    }
}

pub fn http_status_for_response_mode(
    status: ToolExecutionStatus,
    mode: AdapterResponseMode,
) -> http::StatusCode {
    match mode {
        AdapterResponseMode::AgentToolCompatible => http::StatusCode::OK,
        AdapterResponseMode::HttpStrict => match status {
            ToolExecutionStatus::Completed | ToolExecutionStatus::Replayed => http::StatusCode::OK,
            ToolExecutionStatus::Denied | ToolExecutionStatus::Blocked => {
                http::StatusCode::FORBIDDEN
            }
            ToolExecutionStatus::ApprovalRequired => http::StatusCode::LOCKED,
            ToolExecutionStatus::SnapshotStale => http::StatusCode::CONFLICT,
            ToolExecutionStatus::Failed => http::StatusCode::BAD_GATEWAY,
            ToolExecutionStatus::Timeout => http::StatusCode::GATEWAY_TIMEOUT,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_compatible_mode_returns_200_for_business_states() {
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::ApprovalRequired,
                AdapterResponseMode::AgentToolCompatible,
            ),
            http::StatusCode::OK
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::SnapshotStale,
                AdapterResponseMode::AgentToolCompatible,
            ),
            http::StatusCode::OK
        );
    }

    #[test]
    fn strict_mode_preserves_semantic_http_errors() {
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::ApprovalRequired,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::LOCKED
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::SnapshotStale,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::CONFLICT
        );
    }

    #[test]
    fn completed_envelope_uses_none_agent_action() {
        let envelope = ToolCallEnvelope::completed(
            "exec_1".to_string(),
            "support.echo".to_string(),
            serde_json::json!({"ok": true}),
        );

        assert_eq!(envelope.status, ToolExecutionStatus::Completed);
        assert!(envelope.executed);
        assert_eq!(envelope.agent_action, AgentAction::None);
        assert_eq!(envelope.tool_execution_id, "exec_1");
        assert_eq!(envelope.cost, ToolCallCost::default());
        assert_eq!(envelope.policy.decision, "allow");
        assert_eq!(envelope.policy.reason, "tool_allowed");
        assert_eq!(envelope.events, ToolEventIds::default());
        assert!(!envelope.visible_to_user);
        assert_eq!(envelope.user_message, "");
        assert_eq!(envelope.developer_message, "");
        assert_eq!(envelope.admin_message, "");
    }

    #[test]
    fn adapter_response_mode_defaults_to_http_strict() {
        assert_eq!(
            AdapterResponseMode::default(),
            AdapterResponseMode::HttpStrict
        );
    }

    #[test]
    fn cost_default_is_waived_and_not_billable() {
        let cost = ToolCallCost::default();

        assert_eq!(cost.stage, CostStage::Waived);
        assert_eq!(cost.estimated_micros, 0);
        assert_eq!(cost.reserved_micros, 0);
        assert_eq!(cost.actual_micros, 0);
        assert_eq!(cost.currency, "USD");
        assert!(!cost.billable);
        assert!(!cost.charge_on_failure);
    }

    #[test]
    fn strict_mode_maps_all_business_states() {
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::Completed,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::OK
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::Replayed,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::OK
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::Denied,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::FORBIDDEN
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::Blocked,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::FORBIDDEN
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::Failed,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            http_status_for_response_mode(
                ToolExecutionStatus::Timeout,
                AdapterResponseMode::HttpStrict,
            ),
            http::StatusCode::GATEWAY_TIMEOUT
        );
    }
}
