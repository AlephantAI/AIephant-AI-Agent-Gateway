use crate::agent::tools::types::{
    ToolBillingOverride, ToolExecutionErrorEnvelope, ToolExecutionStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenApiOutcomeStatus {
    HttpStatus(u16),
    SchemaInvalid,
    PolicyBlocked,
    EgressBlocked,
    SnapshotStale,
    Timeout,
    RequestTooLarge,
    ResponseTooLarge,
    InvalidJsonResponse,
    InternalError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiOutcomeInput {
    pub status: OpenApiOutcomeStatus,
    pub fixed_micros: u64,
    pub currency: String,
    pub charge_on_failure: bool,
    pub tool_execution_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiOutcomeDecision {
    pub status: ToolExecutionStatus,
    pub error: Option<ToolExecutionErrorEnvelope>,
    pub billing: ToolBillingOverride,
    pub executed: bool,
    pub failure_stage: String,
    pub billing_status: String,
    pub billing_reason: String,
}

pub fn decide(input: OpenApiOutcomeInput) -> OpenApiOutcomeDecision {
    let dedupe_key = format!("tool_execution:{}", input.tool_execution_id);
    let currency = input.currency;

    match input.status {
        OpenApiOutcomeStatus::HttpStatus(status) if (200..=299).contains(&status) => decision(
            ToolExecutionStatus::Completed,
            None,
            billing(
                true,
                input.fixed_micros,
                currency,
                dedupe_key,
                "openapi_2xx",
            ),
            true,
            "",
            "actual",
            "openapi_2xx",
        ),
        OpenApiOutcomeStatus::HttpStatus(status) if (400..=499).contains(&status) => {
            let reason = if input.charge_on_failure {
                "openapi_4xx_per_call"
            } else {
                "openapi_4xx_waived"
            };
            decision(
                ToolExecutionStatus::Failed,
                Some(error("openapi_http_4xx", "OpenAPI upstream returned 4xx")),
                billing(
                    input.charge_on_failure,
                    billable_cost(input.charge_on_failure, input.fixed_micros),
                    currency,
                    dedupe_key,
                    reason,
                ),
                true,
                "upstream",
                if input.charge_on_failure {
                    "billable"
                } else {
                    "waived"
                },
                reason,
            )
        }
        OpenApiOutcomeStatus::HttpStatus(status) if (500..=599).contains(&status) => {
            let reason = if input.charge_on_failure {
                "openapi_5xx_per_call"
            } else {
                "openapi_5xx_waived"
            };
            decision(
                ToolExecutionStatus::Failed,
                Some(error("openapi_http_5xx", "OpenAPI upstream returned 5xx")),
                billing(
                    input.charge_on_failure,
                    billable_cost(input.charge_on_failure, input.fixed_micros),
                    currency,
                    dedupe_key,
                    reason,
                ),
                true,
                "upstream",
                if input.charge_on_failure {
                    "billable"
                } else {
                    "waived"
                },
                reason,
            )
        }
        OpenApiOutcomeStatus::HttpStatus(_) => decision(
            ToolExecutionStatus::Failed,
            Some(error(
                "openapi_unexpected_http_status",
                "OpenAPI upstream returned unexpected HTTP status",
            )),
            billing(false, 0, currency, dedupe_key, "unexpected_http_status"),
            true,
            "upstream",
            "waived",
            "unexpected_http_status",
        ),
        OpenApiOutcomeStatus::SchemaInvalid => waived_failure(
            ToolExecutionStatus::Failed,
            currency,
            dedupe_key,
            "schema",
            "openapi_schema_invalid",
            "schema_invalid",
            "OpenAPI request schema validation failed",
            false,
        ),
        OpenApiOutcomeStatus::PolicyBlocked => waived_failure(
            ToolExecutionStatus::Blocked,
            currency,
            dedupe_key,
            "policy",
            "openapi_policy_blocked",
            "policy_blocked",
            "OpenAPI tool call blocked by policy",
            false,
        ),
        OpenApiOutcomeStatus::EgressBlocked => waived_failure(
            ToolExecutionStatus::Blocked,
            currency,
            dedupe_key,
            "egress",
            "openapi_egress_blocked",
            "egress_blocked",
            "OpenAPI egress blocked",
            false,
        ),
        OpenApiOutcomeStatus::SnapshotStale => waived_failure(
            ToolExecutionStatus::Failed,
            currency,
            dedupe_key,
            "snapshot",
            "openapi_snapshot_stale",
            "snapshot_stale",
            "OpenAPI target snapshot is stale",
            false,
        ),
        OpenApiOutcomeStatus::Timeout => decision(
            ToolExecutionStatus::Timeout,
            Some(error("openapi_timeout", "OpenAPI upstream timed out")),
            billing(false, 0, currency, dedupe_key, "timeout"),
            true,
            "timeout",
            "waived",
            "timeout",
        ),
        OpenApiOutcomeStatus::RequestTooLarge => waived_failure(
            ToolExecutionStatus::Failed,
            currency,
            dedupe_key,
            "request",
            "openapi_request_too_large",
            "request_too_large",
            "OpenAPI request exceeded size limit",
            false,
        ),
        OpenApiOutcomeStatus::ResponseTooLarge => waived_failure(
            ToolExecutionStatus::Failed,
            currency,
            dedupe_key,
            "response",
            "openapi_response_too_large",
            "response_too_large",
            "OpenAPI response exceeded size limit",
            true,
        ),
        OpenApiOutcomeStatus::InvalidJsonResponse => waived_failure(
            ToolExecutionStatus::Failed,
            currency,
            dedupe_key,
            "response",
            "openapi_invalid_json_response",
            "invalid_json_response",
            "OpenAPI response was not valid JSON",
            true,
        ),
        OpenApiOutcomeStatus::InternalError => waived_failure(
            ToolExecutionStatus::Failed,
            currency,
            dedupe_key,
            "internal",
            "openapi_internal_error",
            "internal_error",
            "OpenAPI gateway internal error",
            false,
        ),
    }
}

fn waived_failure(
    status: ToolExecutionStatus,
    currency: String,
    dedupe_key: String,
    failure_stage: &str,
    code: &str,
    billing_reason: &str,
    message: &str,
    executed: bool,
) -> OpenApiOutcomeDecision {
    decision(
        status,
        Some(error(code, message)),
        billing(false, 0, currency, dedupe_key, billing_reason),
        executed,
        failure_stage,
        "waived",
        billing_reason,
    )
}

fn decision(
    status: ToolExecutionStatus,
    error: Option<ToolExecutionErrorEnvelope>,
    billing: ToolBillingOverride,
    executed: bool,
    failure_stage: &str,
    billing_status: &str,
    billing_reason: &str,
) -> OpenApiOutcomeDecision {
    OpenApiOutcomeDecision {
        status,
        error,
        billing,
        executed,
        failure_stage: failure_stage.to_string(),
        billing_status: billing_status.to_string(),
        billing_reason: billing_reason.to_string(),
    }
}

fn billing(
    billable: bool,
    cost_micros: u64,
    currency: String,
    dedupe_key: String,
    reason: &str,
) -> ToolBillingOverride {
    ToolBillingOverride {
        reason: reason.to_string(),
        billable,
        cost_micros,
        currency,
        dedupe_key,
    }
}

const fn billable_cost(billable: bool, fixed_micros: u64) -> u64 {
    if billable { fixed_micros } else { 0 }
}

fn error(code: &str, message: &str) -> ToolExecutionErrorEnvelope {
    ToolExecutionErrorEnvelope {
        code: code.to_string(),
        message: message.to_string(),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::types::ToolExecutionStatus;

    #[test]
    fn completed_2xx_is_billable_actual() {
        let decision = decide(OpenApiOutcomeInput {
            status: OpenApiOutcomeStatus::HttpStatus(201),
            fixed_micros: 4200,
            currency: "USD".to_string(),
            charge_on_failure: false,
            tool_execution_id: "exec-1".to_string(),
        });

        assert_eq!(decision.status, ToolExecutionStatus::Completed);
        assert_eq!(decision.error, None);
        assert_eq!(decision.billing.cost_micros, 4200);
        assert_eq!(decision.billing.currency, "USD");
        assert_eq!(decision.billing.dedupe_key, "tool_execution:exec-1");
        assert!(decision.billing.billable);
        assert_eq!(decision.executed, true);
        assert_eq!(decision.failure_stage, "");
        assert_eq!(decision.billing_status, "actual");
        assert_eq!(decision.billing_reason, "openapi_2xx");
    }

    #[test]
    fn policy_blocked_is_waived() {
        let decision = decide(OpenApiOutcomeInput {
            status: OpenApiOutcomeStatus::PolicyBlocked,
            fixed_micros: 4200,
            currency: "USD".to_string(),
            charge_on_failure: true,
            tool_execution_id: "exec-policy".to_string(),
        });

        assert_eq!(decision.status, ToolExecutionStatus::Blocked);
        assert_eq!(
            decision.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_policy_blocked")
        );
        assert!(!decision.billing.billable);
        assert_eq!(decision.billing.cost_micros, 0);
        assert_eq!(decision.billing.dedupe_key, "tool_execution:exec-policy");
        assert_eq!(decision.executed, false);
        assert_eq!(decision.failure_stage, "policy");
        assert_eq!(decision.billing_status, "waived");
        assert_eq!(decision.billing.reason, "policy_blocked");
        assert_eq!(decision.billing_reason, "policy_blocked");
    }

    #[test]
    fn upstream_4xx_can_charge_per_call() {
        let decision = decide(OpenApiOutcomeInput {
            status: OpenApiOutcomeStatus::HttpStatus(404),
            fixed_micros: 99,
            currency: "USD".to_string(),
            charge_on_failure: true,
            tool_execution_id: "exec-404".to_string(),
        });

        assert_eq!(decision.status, ToolExecutionStatus::Failed);
        assert_eq!(
            decision.error.as_ref().map(|error| error.code.as_str()),
            Some("openapi_http_4xx")
        );
        assert!(decision.billing.billable);
        assert_eq!(decision.billing.cost_micros, 99);
        assert_eq!(decision.billing.dedupe_key, "tool_execution:exec-404");
        assert_eq!(decision.executed, true);
        assert_eq!(decision.failure_stage, "upstream");
        assert_eq!(decision.billing_status, "billable");
        assert_eq!(decision.billing_reason, "openapi_4xx_per_call");
    }
}
