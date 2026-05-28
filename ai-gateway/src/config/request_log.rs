//! Request/response log transport config (HTTP vs Redis Stream); unrelated to
//! cache Redis.

use serde::{Deserialize, Serialize};
use url::Url;

/// Default stream key aligned with collector consumption when
/// `QUEUE_PROVIDER=redis`.
pub const DEFAULT_REQUEST_RESPONSE_STREAM_KEY: &str =
    "lc:stream:alephant-request-response-logs";

#[derive(
    Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum RequestLogTransport {
    Http,
    #[default]
    Redis,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RequestLogConfig {
    #[serde(default)]
    pub transport: RequestLogTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_queue_redis_url: Option<Url>,
    #[serde(default = "default_stream_key")]
    pub request_response_stream_key: String,
}

fn default_stream_key() -> String {
    DEFAULT_REQUEST_RESPONSE_STREAM_KEY.to_string()
}

impl Default for RequestLogConfig {
    fn default() -> Self {
        Self {
            transport: RequestLogTransport::default(),
            log_queue_redis_url: None,
            request_response_stream_key: default_stream_key(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_request_log_defaults() {
        let parsed: RequestLogConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.transport, RequestLogTransport::Redis);
        assert!(parsed.log_queue_redis_url.is_none());
        assert_eq!(
            parsed.request_response_stream_key,
            "lc:stream:alephant-request-response-logs"
        );
    }

    #[test]
    fn deserializes_request_log_kebab_yaml() {
        let yaml = r#"
transport: http
log-queue-redis-url: "redis://127.0.0.1:6380/0"
request-response-stream-key: "custom:stream"
"#;
        let parsed: RequestLogConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(parsed.transport, RequestLogTransport::Http);
        assert!(parsed.log_queue_redis_url.is_some());
        assert_eq!(parsed.request_response_stream_key, "custom:stream");
    }
}
