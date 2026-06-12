use crate::policy_proto::{AgentPolicyDecisionKind, ValidateAgentPolicyResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicyDecisionKind {
    Allow,
    Deny,
    Block,
    ApprovalRequired,
    AuditWarning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPolicyDecision {
    pub kind: ToolPolicyDecisionKind,
    pub reason: String,
    pub blocked_by: String,
    pub policy_id: String,
    pub policy_revision: i64,
    pub approval_request_id: Option<String>,
    pub approval_status_id: Option<String>,
    pub resume_token: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ToolPolicyError {
    #[error("unknown tool policy decision")]
    UnknownDecision,
    #[error("approval-required tool policy decision is missing approval fields")]
    ApprovalFieldsMissing,
}

impl ToolPolicyDecision {
    pub fn from_response(
        response: ValidateAgentPolicyResponse,
        enforce: bool,
    ) -> Result<Self, ToolPolicyError> {
        let decision = AgentPolicyDecisionKind::try_from(response.decision)
            .map_err(|_| ToolPolicyError::UnknownDecision)?;
        let kind = match decision {
            AgentPolicyDecisionKind::Allow => ToolPolicyDecisionKind::Allow,
            AgentPolicyDecisionKind::Deny => ToolPolicyDecisionKind::Deny,
            AgentPolicyDecisionKind::Block => ToolPolicyDecisionKind::Block,
            AgentPolicyDecisionKind::ApprovalRequired => ToolPolicyDecisionKind::ApprovalRequired,
            AgentPolicyDecisionKind::AuditWarning => ToolPolicyDecisionKind::AuditWarning,
            AgentPolicyDecisionKind::Unspecified if !enforce && response.allowed => {
                ToolPolicyDecisionKind::Allow
            }
            AgentPolicyDecisionKind::Unspecified => {
                return Err(ToolPolicyError::UnknownDecision);
            }
        };

        let approval_request_id = trimmed_option(response.approval_request_id);
        if matches!(kind, ToolPolicyDecisionKind::ApprovalRequired) && approval_request_id.is_none()
        {
            return Err(ToolPolicyError::ApprovalFieldsMissing);
        }

        Ok(Self {
            kind,
            reason: response.reason,
            blocked_by: response.blocked_by,
            policy_id: response.policy_id,
            policy_revision: response.policy_revision,
            approval_request_id,
            approval_status_id: trimmed_option(response.approval_status_id),
            resume_token: trimmed_option(response.resume_token),
            expires_at: trimmed_option(response.expires_at),
        })
    }
}

fn trimmed_option(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
