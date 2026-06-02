use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Timeout for one `PolicyService/Evaluate` gRPC call (fixed; not
/// configurable).
pub const POLICY_GRPC_EVALUATE_TIMEOUT: Duration = Duration::from_secs(150);

/// Backoff between background reconnect attempts when policy gRPC client is
/// missing (fixed).
pub const POLICY_GRPC_RECONNECT_INTERVAL: Duration = Duration::from_secs(5);

/// Max request body bytes sent to policy (reserved for aligning with
/// `EvaluateRequest.body` etc.).
pub const POLICY_MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq,
)]
#[serde(rename_all = "kebab-case")]
pub enum OnUnavailable {
    #[default]
    Deny,
    Allow,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct PolicyConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub grpc_endpoint: String,
    #[serde(default)]
    pub on_unavailable: OnUnavailable,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            grpc_endpoint: String::new(),
            on_unavailable: OnUnavailable::Deny,
        }
    }
}
