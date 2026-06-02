use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::types::secret::Secret;

pub const DEFAULT_X402_LOG_STREAM_KEY: &str = "lc:stream:x402_payment:log";
pub const DEFAULT_X402_POLICY_BODY_MAX_BYTES: usize = 1024 * 1024;

fn default_log_stream_key() -> String {
    DEFAULT_X402_LOG_STREAM_KEY.to_string()
}

fn default_payment_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_policy_body_max_bytes() -> usize {
    DEFAULT_X402_POLICY_BODY_MAX_BYTES
}

fn default_payment_service_key() -> Secret<String> {
    Secret::from(String::new())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct X402Config {
    pub enabled: bool,
    pub payment_grpc_endpoint: String,
    #[serde(default = "default_log_stream_key")]
    pub log_stream_key: String,
    pub request_body_log_max_bytes: usize,
    #[serde(default = "default_policy_body_max_bytes")]
    pub request_body_policy_max_bytes: usize,
    #[serde(with = "humantime_serde", default = "default_payment_timeout")]
    pub payment_timeout: Duration,
    #[serde(default = "default_payment_service_key")]
    pub payment_service_key: Secret<String>,
}

impl Default for X402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            payment_grpc_endpoint: "http://127.0.0.1:9091".to_string(),
            log_stream_key: default_log_stream_key(),
            request_body_log_max_bytes: 0,
            request_body_policy_max_bytes: default_policy_body_max_bytes(),
            payment_timeout: default_payment_timeout(),
            payment_service_key: default_payment_service_key(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let cfg = X402Config::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.payment_grpc_endpoint, "http://127.0.0.1:9091");
        assert_eq!(cfg.log_stream_key, "lc:stream:x402_payment:log");
        assert_eq!(cfg.request_body_log_max_bytes, 0);
        assert_eq!(cfg.request_body_policy_max_bytes, 1024 * 1024);
        assert_eq!(cfg.payment_timeout, Duration::from_secs(5));
        assert_eq!(cfg.payment_service_key.expose(), "");
    }

    #[test]
    fn deserializes_kebab_case() {
        let yaml = r#"
enabled: true
payment-grpc-endpoint: "http://127.0.0.1:9191"
log-stream-key: "custom:x402"
request-body-log-max-bytes: 128
request-body-policy-max-bytes: 2048
payment-timeout: "7s"
payment-service-key: "yaml-payment-key"
"#;
        let cfg: X402Config = serde_yml::from_str(yaml).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.payment_grpc_endpoint, "http://127.0.0.1:9191");
        assert_eq!(cfg.log_stream_key, "custom:x402");
        assert_eq!(cfg.request_body_log_max_bytes, 128);
        assert_eq!(cfg.request_body_policy_max_bytes, 2048);
        assert_eq!(cfg.payment_timeout, Duration::from_secs(7));
        assert_eq!(cfg.payment_service_key.expose(), "yaml-payment-key");
    }
}
