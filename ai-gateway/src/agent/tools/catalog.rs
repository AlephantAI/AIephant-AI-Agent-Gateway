use std::collections::HashSet;

use sha2::{Digest, Sha256};

use super::{
    runtime_snapshot::types::RuntimeTool,
    types::{ToolAvailability, ToolCostPolicy, ToolDescriptor, ToolDescriptorMetadata},
};
use crate::{
    config::agent::{AgentToolTargetConfig, AgentToolTargetKind},
    types::extensions::AuthContext,
};

pub(crate) const TOOL_SCHEMA_VERSION: &str = "2026-06-05.1";

pub fn visible_tools(
    auth: &AuthContext,
    targets: &[AgentToolTargetConfig],
    _reported_agent_id: Option<&str>,
    default_timeout_ms: u64,
) -> Vec<ToolDescriptor> {
    let trusted_agent_id = trusted_agent_id_from_auth(auth);
    let mut seen_framework_names = HashSet::new();
    targets
        .iter()
        .filter(|target| target_visible(auth, target, trusted_agent_id.as_deref()))
        .map(|target| descriptor(target, default_timeout_ms, &mut seen_framework_names))
        .collect()
}

pub fn find_callable_tool<'a>(
    auth: &AuthContext,
    targets: &'a [AgentToolTargetConfig],
    _reported_agent_id: Option<&str>,
    tool_id: &str,
) -> Option<&'a AgentToolTargetConfig> {
    let trusted_agent_id = trusted_agent_id_from_auth(auth);
    targets
        .iter()
        .find(|target| target.tool_id == tool_id)
        .filter(|target| target_visible(auth, target, trusted_agent_id.as_deref()))
}

fn target_visible(
    auth: &AuthContext,
    target: &AgentToolTargetConfig,
    requested_agent_id: Option<&str>,
) -> bool {
    let allow = &target.allowlist;

    let workspace_allowed = allow.workspace_ids.is_empty()
        || allow
            .workspace_ids
            .iter()
            .any(|id| id == auth.org_id.as_ref());
    let vk_allowed = allow.virtual_key_ids.is_empty()
        || auth
            .virtual_key_id
            .is_some_and(|id| allow.virtual_key_ids.contains(&id));
    let agent_allowed = allow.agent_ids.is_empty()
        || requested_agent_id
            .is_some_and(|agent_id| allow.agent_ids.iter().any(|id| id == agent_id));

    workspace_allowed && vk_allowed && agent_allowed
}

fn trusted_agent_id_from_auth(auth: &AuthContext) -> Option<String> {
    (auth.entity_type.eq_ignore_ascii_case("agent") && !auth.entity_id.is_nil())
        .then(|| auth.entity_id.to_string())
}

fn descriptor(
    target: &AgentToolTargetConfig,
    default_timeout_ms: u64,
    seen_framework_names: &mut HashSet<String>,
) -> ToolDescriptor {
    let schema_hash = schema_hash_for_value(&target.input_schema);
    let target_kind = target_kind_name(&target.kind).to_string();

    ToolDescriptor {
        tool_id: target.tool_id.clone(),
        framework_tool_name: framework_tool_name(&target.tool_id, seen_framework_names),
        metadata: ToolDescriptorMetadata {
            target_kind,
            target_id: target.tool_id.clone(),
            service_slug: None,
            operation_id: None,
            operation_slug: None,
            target_hash: None,
            target_revision: None,
            schema_hash: None,
            auth_revision: None,
            rate_card_revision: None,
        },
        upstream_tool_name: target.tool_id.clone(),
        name_sanitization_version: "v1".to_string(),
        mapping_revision: 0,
        snapshot_revision: 0,
        policy_revision: 0,
        display_name: target.name.clone(),
        safe_model_description: target.description.clone(),
        name: target.name.clone(),
        description: target.description.clone(),
        tool_version: 0,
        input_schema: target.input_schema.clone(),
        output_schema: serde_json::json!({}),
        schema_version: TOOL_SCHEMA_VERSION.to_string(),
        schema_hash,
        risk_level: target.risk_level.clone(),
        approval_mode: "none".to_string(),
        timeout_ms: target.timeout_ms.unwrap_or(default_timeout_ms),
        availability: ToolAvailability {
            catalog_status: "published".to_string(),
            policy_preview: "callable".to_string(),
            runtime_status: "healthy".to_string(),
            visibility: "listed_available".to_string(),
            source: "static".to_string(),
            reason_code: String::new(),
            reason: String::new(),
            may_become_available: false,
            remediation: String::new(),
        },
        cost_policy: ToolCostPolicy {
            pricing_type: "per_call".to_string(),
            fixed_micros: target.rate_card.fixed_micros,
            source: "rate_card".to_string(),
            currency: target.rate_card.currency.clone(),
        },
    }
}

pub(crate) fn schema_hash_for_value(input_schema: &serde_json::Value) -> String {
    let schema_bytes =
        serde_json::to_vec(input_schema).expect("serde_json::Value should serialize");
    format!("sha256:{:x}", Sha256::digest(schema_bytes))
}

pub fn descriptor_from_runtime_tool(
    tool: &RuntimeTool,
    snapshot_revision: u64,
    policy_revision: u64,
) -> ToolDescriptor {
    let target_kind = if tool.target.kind.is_empty() {
        tool.kind.clone()
    } else {
        tool.target.kind.clone()
    };

    let is_openapi = target_kind == "openapi";
    let openapi = &tool.target.openapi;

    ToolDescriptor {
        tool_id: tool.tool_id.clone(),
        framework_tool_name: tool.framework_tool_name.clone(),
        metadata: ToolDescriptorMetadata {
            target_kind,
            target_id: tool.tool_id.clone(),
            service_slug: if is_openapi {
                Some(openapi.service_slug.clone())
            } else {
                None
            },
            operation_id: if is_openapi {
                Some(openapi.operation_id.clone())
            } else {
                None
            },
            operation_slug: if is_openapi {
                Some(openapi.operation_slug.clone())
            } else {
                None
            },
            target_hash: if is_openapi {
                Some(openapi.target_hash.clone())
            } else {
                None
            },
            target_revision: if is_openapi {
                Some(tool.target_revision)
            } else {
                None
            },
            schema_hash: if is_openapi {
                Some(tool.schema_hash.clone())
            } else {
                None
            },
            auth_revision: if is_openapi {
                Some(openapi.auth_revision)
            } else {
                None
            },
            rate_card_revision: if is_openapi {
                Some(tool.rate_card_revision)
            } else {
                None
            },
        },
        upstream_tool_name: tool.upstream_tool_name.clone(),
        name_sanitization_version: "v1".to_string(),
        mapping_revision: tool.tool_version,
        snapshot_revision,
        policy_revision,
        display_name: tool.display_name.clone(),
        safe_model_description: tool.safe_model_description.clone(),
        name: tool.display_name.clone(),
        description: tool.description.clone(),
        tool_version: tool.tool_version,
        input_schema: tool.input_schema.clone(),
        output_schema: tool.output_schema.clone(),
        schema_version: tool.schema_version.clone(),
        schema_hash: tool.schema_hash.clone(),
        risk_level: tool.risk_level.clone(),
        approval_mode: tool.approval_mode.clone(),
        timeout_ms: tool.timeout_ms,
        availability: ToolAvailability {
            catalog_status: "published".to_string(),
            policy_preview: "callable".to_string(),
            runtime_status: "healthy".to_string(),
            visibility: "listed_available".to_string(),
            source: "snapshot".to_string(),
            reason_code: String::new(),
            reason: String::new(),
            may_become_available: false,
            remediation: String::new(),
        },
        cost_policy: ToolCostPolicy {
            pricing_type: "per_call".to_string(),
            fixed_micros: tool.fixed_micros,
            source: "rate_card".to_string(),
            currency: tool.currency.clone(),
        },
    }
}

pub(crate) fn framework_tool_name(tool_id: &str, seen: &mut HashSet<String>) -> String {
    let base = framework_tool_name_base(tool_id);
    if seen.insert(base.clone()) {
        return base;
    }

    let suffix = stable_hash_suffix(tool_id);
    let keep = 64usize.saturating_sub(1 + suffix.len());
    let mut with_suffix: String = base.chars().take(keep).collect();
    with_suffix.push('_');
    with_suffix.push_str(&suffix);
    if seen.insert(with_suffix.clone()) {
        return with_suffix;
    }

    for salt in 1u64.. {
        let salted = stable_hash_suffix(&format!("{tool_id}:{salt}"));
        let mut candidate: String = base.chars().take(keep).collect();
        candidate.push('_');
        candidate.push_str(&salted);
        if seen.insert(candidate.clone()) {
            return candidate;
        }
    }

    unreachable!("unbounded stable suffix salt should produce a unique name")
}

fn framework_tool_name_base(tool_id: &str) -> String {
    let mut base: String = tool_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if base.is_empty() {
        base.push_str("tool");
    }
    if base.len() > 64 {
        base.truncate(64);
    }
    base
}

fn stable_hash_suffix(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
        .chars()
        .take(8)
        .collect()
}

pub(crate) const fn target_kind_name(kind: &AgentToolTargetKind) -> &'static str {
    match kind {
        AgentToolTargetKind::Mock => "mock",
        AgentToolTargetKind::Http => "http",
        AgentToolTargetKind::McpHttp => "mcp-http",
        AgentToolTargetKind::McpStreamableHttp => "mcp-streamable-http",
        AgentToolTargetKind::McpSse => "mcp-sse",
        AgentToolTargetKind::OpenApi => "openapi",
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::{
        config::agent::{AgentToolAllowlistConfig, AgentToolTargetConfig},
        types::{extensions::AuthContext, org::OrgId, secret::Secret, user::UserId},
    };

    #[test]
    fn visible_tools_filters_by_virtual_key() {
        let vk = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let auth = auth_context_with_virtual_key(vk);
        let targets = vec![
            target_with_vk("support.echo", vk),
            target_with_vk("support.hidden", Uuid::new_v4()),
        ];

        let tools = visible_tools(&auth, &targets, None, 8000);

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_id, "support.echo");
    }

    #[test]
    fn visible_tools_does_not_trust_reported_agent_id_for_allowlist() {
        let auth = auth_context_with_virtual_key(Uuid::new_v4());
        let targets = vec![target_with_agent("support.agent-only", "agent-1")];

        let tools = visible_tools(&auth, &targets, Some("agent-1"), 8000);

        assert!(tools.is_empty());
    }

    #[test]
    fn visible_tools_matches_authenticated_agent_entity_id() {
        let agent_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let mut auth = auth_context_with_virtual_key(Uuid::new_v4());
        auth.entity_type = "agent".to_string();
        auth.entity_id = agent_id;
        let targets = vec![target_with_agent(
            "support.agent-only",
            &agent_id.to_string(),
        )];

        let tools = visible_tools(&auth, &targets, Some("spoofed-agent"), 8000);

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_id, "support.agent-only");
    }

    #[test]
    fn descriptor_uses_stable_schema_version_and_content_hash() {
        let auth = auth_context_with_virtual_key(Uuid::new_v4());
        let target = AgentToolTargetConfig {
            tool_id: "support.echo".to_string(),
            input_schema: serde_json::json!({ "type": "object" }),
            ..AgentToolTargetConfig::default()
        };

        let tools = visible_tools(&auth, &[target], None, 8000);

        assert_eq!(tools[0].schema_version, TOOL_SCHEMA_VERSION);
        assert!(tools[0].schema_hash.starts_with("sha256:"));
        assert_ne!(tools[0].schema_hash, tools[0].schema_version);
    }

    #[test]
    fn visible_tools_returns_framework_safe_names_and_target_metadata() {
        let auth = auth_context_with_virtual_key(Uuid::new_v4());
        let targets = vec![
            AgentToolTargetConfig {
                tool_id: "Docs.Search".to_string(),
                kind: crate::config::agent::AgentToolTargetKind::McpStreamableHttp,
                ..AgentToolTargetConfig::default()
            },
            AgentToolTargetConfig {
                tool_id: "docs_search".to_string(),
                ..AgentToolTargetConfig::default()
            },
            AgentToolTargetConfig {
                tool_id: "SDK:Tool/Name".to_string(),
                ..AgentToolTargetConfig::default()
            },
        ];

        let tools = visible_tools(&auth, &targets, None, 8000);

        assert_eq!(tools[0].framework_tool_name, "docs_search");
        assert_eq!(tools[0].metadata.target_kind, "mcp-streamable-http");
        assert_eq!(tools[0].metadata.target_id, "Docs.Search");
        assert_eq!(tools[1].framework_tool_name.len(), "docs_search_".len() + 8);
        assert!(tools[1].framework_tool_name.starts_with("docs_search_"));
        assert_eq!(tools[2].framework_tool_name, "sdk_tool_name");
    }

    #[test]
    fn mcp_sse_descriptor_exposes_target_kind() {
        let auth = auth_context_with_virtual_key(Uuid::new_v4());
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            name: "Search docs".to_string(),
            description: "Search product docs".to_string(),
            kind: crate::config::agent::AgentToolTargetKind::McpSse,
            url: Some("https://mcp.example.com/sse".to_string()),
            ..AgentToolTargetConfig::default()
        };

        let tools = visible_tools(&auth, &[target], None, 8000);

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_id, "docs.search");
        assert_eq!(tools[0].metadata.target_kind, "mcp-sse");
        assert_eq!(tools[0].metadata.target_id, "docs.search");
    }

    #[test]
    fn framework_tool_name_truncates_before_hash_suffix() {
        let mut seen = std::collections::HashSet::new();
        let long_id = "A".repeat(80);
        let colliding_id = "a".repeat(80);

        let first = framework_tool_name(&long_id, &mut seen);
        let second = framework_tool_name(&colliding_id, &mut seen);

        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        assert_eq!(second.len(), 64);
        assert!(second.starts_with(&"a".repeat(55)));
        assert_eq!(second.as_bytes()[55], b'_');
    }

    #[test]
    fn runtime_descriptor_includes_adapter_mapping_and_availability() {
        let tool = crate::agent::tools::runtime_snapshot::types::RuntimeTool {
            tool_id: "zendesk_get_ticket".to_string(),
            kind: "mcp-streamable-http".to_string(),
            framework_tool_name: "zendesk_get_ticket".to_string(),
            upstream_tool_name: "zendesk.get_ticket".to_string(),
            display_name: "Get ticket".to_string(),
            description: "Fetch ticket".to_string(),
            safe_model_description: "Fetch ticket".to_string(),
            schema_hash: "sha256:schema".to_string(),
            tool_version: 3,
            fixed_micros: 5000,
            currency: "USD".to_string(),
            ..Default::default()
        };

        let descriptor = descriptor_from_runtime_tool(&tool, 42, 17);

        assert_eq!(descriptor.tool_id, "zendesk_get_ticket");
        assert_eq!(descriptor.framework_tool_name, "zendesk_get_ticket");
        assert_eq!(descriptor.metadata.target_kind, "mcp-streamable-http");
        assert_eq!(descriptor.metadata.target_id, "zendesk_get_ticket");
        assert_eq!(descriptor.upstream_tool_name, "zendesk.get_ticket");
        assert_eq!(descriptor.mapping_revision, tool.tool_version);
        assert_eq!(descriptor.name, "Get ticket");
        assert_eq!(descriptor.availability.visibility, "listed_available");
        assert_eq!(descriptor.availability.source, "snapshot");
        assert_eq!(descriptor.availability.reason_code, "");
        assert!(!descriptor.availability.may_become_available);
        assert_eq!(descriptor.cost_policy.fixed_micros, 5000);
    }

    #[test]
    fn runtime_openapi_descriptor_exposes_operation_versions() {
        let tool = crate::agent::tools::runtime_snapshot::types::RuntimeTool {
            tool_id: "tool_support_ticket_get".to_string(),
            kind: "openapi".to_string(),
            framework_tool_name: "support_get_ticket".to_string(),
            upstream_tool_name: "support.getTicket".to_string(),
            display_name: "Get ticket".to_string(),
            description: "Fetch a support ticket".to_string(),
            safe_model_description: "Fetch a support ticket".to_string(),
            schema_hash: "sha256:schema".to_string(),
            rate_card_revision: 7,
            target_revision: 12,
            target: crate::agent::tools::runtime_snapshot::types::RuntimeToolTarget {
                kind: "openapi".to_string(),
                method: "GET".to_string(),
                openapi: crate::agent::tools::openapi::types::RuntimeOpenApiTarget {
                    target_hash: "sha256:target".to_string(),
                    service_slug: "support-api".to_string(),
                    operation_id: "getTicket".to_string(),
                    operation_slug: "get_ticket".to_string(),
                    auth_revision: 4,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };

        let descriptor = descriptor_from_runtime_tool(&tool, 42, 17);

        assert_eq!(descriptor.metadata.target_kind, "openapi");
        assert_eq!(
            descriptor.metadata.service_slug.as_deref(),
            Some("support-api")
        );
        assert_eq!(
            descriptor.metadata.operation_id.as_deref(),
            Some("getTicket")
        );
        assert_eq!(
            descriptor.metadata.operation_slug.as_deref(),
            Some("get_ticket")
        );
        assert_eq!(
            descriptor.metadata.target_hash.as_deref(),
            Some("sha256:target")
        );
        assert_eq!(descriptor.metadata.target_revision, Some(12));
        assert_eq!(
            descriptor.metadata.schema_hash.as_deref(),
            Some("sha256:schema")
        );
        assert_eq!(descriptor.metadata.auth_revision, Some(4));
        assert_eq!(descriptor.metadata.rate_card_revision, Some(7));

        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(value["metadata"]["serviceSlug"], "support-api");
        assert_eq!(value["metadata"]["operationId"], "getTicket");
        assert_eq!(value["metadata"]["targetRevision"], 12);
    }

    #[test]
    fn openapi_descriptor_keeps_tool_id_framework_name_and_operation_id_separate() {
        let tool = crate::agent::tools::runtime_snapshot::types::RuntimeTool {
            tool_id: "tool_01J_support_ticket_get".to_string(),
            kind: "openapi".to_string(),
            framework_tool_name: "support_ticket_lookup".to_string(),
            upstream_tool_name: "support.getTicket".to_string(),
            display_name: "Get ticket".to_string(),
            description: "Fetch a support ticket".to_string(),
            safe_model_description: "Fetch a support ticket".to_string(),
            target: crate::agent::tools::runtime_snapshot::types::RuntimeToolTarget {
                kind: "openapi".to_string(),
                method: "GET".to_string(),
                openapi: crate::agent::tools::openapi::types::RuntimeOpenApiTarget {
                    operation_id: "getTicket".to_string(),
                    operation_slug: "get_ticket".to_string(),
                    service_slug: "support-api".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };

        let descriptor = descriptor_from_runtime_tool(&tool, 42, 17);

        assert_eq!(descriptor.tool_id, "tool_01J_support_ticket_get");
        assert_eq!(descriptor.framework_tool_name, "support_ticket_lookup");
        assert_eq!(
            descriptor.metadata.operation_id.as_deref(),
            Some("getTicket")
        );
        assert_ne!(descriptor.tool_id, descriptor.framework_tool_name);
        assert_ne!(
            Some(descriptor.tool_id.as_str()),
            descriptor.metadata.operation_id.as_deref()
        );
        assert_ne!(
            Some(descriptor.framework_tool_name.as_str()),
            descriptor.metadata.operation_id.as_deref()
        );
    }

    #[test]
    fn static_descriptor_omits_openapi_only_metadata_fields() {
        let target = AgentToolTargetConfig {
            tool_id: "mock.echo".to_string(),
            name: "Mock echo".to_string(),
            description: "Echo input".to_string(),
            ..AgentToolTargetConfig::default()
        };
        let mut seen = HashSet::new();
        let descriptor = descriptor(&target, 1_000, &mut seen);

        let value = serde_json::to_value(&descriptor).unwrap();

        assert_eq!(value["metadata"]["targetKind"], "mock");
        assert!(value["metadata"].get("serviceSlug").is_none());
        assert!(value["metadata"].get("operationId").is_none());
        assert!(value["metadata"].get("targetHash").is_none());
        assert!(value["metadata"].get("targetRevision").is_none());
        assert!(value["metadata"].get("rateCardRevision").is_none());
    }

    fn target_with_vk(tool_id: &str, virtual_key_id: Uuid) -> AgentToolTargetConfig {
        AgentToolTargetConfig {
            tool_id: tool_id.to_string(),
            allowlist: AgentToolAllowlistConfig {
                virtual_key_ids: vec![virtual_key_id],
                ..AgentToolAllowlistConfig::default()
            },
            ..AgentToolTargetConfig::default()
        }
    }

    fn target_with_agent(tool_id: &str, agent_id: &str) -> AgentToolTargetConfig {
        AgentToolTargetConfig {
            tool_id: tool_id.to_string(),
            allowlist: AgentToolAllowlistConfig {
                agent_ids: vec![agent_id.to_string()],
                ..AgentToolAllowlistConfig::default()
            },
            ..AgentToolTargetConfig::default()
        }
    }

    fn auth_context_with_virtual_key(virtual_key_id: Uuid) -> AuthContext {
        AuthContext {
            api_key: Secret::from(String::new()),
            user_id: UserId::new(Uuid::nil()),
            org_id: OrgId::new(Uuid::nil()),
            workspace_type: None,
            virtual_key_id: Some(virtual_key_id),
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
