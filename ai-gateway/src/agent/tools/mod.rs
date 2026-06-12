pub mod audit;
pub mod catalog;
pub mod egress_policy;
pub mod executor;
pub mod idempotency;
pub mod mcp_http;
pub mod mcp_sse;
pub mod mcp_streamable_http;
pub mod openapi;
pub mod policy_evaluator;
pub mod response;
pub mod runtime_snapshot;
pub mod schema_validator;
pub mod service;
pub mod types;

#[cfg(test)]
mod policy_evaluator_tests {
    use crate::{
        agent::tools::policy_evaluator::{
            ToolPolicyDecision, ToolPolicyDecisionKind, ToolPolicyError,
        },
        policy_proto::{AgentPolicyDecisionKind, ValidateAgentPolicyResponse},
    };

    #[test]
    fn maps_approval_required_response() {
        let response = ValidateAgentPolicyResponse {
            decision: AgentPolicyDecisionKind::ApprovalRequired as i32,
            reason: " needs approval ".to_string(),
            blocked_by: " agent.policy.tool ".to_string(),
            policy_id: " policy-1 ".to_string(),
            policy_revision: 42,
            approval_request_id: " approval-1 ".to_string(),
            approval_status_id: " status-1 ".to_string(),
            resume_token: " resume-1 ".to_string(),
            expires_at: " 2026-06-05T12:00:00Z ".to_string(),
            ..Default::default()
        };

        let decision =
            ToolPolicyDecision::from_response(response, true).expect("approval-required decision");

        assert_eq!(decision.kind, ToolPolicyDecisionKind::ApprovalRequired);
        assert_eq!(decision.reason, " needs approval ");
        assert_eq!(decision.blocked_by, " agent.policy.tool ");
        assert_eq!(decision.policy_id, " policy-1 ");
        assert_eq!(decision.policy_revision, 42);
        assert_eq!(decision.approval_request_id.as_deref(), Some("approval-1"));
        assert_eq!(decision.approval_status_id.as_deref(), Some("status-1"));
        assert_eq!(decision.resume_token.as_deref(), Some("resume-1"));
        assert_eq!(decision.expires_at.as_deref(), Some("2026-06-05T12:00:00Z"));
    }

    #[test]
    fn unknown_decision_in_enforce_mode_fails_closed() {
        let response = ValidateAgentPolicyResponse {
            allowed: true,
            decision: AgentPolicyDecisionKind::Unspecified as i32,
            ..Default::default()
        };

        let err = ToolPolicyDecision::from_response(response, true)
            .expect_err("enforce mode must not infer allow");

        assert_eq!(err, ToolPolicyError::UnknownDecision);
    }

    #[test]
    fn invalid_numeric_decision_in_enforce_mode_fails_closed() {
        let response = ValidateAgentPolicyResponse {
            allowed: true,
            decision: 999,
            ..Default::default()
        };

        let err = ToolPolicyDecision::from_response(response, true)
            .expect_err("invalid enum numeric value must fail closed");

        assert_eq!(err, ToolPolicyError::UnknownDecision);
    }

    #[test]
    fn approval_required_without_request_id_fails() {
        for approval_request_id in ["", "   "] {
            let response = ValidateAgentPolicyResponse {
                decision: AgentPolicyDecisionKind::ApprovalRequired as i32,
                approval_request_id: approval_request_id.to_string(),
                ..Default::default()
            };

            let err = ToolPolicyDecision::from_response(response, true)
                .expect_err("approval id is required");

            assert_eq!(err, ToolPolicyError::ApprovalFieldsMissing);
        }
    }

    #[test]
    fn unspecified_allowed_in_audit_mode_is_allow() {
        let response = ValidateAgentPolicyResponse {
            allowed: true,
            decision: AgentPolicyDecisionKind::Unspecified as i32,
            ..Default::default()
        };

        let decision =
            ToolPolicyDecision::from_response(response, false).expect("audit mode fallback");

        assert_eq!(decision.kind, ToolPolicyDecisionKind::Allow);
    }
}
