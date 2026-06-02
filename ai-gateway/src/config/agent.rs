use serde::{Deserialize, Serialize};

use crate::{agent::context::AgentPolicyMode, types::secret::Secret};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentConfig {
    pub enabled: bool,
    pub event_stream_key: String,
    pub event_log_http_fallback_enabled: bool,
    /// Absolute URL, or a path joined to `alephant.log_collector_url`.
    pub event_log_http_endpoint: String,
    pub event_log_http_timeout_ms: u64,
    /// Header used for HTTP fallback auth. `authorization` values are sent
    /// with a Bearer prefix by the transport layer.
    pub event_log_http_auth_header: String,
    pub event_log_http_auth_token: Secret<String>,
    pub allow_header_context: bool,
    pub validate_agent_registry: bool,
    pub context_conflict_action: AgentConflictAction,
    pub step_conflict_action: AgentConflictAction,
    pub event_ttl_seconds: u64,
    pub max_header_value_bytes: usize,
    pub max_event_bytes: usize,
    pub max_batch_events: usize,
    pub max_metadata_bytes: usize,
    pub metadata_redaction: AgentMetadataRedaction,
    pub forward_agent_headers_upstream: bool,
    pub policy_timeout_ms: u64,
    pub policy_mode: AgentPolicyMode,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            event_stream_key: "lc:stream:alephant-agent-events".to_string(),
            event_log_http_fallback_enabled: true,
            event_log_http_endpoint: "/v1/log/agent-event".to_string(),
            event_log_http_timeout_ms: 1000,
            event_log_http_auth_header: "authorization".to_string(),
            event_log_http_auth_token: Secret::from(String::new()),
            allow_header_context: true,
            validate_agent_registry: false,
            context_conflict_action: AgentConflictAction::Warn,
            step_conflict_action: AgentConflictAction::Warn,
            event_ttl_seconds: 86_400,
            max_header_value_bytes: 256,
            max_event_bytes: 65_536,
            max_batch_events: 100,
            max_metadata_bytes: 8_192,
            metadata_redaction: AgentMetadataRedaction::Basic,
            forward_agent_headers_upstream: false,
            policy_timeout_ms: 3000,
            policy_mode: AgentPolicyMode::Audit,
        }
    }
}

#[derive(
    Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum AgentConflictAction {
    Disabled,
    #[default]
    Warn,
    Strict,
}

#[derive(
    Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum AgentMetadataRedaction {
    Disabled,
    #[default]
    Basic,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let cfg = AgentConfig::default();

        assert!(!cfg.enabled);
        assert_eq!(cfg.event_stream_key, "lc:stream:alephant-agent-events");
        assert!(cfg.event_log_http_fallback_enabled);
        assert_eq!(cfg.event_log_http_endpoint, "/v1/log/agent-event");
        assert_eq!(cfg.event_log_http_timeout_ms, 1000);
        assert_eq!(cfg.event_log_http_auth_header, "authorization");
        assert_eq!(cfg.event_log_http_auth_token.expose(), "");
        assert!(cfg.allow_header_context);
        assert!(!cfg.validate_agent_registry);
        assert_eq!(cfg.context_conflict_action, AgentConflictAction::Warn);
        assert_eq!(cfg.step_conflict_action, AgentConflictAction::Warn);
        assert_eq!(cfg.event_ttl_seconds, 86_400);
        assert_eq!(cfg.max_header_value_bytes, 256);
        assert_eq!(cfg.max_event_bytes, 65_536);
        assert_eq!(cfg.max_batch_events, 100);
        assert_eq!(cfg.max_metadata_bytes, 8_192);
        assert_eq!(cfg.metadata_redaction, AgentMetadataRedaction::Basic);
        assert!(!cfg.forward_agent_headers_upstream);
        assert_eq!(cfg.policy_timeout_ms, 3000);
        assert_eq!(cfg.policy_mode, AgentPolicyMode::Audit);
    }

    #[test]
    fn deserializes_kebab_case_config() {
        let raw = r#"
enabled: true
event-stream-key: custom:agent
event-log-http-fallback-enabled: false
event-log-http-endpoint: "http://collector.local/v1/log/agent-event"
event-log-http-timeout-ms: 750
event-log-http-auth-header: "x-alephant-internal-token"
event-log-http-auth-token: "agent-token"
allow-header-context: false
validate-agent-registry: true
context-conflict-action: strict
step-conflict-action: disabled
event-ttl-seconds: 60
max-header-value-bytes: 128
max-event-bytes: 4096
max-batch-events: 10
max-metadata-bytes: 1024
metadata-redaction: disabled
forward-agent-headers-upstream: true
policy-timeout-ms: 2500
policy-mode: enforce
"#;
        let cfg: AgentConfig = serde_yml::from_str(raw).unwrap();

        assert!(cfg.enabled);
        assert_eq!(cfg.event_stream_key, "custom:agent");
        assert!(!cfg.event_log_http_fallback_enabled);
        assert_eq!(
            cfg.event_log_http_endpoint,
            "http://collector.local/v1/log/agent-event"
        );
        assert_eq!(cfg.event_log_http_timeout_ms, 750);
        assert_eq!(cfg.event_log_http_auth_header, "x-alephant-internal-token");
        assert_eq!(cfg.event_log_http_auth_token.expose(), "agent-token");
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("agent-token"));
        let serialized = serde_yml::to_string(&cfg).unwrap();
        assert!(!serialized.contains("agent-token"));
        assert!(serialized.contains("event-log-http-auth-token: '*****'"));
        assert!(!cfg.allow_header_context);
        assert!(cfg.validate_agent_registry);
        assert_eq!(cfg.context_conflict_action, AgentConflictAction::Strict);
        assert_eq!(cfg.step_conflict_action, AgentConflictAction::Disabled);
        assert_eq!(cfg.event_ttl_seconds, 60);
        assert_eq!(cfg.max_header_value_bytes, 128);
        assert_eq!(cfg.max_event_bytes, 4096);
        assert_eq!(cfg.max_batch_events, 10);
        assert_eq!(cfg.max_metadata_bytes, 1024);
        assert_eq!(cfg.metadata_redaction, AgentMetadataRedaction::Disabled);
        assert!(cfg.forward_agent_headers_upstream);
        assert_eq!(cfg.policy_timeout_ms, 2500);
        assert_eq!(cfg.policy_mode, AgentPolicyMode::Enforce);
    }

    #[test]
    fn unknown_policy_mode_deserializes_as_audit() {
        let raw = r#"
policy-mode: future
"#;
        let cfg: AgentConfig = serde_yml::from_str(raw).unwrap();

        assert_eq!(cfg.policy_mode, AgentPolicyMode::Audit);
    }
}
