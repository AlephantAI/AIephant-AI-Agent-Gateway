use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::context::AgentPolicyMode;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentConfig {
    pub enabled: bool,
    pub event_stream_key: String,
    pub event_log_http_fallback_enabled: bool,
    /// Absolute URL, or a path joined to `alephant.log_collector_url`.
    pub event_log_http_endpoint: String,
    pub event_log_http_timeout_ms: u64,
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
    pub tools: AgentToolsConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            event_stream_key: "lc:stream:alephant-agent-events".to_string(),
            event_log_http_fallback_enabled: true,
            event_log_http_endpoint: "/v1/log/agent-event".to_string(),
            event_log_http_timeout_ms: 1000,
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
            tools: AgentToolsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentToolsConfig {
    pub enabled: bool,
    pub catalog_source: AgentToolsCatalogSource,
    pub redis_active_pointer_prefix: String,
    pub snapshot_cache_ttl_ms: u64,
    pub require_policy_decision: bool,
    pub fail_open_for_static_targets: bool,
    pub schema_validation_enabled: bool,
    pub idempotency_ttl_seconds: u64,
    pub response_mode: AgentToolResponseMode,
    pub redis_timeout_ms: u64,
    pub mcp_session_cache_ttl_secs: u64,
    pub mcp_session_lock_ttl_secs: u64,
    pub mcp_session_max_concurrent_per_session: usize,
    pub mcp_sse_max_event_bytes: usize,
    pub mcp_sse_max_line_bytes: usize,
    pub mcp_sse_max_events: usize,
    pub mcp_sse_max_batch_items: usize,
    pub mcp_sse_idle_timeout_ms: u64,
    pub policy_timeout_ms: u64,
    pub max_concurrent_per_workspace: usize,
    pub budget: AgentToolsBudgetConfig,
    pub egress_policy: AgentToolEgressPolicyConfig,
    pub timeout_ms: u64,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub targets: Vec<AgentToolTargetConfig>,
}

impl Default for AgentToolsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            catalog_source: AgentToolsCatalogSource::Static,
            redis_active_pointer_prefix: "alephant:agent-tools:v1".to_string(),
            snapshot_cache_ttl_ms: 60_000,
            require_policy_decision: true,
            fail_open_for_static_targets: false,
            schema_validation_enabled: true,
            idempotency_ttl_seconds: 3600,
            response_mode: AgentToolResponseMode::HttpStrict,
            redis_timeout_ms: 1000,
            mcp_session_cache_ttl_secs: 600,
            mcp_session_lock_ttl_secs: 5,
            mcp_session_max_concurrent_per_session: 1,
            mcp_sse_max_event_bytes: 16_384,
            mcp_sse_max_line_bytes: 8_192,
            mcp_sse_max_events: 256,
            mcp_sse_max_batch_items: 64,
            mcp_sse_idle_timeout_ms: 5000,
            policy_timeout_ms: 3000,
            max_concurrent_per_workspace: 32,
            budget: AgentToolsBudgetConfig::default(),
            egress_policy: AgentToolEgressPolicyConfig::default(),
            timeout_ms: 8000,
            max_request_bytes: 65_536,
            max_response_bytes: 65_536,
            targets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentToolsBudgetConfig {
    pub max_tool_call_cost_micros: Option<u64>,
}

impl Default for AgentToolsBudgetConfig {
    fn default() -> Self {
        Self {
            max_tool_call_cost_micros: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentToolsCatalogSource {
    #[default]
    Static,
    Redis,
    Hybrid,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentToolResponseMode {
    #[default]
    HttpStrict,
    AgentToolCompatible,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentToolEgressPolicyConfig {
    pub https_only: bool,
    pub block_loopback: bool,
    pub block_link_local: bool,
    pub block_metadata_ip: bool,
    pub block_private_network: bool,
    pub allow_environment_proxy: bool,
}

impl Default for AgentToolEgressPolicyConfig {
    fn default() -> Self {
        Self {
            https_only: true,
            block_loopback: true,
            block_link_local: true,
            block_metadata_ip: true,
            block_private_network: true,
            allow_environment_proxy: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentToolTargetConfig {
    pub tool_id: String,
    pub name: String,
    pub description: String,
    pub kind: AgentToolTargetKind,
    pub url: Option<String>,
    pub method: String,
    pub service_slug: Option<String>,
    pub operation_id: Option<String>,
    pub operation_slug: Option<String>,
    pub risk_level: String,
    pub timeout_ms: Option<u64>,
    pub input_schema: serde_json::Value,
    pub rate_card: AgentToolRateCardConfig,
    pub allowlist: AgentToolAllowlistConfig,
}

impl Default for AgentToolTargetConfig {
    fn default() -> Self {
        Self {
            tool_id: String::new(),
            name: String::new(),
            description: String::new(),
            kind: AgentToolTargetKind::default(),
            url: None,
            method: "POST".to_string(),
            service_slug: None,
            operation_id: None,
            operation_slug: None,
            risk_level: "medium".to_string(),
            timeout_ms: None,
            input_schema: serde_json::json!({ "type": "object" }),
            rate_card: AgentToolRateCardConfig::default(),
            allowlist: AgentToolAllowlistConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentToolTargetKind {
    #[default]
    Mock,
    Http,
    McpHttp,
    McpStreamableHttp,
    McpSse,
    #[serde(rename = "openapi")]
    OpenApi,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentToolRateCardConfig {
    pub currency: String,
    pub fixed_micros: u64,
}

impl Default for AgentToolRateCardConfig {
    fn default() -> Self {
        Self {
            currency: "USD".to_string(),
            fixed_micros: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AgentToolAllowlistConfig {
    pub workspace_ids: Vec<Uuid>,
    pub virtual_key_ids: Vec<Uuid>,
    pub agent_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentConflictAction {
    Disabled,
    #[default]
    Warn,
    Strict,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
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
        assert!(!cfg.tools.enabled);
        assert_eq!(cfg.tools.timeout_ms, 8000);
        assert_eq!(cfg.tools.max_request_bytes, 65_536);
        assert_eq!(cfg.tools.max_response_bytes, 65_536);
        assert!(cfg.tools.targets.is_empty());
    }

    #[test]
    fn deserializes_kebab_case_config() {
        let raw = r#"
enabled: true
event-stream-key: custom:agent
event-log-http-fallback-enabled: false
event-log-http-endpoint: "http://collector.local/v1/log/agent-event"
event-log-http-timeout-ms: 750
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
tools:
  enabled: true
  timeout-ms: 12000
  max-request-bytes: 32768
  max-response-bytes: 49152
  targets:
    - tool-id: get-ledger
      name: Ledger Lookup
      description: Fetches ledger entries
      kind: http
      url: "https://tools.local/ledger"
      method: POST
      risk-level: low
      timeout-ms: 2000
      input-schema:
        type: object
        properties:
          account:
            type: string
      rate-card:
        currency: USD
        fixed-micros: 2500
      allowlist:
        workspace-ids:
          - 11111111-1111-1111-1111-111111111111
        virtual-key-ids:
          - 22222222-2222-2222-2222-222222222222
        agent-ids:
          - agent-ledger
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
        let serialized = serde_yml::to_string(&cfg).unwrap();
        assert!(!serialized.contains("event-log-http-auth"));
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
        assert!(cfg.tools.enabled);
        assert_eq!(cfg.tools.timeout_ms, 12000);
        assert_eq!(cfg.tools.max_request_bytes, 32_768);
        assert_eq!(cfg.tools.max_response_bytes, 49_152);

        let target = cfg.tools.targets.first().unwrap();
        assert_eq!(target.tool_id, "get-ledger");
        assert_eq!(target.name, "Ledger Lookup");
        assert_eq!(target.description, "Fetches ledger entries");
        assert_eq!(target.kind, AgentToolTargetKind::Http);
        assert_eq!(target.url.as_deref(), Some("https://tools.local/ledger"));
        assert_eq!(target.method, "POST");
        assert_eq!(target.risk_level, "low");
        assert_eq!(target.timeout_ms, Some(2000));
        assert_eq!(target.input_schema["type"], "object");
        assert_eq!(target.rate_card.currency, "USD");
        assert_eq!(target.rate_card.fixed_micros, 2500);
        assert_eq!(
            target.allowlist.workspace_ids[0],
            uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap()
        );
        assert_eq!(
            target.allowlist.virtual_key_ids[0],
            uuid::Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap()
        );
        assert_eq!(target.allowlist.agent_ids[0], "agent-ledger");
    }

    #[test]
    fn tool_target_defaults_are_conservative() {
        let raw = r#"
tools:
  targets:
    - tool-id: mock-ledger
"#;
        let cfg: AgentConfig = serde_yml::from_str(raw).unwrap();
        let target = cfg.tools.targets.first().unwrap();

        assert_eq!(target.kind, AgentToolTargetKind::Mock);
        assert_eq!(target.url, None);
        assert_eq!(target.method, "POST");
        assert_eq!(target.risk_level, "medium");
        assert_eq!(target.timeout_ms, None);
        assert_eq!(target.input_schema["type"], "object");
        assert_eq!(target.rate_card.currency, "USD");
        assert_eq!(target.rate_card.fixed_micros, 0);
        assert!(target.allowlist.workspace_ids.is_empty());
        assert!(target.allowlist.virtual_key_ids.is_empty());
        assert!(target.allowlist.agent_ids.is_empty());
    }

    #[test]
    fn parses_mcp_http_agent_tool_target_kind() {
        let raw = r#"
enabled: true
targets:
  - tool-id: docs.search
    name: Search Docs
    description: Search docs through an MCP HTTP server
    kind: mcp-http
    url: https://mcp.example.com/mcp
    method: POST
"#;

        let cfg: AgentToolsConfig = serde_yml::from_str(raw).unwrap();

        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.targets[0].kind, AgentToolTargetKind::McpHttp);
        assert_eq!(
            cfg.targets[0].url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
    }

    #[test]
    fn parses_mcp_sse_agent_tool_target_kind() {
        let cfg: AgentToolsConfig = serde_yml::from_str(
            r#"
enabled: true
targets:
  - tool-id: docs.search
    name: Search docs
    description: Search product docs through traditional MCP SSE
    kind: mcp-sse
    url: https://mcp.example.com/sse
    method: GET
"#,
        )
        .expect("agent tools config");

        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.targets[0].kind, AgentToolTargetKind::McpSse);
        assert_eq!(
            cfg.targets[0].url.as_deref(),
            Some("https://mcp.example.com/sse")
        );
        assert_eq!(cfg.targets[0].method, "GET");
        assert_eq!(
            serde_yml::to_string(&cfg.targets[0].kind)
                .expect("serialize kind")
                .trim(),
            "mcp-sse"
        );
    }

    #[test]
    fn agent_tool_target_kind_accepts_openapi() {
        let yaml = r#"
tool-id: support.get-ticket
name: Get ticket
description: Fetch a support ticket
kind: openapi
method: GET
input-schema:
  type: object
"#;

        let target: AgentToolTargetConfig = serde_yml::from_str(yaml).unwrap();

        assert_eq!(target.kind, AgentToolTargetKind::OpenApi);
        assert_eq!(target.method, "GET");
    }

    #[test]
    fn defaults_include_mcp_streamable_http_session_config() {
        let cfg = AgentToolsConfig::default();

        assert_eq!(cfg.mcp_session_cache_ttl_secs, 600);
        assert_eq!(cfg.mcp_session_lock_ttl_secs, 5);
        assert_eq!(cfg.mcp_session_max_concurrent_per_session, 1);
        assert_eq!(cfg.mcp_sse_max_event_bytes, 16_384);
        assert_eq!(cfg.mcp_sse_max_line_bytes, 8_192);
        assert_eq!(cfg.mcp_sse_max_events, 256);
        assert_eq!(cfg.mcp_sse_max_batch_items, 64);
        assert_eq!(cfg.mcp_sse_idle_timeout_ms, 5000);
    }

    #[test]
    fn parses_mcp_streamable_http_agent_tool_target_kind() {
        let cfg: AgentConfig = serde_yml::from_str(
            r#"
enabled: true
tools:
  enabled: true
  mcp-session-cache-ttl-secs: 120
  mcp-session-lock-ttl-secs: 3
  mcp-session-max-concurrent-per-session: 1
  mcp-sse-max-event-bytes: 2048
  mcp-sse-max-line-bytes: 1024
  mcp-sse-max-events: 16
  mcp-sse-max-batch-items: 8
  mcp-sse-idle-timeout-ms: 750
  targets:
    - tool-id: docs.search
      name: Search docs
      description: Search product docs
      kind: mcp-streamable-http
      url: https://mcp.example.com/mcp
"#,
        )
        .expect("agent config");

        assert_eq!(cfg.tools.mcp_session_cache_ttl_secs, 120);
        assert_eq!(cfg.tools.mcp_session_lock_ttl_secs, 3);
        assert_eq!(cfg.tools.mcp_sse_max_event_bytes, 2048);
        assert_eq!(cfg.tools.mcp_sse_idle_timeout_ms, 750);
        assert_eq!(
            cfg.tools.targets[0].kind,
            AgentToolTargetKind::McpStreamableHttp
        );
        assert_eq!(
            serde_yml::to_string(&cfg.tools.targets[0].kind)
                .expect("serialize kind")
                .trim(),
            "mcp-streamable-http"
        );
    }

    #[test]
    fn agent_tools_runtime_defaults_are_conservative() {
        let cfg = AgentToolsConfig::default();

        assert!(!cfg.enabled);
        assert_eq!(cfg.catalog_source, AgentToolsCatalogSource::Static);
        assert_eq!(cfg.redis_active_pointer_prefix, "alephant:agent-tools:v1");
        assert_eq!(cfg.snapshot_cache_ttl_ms, 60000);
        assert!(cfg.require_policy_decision);
        assert!(!cfg.fail_open_for_static_targets);
        assert!(cfg.schema_validation_enabled);
        assert_eq!(cfg.idempotency_ttl_seconds, 3600);
        assert_eq!(cfg.response_mode, AgentToolResponseMode::HttpStrict);
        assert_eq!(cfg.redis_timeout_ms, 1000);
        assert_eq!(cfg.policy_timeout_ms, 3000);
        assert_eq!(cfg.max_concurrent_per_workspace, 32);
        assert!(cfg.egress_policy.https_only);
        assert!(cfg.egress_policy.block_loopback);
        assert!(cfg.egress_policy.block_link_local);
        assert!(cfg.egress_policy.block_metadata_ip);
        assert!(cfg.egress_policy.block_private_network);
        assert!(!cfg.egress_policy.allow_environment_proxy);
    }

    #[test]
    fn agent_tools_runtime_config_deserializes_kebab_case() {
        let raw = r#"
enabled: true
catalog-source: redis
redis-active-pointer-prefix: "alephant:agent-tools:v1"
snapshot-cache-ttl-ms: 12345
require-policy-decision: true
fail-open-for-static-targets: false
schema-validation-enabled: true
idempotency-ttl-seconds: 7200
response-mode: agent-tool-compatible
redis-timeout-ms: 2345
policy-timeout-ms: 3456
max-concurrent-per-workspace: 64
egress-policy:
  https-only: true
  block-loopback: true
  block-link-local: true
  block-metadata-ip: true
  block-private-network: true
  allow-environment-proxy: false
targets: []
"#;
        let cfg: AgentToolsConfig = serde_yml::from_str(raw).unwrap();

        assert!(cfg.enabled);
        assert_eq!(cfg.catalog_source, AgentToolsCatalogSource::Redis);
        assert_eq!(cfg.snapshot_cache_ttl_ms, 12345);
        assert_eq!(cfg.idempotency_ttl_seconds, 7200);
        assert_eq!(
            cfg.response_mode,
            AgentToolResponseMode::AgentToolCompatible
        );
        assert_eq!(cfg.redis_timeout_ms, 2345);
        assert_eq!(cfg.policy_timeout_ms, 3456);
        assert_eq!(cfg.max_concurrent_per_workspace, 64);
        assert!(cfg.egress_policy.block_link_local);
        assert!(cfg.egress_policy.block_private_network);
        assert!(!cfg.egress_policy.allow_environment_proxy);
    }

    #[test]
    fn agent_tools_budget_config_deserializes_tool_call_limit() {
        let raw = r#"
enabled: true
budget:
  max-tool-call-cost-micros: 25
targets: []
"#;
        let cfg: AgentToolsConfig = serde_yml::from_str(raw).unwrap();

        assert_eq!(cfg.budget.max_tool_call_cost_micros, Some(25));
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
