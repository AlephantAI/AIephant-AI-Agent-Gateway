use std::{collections::HashSet, time::Duration};

use chrono::Utc;
use sha2::{Digest, Sha256};

use super::types::{
    RuntimeActivePointer, RuntimeRateCard, RuntimeSnapshot, RuntimeTool, RuntimeToolTarget,
    SnapshotEnvelope, SnapshotSource, VersionVector, active_pointer_key, revision_key,
};
use crate::{
    agent::tools::catalog::{TOOL_SCHEMA_VERSION, framework_tool_name, target_kind_name},
    config::agent::{AgentToolTargetConfig, AgentToolsConfig},
    types::extensions::AuthContext,
};

#[derive(Debug, Clone)]
pub struct RuntimeSnapshotProvider {
    cache_ttl: Duration,
}

impl RuntimeSnapshotProvider {
    pub fn new(cache_ttl: Duration) -> Self {
        Self { cache_ttl }
    }

    pub async fn resolve_static(
        &self,
        auth: &AuthContext,
        cfg: &AgentToolsConfig,
    ) -> SnapshotEnvelope {
        let _cache_ttl = self.cache_ttl;
        static_snapshot(&auth.org_id.to_string(), cfg)
    }
}

pub fn static_snapshot(workspace_id: &str, config: &AgentToolsConfig) -> SnapshotEnvelope {
    let snapshot_revision = 0;
    let active_pointer_revision = 0;
    let policy_revision = 0;
    let mut seen_framework_names = HashSet::new();
    let mut tools: Vec<RuntimeTool> = config
        .targets
        .iter()
        .map(|target| {
            let framework_name = framework_tool_name(&target.tool_id, &mut seen_framework_names);
            runtime_tool(target, config.timeout_ms, framework_name)
        })
        .collect();
    let toolset_hash = sha256_json(&tools);
    let payload_hash = sha256_json(&serde_json::json!({
        "workspaceId": workspace_id,
        "snapshotRevision": snapshot_revision,
        "tools": tools,
    }));

    for tool in &mut tools {
        tool.version_vector = VersionVector::from_tool(
            snapshot_revision,
            active_pointer_revision,
            &payload_hash,
            &toolset_hash,
            policy_revision,
            tool,
        );
    }

    let active_pointer = RuntimeActivePointer {
        workspace_id: workspace_id.to_string(),
        snapshot_revision,
        active_pointer_revision,
        revision_key: revision_key(
            &config.redis_active_pointer_prefix,
            workspace_id,
            snapshot_revision as i64,
        ),
        schema_version: TOOL_SCHEMA_VERSION.to_string(),
        redis_key: active_pointer_key(&config.redis_active_pointer_prefix, workspace_id),
        activated_at: Utc::now().to_rfc3339(),
        payload_hash: payload_hash.clone(),
        toolset_hash: toolset_hash.clone(),
        source: SnapshotSource::Static,
    };
    let snapshot = RuntimeSnapshot {
        workspace_id: workspace_id.to_string(),
        snapshot_revision,
        active_pointer_revision,
        payload_hash,
        toolset_hash,
        policy_revision,
        source: SnapshotSource::Static,
        tools,
    };

    SnapshotEnvelope {
        workspace_id: workspace_id.to_string(),
        snapshot_revision,
        active_pointer_revision,
        policy_revision,
        payload_hash: snapshot.payload_hash.clone(),
        toolset_hash: snapshot.toolset_hash.clone(),
        source: SnapshotSource::Static,
        active_pointer,
        snapshot,
    }
}

fn runtime_tool(
    target: &AgentToolTargetConfig,
    default_timeout_ms: u64,
    framework_tool_name: String,
) -> RuntimeTool {
    let schema_hash = sha256_json(&target.input_schema);
    let kind = target_kind_name(&target.kind).to_string();

    RuntimeTool {
        tool_id: target.tool_id.clone(),
        kind: kind.clone(),
        framework_tool_name,
        upstream_tool_name: target.tool_id.clone(),
        display_name: target.name.clone(),
        safe_model_description: target.description.clone(),
        name: target.name.clone(),
        description: target.description.clone(),
        tool_version: 0,
        input_schema: target.input_schema.clone(),
        output_schema: serde_json::json!({ "type": "object" }),
        schema_version: TOOL_SCHEMA_VERSION.to_string(),
        schema_hash,
        risk_level: target.risk_level.clone(),
        approval_mode: "none".to_string(),
        charge_on_failure: false,
        fixed_micros: target.rate_card.fixed_micros,
        currency: target.rate_card.currency.clone(),
        timeout_ms: target.timeout_ms.unwrap_or(default_timeout_ms),
        rate_card: RuntimeRateCard {
            currency: target.rate_card.currency.clone(),
            fixed_micros: target.rate_card.fixed_micros,
        },
        rate_card_revision: 0,
        target: RuntimeToolTarget {
            kind,
            url: target.url.clone(),
            method: target.method.clone(),
            openapi: crate::agent::tools::openapi::types::RuntimeOpenApiTarget {
                service_slug: target
                    .service_slug
                    .clone()
                    .unwrap_or_else(|| target.tool_id.clone()),
                operation_id: target
                    .operation_id
                    .clone()
                    .unwrap_or_else(|| target.tool_id.clone()),
                operation_slug: target
                    .operation_slug
                    .clone()
                    .unwrap_or_else(|| target.tool_id.clone()),
                ..Default::default()
            },
        },
        target_revision: 0,
        version_vector: VersionVector::default(),
    }
}

fn sha256_json<T: serde::Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).expect("snapshot value serializes");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::{
        config::agent::{AgentToolRateCardConfig, AgentToolTargetConfig, AgentToolTargetKind},
        types::{extensions::AuthContext, org::OrgId, secret::Secret, user::UserId},
    };

    #[test]
    fn static_provider_converts_static_targets_to_runtime_snapshot() {
        let config = AgentToolsConfig {
            timeout_ms: 8000,
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                name: "Echo".to_string(),
                description: "Echo arguments".to_string(),
                kind: AgentToolTargetKind::Http,
                url: Some("https://tools.example/echo".to_string()),
                timeout_ms: Some(1200),
                rate_card: AgentToolRateCardConfig {
                    currency: "USD".to_string(),
                    fixed_micros: 25,
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let envelope = static_snapshot("workspace-1", &config);

        assert_eq!(envelope.snapshot.workspace_id, "workspace-1");
        assert_eq!(envelope.snapshot.source, SnapshotSource::Static);
        assert_eq!(envelope.snapshot.tools.len(), 1);
        let tool = &envelope.snapshot.tools[0];
        assert_eq!(tool.tool_id, "support.echo");
        assert_eq!(tool.kind, "http");
        assert_eq!(tool.target.kind, "http");
        assert_eq!(tool.timeout_ms, 1200);
        assert_eq!(tool.fixed_micros, 25);
        assert_eq!(tool.currency, "USD");
        assert_eq!(tool.rate_card.fixed_micros, 25);
        assert_eq!(
            tool.version_vector.payload_hash,
            envelope.snapshot.payload_hash
        );
    }

    #[test]
    fn static_provider_preserves_mcp_http_target_kind() {
        let config = AgentToolsConfig {
            timeout_ms: 8000,
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                name: "Search Docs".to_string(),
                description: "Search docs through MCP HTTP".to_string(),
                kind: AgentToolTargetKind::McpHttp,
                url: Some("https://mcp.example.com/mcp".to_string()),
                timeout_ms: Some(1500),
                rate_card: AgentToolRateCardConfig {
                    currency: "USD".to_string(),
                    fixed_micros: 10_000,
                },
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };

        let envelope = static_snapshot("workspace-1", &config);

        assert_eq!(envelope.snapshot.tools.len(), 1);
        let tool = &envelope.snapshot.tools[0];
        assert_eq!(tool.tool_id, "docs.search");
        assert_eq!(tool.kind, "mcp-http");
        assert_eq!(tool.target.kind, "mcp-http");
        assert_eq!(
            tool.target.url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
        assert_eq!(tool.timeout_ms, 1500);
        assert_eq!(tool.fixed_micros, 10_000);
    }

    #[test]
    fn static_provider_preserves_mcp_streamable_http_target_kind() {
        let config = AgentToolsConfig {
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                kind: AgentToolTargetKind::McpStreamableHttp,
                url: Some("https://mcp.example.com/mcp".to_string()),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };

        let envelope = static_snapshot("workspace-1", &config);
        let tool = &envelope.snapshot.tools[0];

        assert_eq!(tool.kind, "mcp-streamable-http");
        assert_eq!(tool.target.kind, "mcp-streamable-http");
        assert_eq!(
            tool.target.url.as_deref(),
            Some("https://mcp.example.com/mcp")
        );
    }

    #[test]
    fn static_provider_preserves_mcp_sse_target_kind() {
        let config = AgentToolsConfig {
            targets: vec![AgentToolTargetConfig {
                tool_id: "docs.search".to_string(),
                kind: AgentToolTargetKind::McpSse,
                url: Some("https://mcp.example.com/sse".to_string()),
                method: "GET".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };

        let envelope = static_snapshot("workspace-1", &config);
        let tool = &envelope.snapshot.tools[0];

        assert_eq!(tool.kind, "mcp-sse");
        assert_eq!(tool.target.kind, "mcp-sse");
        assert_eq!(
            tool.target.url.as_deref(),
            Some("https://mcp.example.com/sse")
        );
        assert_eq!(tool.target.method, "GET");
    }

    #[tokio::test]
    async fn resolve_static_uses_auth_workspace_and_runtime_tool_names() {
        let workspace_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let auth = auth_context(workspace_id);
        let config = AgentToolsConfig {
            targets: vec![AgentToolTargetConfig {
                tool_id: "support.echo".to_string(),
                name: "Echo".to_string(),
                description: "Echo arguments".to_string(),
                ..AgentToolTargetConfig::default()
            }],
            ..AgentToolsConfig::default()
        };
        let provider = RuntimeSnapshotProvider::new(Duration::from_secs(60));

        let envelope = provider.resolve_static(&auth, &config).await;

        assert_eq!(envelope.workspace_id, workspace_id.to_string());
        assert_eq!(envelope.snapshot.workspace_id, workspace_id.to_string());
        let tool = &envelope.snapshot.tools[0];
        assert_eq!(tool.framework_tool_name, "support_echo");
        assert_eq!(tool.upstream_tool_name, "support.echo");
        assert_eq!(tool.kind, "mock");
        assert_eq!(tool.display_name, "Echo");
        assert_eq!(tool.safe_model_description, "Echo arguments");
        assert_eq!(tool.fixed_micros, 0);
        assert_eq!(tool.currency, "USD");
        assert_eq!(tool.output_schema, serde_json::json!({ "type": "object" }));
        assert_eq!(tool.approval_mode, "none");
        assert!(!tool.charge_on_failure);
    }

    fn auth_context(workspace_id: Uuid) -> AuthContext {
        AuthContext {
            api_key: Secret::from(String::new()),
            user_id: UserId::new(Uuid::nil()),
            org_id: OrgId::new(workspace_id),
            workspace_type: None,
            virtual_key_id: None,
            virtual_key_prefix: String::new(),
            master_key_id: None,
            master_key_base_url: None,
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            registered_agent_name: None,
            body_ttl_days: 1,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }
}
