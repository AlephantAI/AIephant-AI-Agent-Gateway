use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::agent::{AgentToolTargetConfig, AgentToolsConfig};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalMcpTargetIdentity<'a> {
    target_id: &'a str,
    target_revision: u64,
    transport_kind: &'static str,
    normalized_url: String,
    method: String,
    timeout_ms: u64,
    auth_revision: &'a str,
    header_template_names: Vec<String>,
    tool_namespace: &'a str,
    sse_max_event_bytes: usize,
    sse_max_line_bytes: usize,
    sse_max_events: usize,
    sse_max_batch_items: usize,
    sse_idle_timeout_ms: u64,
}

pub fn canonical_target_hash(
    target: &AgentToolTargetConfig,
    target_revision: u64,
    auth_revision: &str,
    tools_cfg: &AgentToolsConfig,
) -> String {
    let normalized_url = normalize_url(target.url.as_deref().unwrap_or(""));
    let identity = CanonicalMcpTargetIdentity {
        target_id: &target.tool_id,
        target_revision,
        transport_kind: "mcp-streamable-http",
        normalized_url,
        method: target.method.to_ascii_uppercase(),
        timeout_ms: target.timeout_ms.unwrap_or(tools_cfg.timeout_ms),
        auth_revision,
        header_template_names: Vec::new(),
        tool_namespace: &target.tool_id,
        sse_max_event_bytes: tools_cfg.mcp_sse_max_event_bytes,
        sse_max_line_bytes: tools_cfg.mcp_sse_max_line_bytes,
        sse_max_events: tools_cfg.mcp_sse_max_events,
        sse_max_batch_items: tools_cfg.mcp_sse_max_batch_items,
        sse_idle_timeout_ms: tools_cfg.mcp_sse_idle_timeout_ms,
    };
    let bytes = serde_json::to_vec(&identity).expect("target hash serializes");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn normalize_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return value.to_string();
    };
    if let Some(host) = url.host_str().map(str::to_ascii_lowercase) {
        let _ = url.set_host(Some(&host));
    }
    if matches!(
        (url.scheme(), url.port()),
        ("http", Some(80)) | ("https", Some(443))
    ) {
        let _ = url.set_port(None);
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::agent::{AgentToolTargetConfig, AgentToolTargetKind, AgentToolsConfig};

    #[test]
    fn target_hash_normalizes_host_case_and_default_port() {
        let cfg = AgentToolsConfig::default();
        let left = target("HTTPS://MCP.EXAMPLE.COM:443/mcp");
        let right = target("https://mcp.example.com/mcp");

        assert_eq!(
            canonical_target_hash(&left, 7, "auth-v1", &cfg),
            canonical_target_hash(&right, 7, "auth-v1", &cfg)
        );
    }

    #[test]
    fn target_hash_changes_when_cost_relevant_identity_changes() {
        let cfg = AgentToolsConfig::default();
        let base =
            canonical_target_hash(&target("https://mcp.example.com/mcp"), 7, "auth-v1", &cfg);

        let mut changed_cfg = cfg.clone();
        changed_cfg.mcp_sse_max_event_bytes += 1;
        assert_ne!(
            base,
            canonical_target_hash(
                &target("https://mcp.example.com/mcp"),
                7,
                "auth-v1",
                &changed_cfg
            )
        );
        assert_ne!(
            base,
            canonical_target_hash(&target("https://mcp.example.com/other"), 7, "auth-v1", &cfg)
        );
        assert_ne!(
            base,
            canonical_target_hash(&target("https://mcp.example.com/mcp"), 8, "auth-v1", &cfg)
        );
        assert_ne!(
            base,
            canonical_target_hash(&target("https://mcp.example.com/mcp"), 7, "auth-v2", &cfg)
        );
    }

    fn target(url: &str) -> AgentToolTargetConfig {
        AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpStreamableHttp,
            url: Some(url.to_string()),
            ..AgentToolTargetConfig::default()
        }
    }
}
