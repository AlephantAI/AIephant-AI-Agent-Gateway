use serde::{Deserialize, Serialize};

pub fn active_pointer_key(prefix: &str, workspace_id: &str) -> String {
    format!("{prefix}:{workspace_id}:active")
}

pub fn revision_key(prefix: &str, workspace_id: &str, snapshot_revision: i64) -> String {
    format!("{prefix}:{workspace_id}:rev:{snapshot_revision}")
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotSource {
    #[default]
    Static,
    Redis,
    Lkg,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeActivePointer {
    pub schema_version: String,
    pub workspace_id: String,
    pub snapshot_revision: u64,
    pub active_pointer_revision: u64,
    pub redis_key: String,
    pub activated_at: String,
    pub revision_key: String,
    pub payload_hash: String,
    pub toolset_hash: String,
    pub source: SnapshotSource,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct SnapshotEnvelope {
    pub workspace_id: String,
    pub snapshot_revision: u64,
    pub active_pointer_revision: u64,
    pub policy_revision: u64,
    pub payload_hash: String,
    pub toolset_hash: String,
    pub source: SnapshotSource,
    pub active_pointer: RuntimeActivePointer,
    pub snapshot: RuntimeSnapshot,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub workspace_id: String,
    pub snapshot_revision: u64,
    pub active_pointer_revision: u64,
    pub payload_hash: String,
    pub toolset_hash: String,
    pub policy_revision: u64,
    pub source: SnapshotSource,
    pub tools: Vec<RuntimeTool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeTool {
    pub tool_id: String,
    pub kind: String,
    pub framework_tool_name: String,
    pub upstream_tool_name: String,
    pub display_name: String,
    pub safe_model_description: String,
    pub name: String,
    pub description: String,
    pub tool_version: u64,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub schema_version: String,
    pub schema_hash: String,
    pub risk_level: String,
    pub approval_mode: String,
    pub charge_on_failure: bool,
    pub fixed_micros: u64,
    pub currency: String,
    pub timeout_ms: u64,
    pub rate_card: RuntimeRateCard,
    pub rate_card_revision: u64,
    pub target: RuntimeToolTarget,
    pub target_revision: u64,
    pub version_vector: VersionVector,
}

impl Default for RuntimeTool {
    fn default() -> Self {
        Self {
            tool_id: String::new(),
            kind: "mock".to_string(),
            framework_tool_name: String::new(),
            upstream_tool_name: String::new(),
            display_name: String::new(),
            safe_model_description: String::new(),
            name: String::new(),
            description: String::new(),
            tool_version: 0,
            input_schema: serde_json::Value::Null,
            output_schema: serde_json::Value::Null,
            schema_version: String::new(),
            schema_hash: String::new(),
            risk_level: String::new(),
            approval_mode: String::new(),
            charge_on_failure: false,
            fixed_micros: 0,
            currency: "USD".to_string(),
            timeout_ms: 0,
            rate_card: RuntimeRateCard::default(),
            rate_card_revision: 0,
            target: RuntimeToolTarget::default(),
            target_revision: 0,
            version_vector: VersionVector::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeRateCard {
    pub currency: String,
    pub fixed_micros: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeToolTarget {
    pub kind: String,
    pub url: Option<String>,
    pub method: String,
    #[serde(
        flatten,
        skip_serializing_if = "crate::agent::tools::openapi::types::RuntimeOpenApiTarget::is_empty"
    )]
    pub openapi: crate::agent::tools::openapi::types::RuntimeOpenApiTarget,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionVector {
    pub snapshot_revision: u64,
    pub active_pointer_revision: u64,
    pub payload_hash: String,
    pub toolset_hash: String,
    pub policy_revision: u64,
    pub tool_id: String,
    pub tool_version: u64,
    pub schema_hash: String,
    pub rate_card_revision: u64,
    pub target_revision: u64,
}

impl VersionVector {
    pub fn from_tool(
        snapshot_revision: u64,
        active_pointer_revision: u64,
        payload_hash: &str,
        toolset_hash: &str,
        policy_revision: u64,
        tool: &RuntimeTool,
    ) -> Self {
        Self {
            snapshot_revision,
            active_pointer_revision,
            payload_hash: payload_hash.to_string(),
            toolset_hash: toolset_hash.to_string(),
            policy_revision,
            tool_id: tool.tool_id.clone(),
            tool_version: tool.tool_version,
            schema_hash: tool.schema_hash.clone(),
            rate_card_revision: tool.rate_card_revision,
            target_revision: tool.target_revision,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_pointer_derives_revision_key_from_workspace_and_revision() {
        let key = revision_key("alephant:agent-tools:v1", "workspace-1", 42);

        assert_eq!(key, "alephant:agent-tools:v1:workspace-1:rev:42");
    }

    #[test]
    fn runtime_tool_version_vector_contains_replay_fields() {
        let tool = RuntimeTool {
            tool_id: "support.echo".to_string(),
            tool_version: 3,
            schema_hash: "sha256:schema".to_string(),
            rate_card_revision: 2,
            target_revision: 4,
            ..RuntimeTool::default()
        };
        let vector = VersionVector::from_tool(42, 8, "sha256:payload", "sha256:toolset", 17, &tool);

        assert_eq!(vector.snapshot_revision, 42);
        assert_eq!(vector.active_pointer_revision, 8);
        assert_eq!(vector.payload_hash, "sha256:payload");
        assert_eq!(vector.toolset_hash, "sha256:toolset");
        assert_eq!(vector.policy_revision, 17);
        assert_eq!(vector.target_revision, 4);
    }

    #[test]
    fn runtime_snapshot_deserializes_openapi_target_metadata() {
        let raw = r#"{
          "toolId": "support.get-ticket",
          "kind": "openapi",
          "frameworkToolName": "support_get_ticket",
          "displayName": "Get ticket",
          "safeModelDescription": "Fetch a support ticket",
          "schemaHash": "sha256:schema",
          "rateCardRevision": 7,
          "targetRevision": 12,
          "target": {
            "kind": "openapi",
            "method": "GET",
            "targetHash": "sha256:target",
            "serviceSlug": "support-api",
            "operationId": "getTicket",
            "operationSlug": "get_ticket",
            "authRevision": 4,
            "baseUrl": "https://api.example.test",
            "canonicalHost": "api.example.test",
            "allowedScheme": "https",
            "allowedPort": 443,
            "pathTemplate": "/v1/tickets/{ticket_id}",
            "maxResponseBytes": 65536
          }
        }"#;

        let tool: RuntimeTool = serde_json::from_str(raw).unwrap();

        assert_eq!(tool.target.kind, "openapi");
        assert_eq!(tool.target.method, "GET");
        assert_eq!(tool.target.openapi.service_slug, "support-api");
        assert_eq!(tool.target.openapi.target_hash, "sha256:target");
        assert_eq!(tool.target.openapi.auth_revision, 4);
    }

    #[test]
    fn runtime_snapshot_openapi_method_stays_on_outer_target() {
        let raw = r#"{
          "toolId": "support.create-ticket",
          "kind": "openapi",
          "target": {
            "kind": "openapi",
            "method": "POST",
            "targetHash": "sha256:target",
            "serviceSlug": "support-api",
            "operationId": "createTicket",
            "baseUrl": "https://api.example.test",
            "pathTemplate": "/v1/tickets"
          }
        }"#;

        let tool: RuntimeTool = serde_json::from_str(raw).unwrap();

        assert_eq!(tool.target.kind, "openapi");
        assert_eq!(tool.target.method, "POST");
        assert_eq!(tool.target.openapi.operation_id, "createTicket");
    }

    #[test]
    fn runtime_tool_target_serializes_legacy_targets_without_openapi_fields() {
        let target = RuntimeToolTarget {
            kind: "http".to_string(),
            url: Some("https://tools.example.test/call".to_string()),
            method: "POST".to_string(),
            openapi: Default::default(),
        };

        let value = serde_json::to_value(target).unwrap();

        assert_eq!(value["kind"], "http");
        assert_eq!(value["url"], "https://tools.example.test/call");
        assert_eq!(value["method"], "POST");
        assert!(value.get("targetHash").is_none());
        assert!(value.get("serviceSlug").is_none());
        assert!(value.get("baseUrl").is_none());
        assert!(value.get("parameterMapping").is_none());
    }
}
