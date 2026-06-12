use sha2::{Digest, Sha256};
use url::Url;

use crate::config::agent::{AgentToolTargetConfig, AgentToolsConfig};

pub fn canonical_mcp_sse_target_hash(
    target: &AgentToolTargetConfig,
    tools_cfg: &AgentToolsConfig,
) -> String {
    let normalized_url = target
        .url
        .as_deref()
        .and_then(normalize_url_for_hash)
        .unwrap_or_default();
    let auth_revision = "0/static";
    let target_revision = "0/static";
    let payload = serde_json::json!({
        "transportKind": "mcp-sse",
        "toolId": target.tool_id,
        "url": normalized_url,
        "method": target.method.to_ascii_uppercase(),
        "timeoutMs": target.timeout_ms.unwrap_or(tools_cfg.timeout_ms),
        "authRevision": auth_revision,
        "targetRevision": target_revision,
        "sseMaxEventBytes": tools_cfg.mcp_sse_max_event_bytes,
        "sseMaxLineBytes": tools_cfg.mcp_sse_max_line_bytes,
        "sseMaxEvents": tools_cfg.mcp_sse_max_events,
        "sseMaxBatchItems": tools_cfg.mcp_sse_max_batch_items,
        "sseIdleTimeoutMs": tools_cfg.mcp_sse_idle_timeout_ms,
    });
    let bytes = serde_json::to_vec(&payload).expect("hash payload serializes");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn normalize_url_for_hash(raw: &str) -> Option<String> {
    let parsed = Url::parse(raw).ok()?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    let host = parsed.host_str()?.to_ascii_lowercase();
    let port = parsed.port();
    let default_port = match scheme.as_str() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let port_part = if port.is_some() && port != default_port {
        format!(":{}", port.expect("port"))
    } else {
        String::new()
    };
    Some(format!("{scheme}://{host}{port_part}{}", parsed.path()))
}

#[cfg(test)]
mod tests {
    use crate::{
        agent::tools::mcp_sse::target_hash::canonical_mcp_sse_target_hash,
        config::agent::{AgentToolTargetConfig, AgentToolTargetKind, AgentToolsConfig},
    };

    fn target(kind: AgentToolTargetKind, url: &str) -> AgentToolTargetConfig {
        AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            kind,
            url: Some(url.to_string()),
            ..AgentToolTargetConfig::default()
        }
    }

    #[test]
    fn target_hash_normalizes_scheme_host_and_default_port() {
        let tools_cfg = AgentToolsConfig::default();
        let a = canonical_mcp_sse_target_hash(
            &target(
                AgentToolTargetKind::McpSse,
                "https://MCP.EXAMPLE.com:443/sse",
            ),
            &tools_cfg,
        );
        let b = canonical_mcp_sse_target_hash(
            &target(AgentToolTargetKind::McpSse, "https://mcp.example.com/sse"),
            &tools_cfg,
        );

        assert_eq!(a, b);
    }

    #[test]
    fn target_hash_is_separate_from_streamable_http() {
        let tools_cfg = AgentToolsConfig::default();
        let sse = canonical_mcp_sse_target_hash(
            &target(AgentToolTargetKind::McpSse, "https://mcp.example.com/sse"),
            &tools_cfg,
        );
        let streamable =
            crate::agent::tools::mcp_streamable_http::target_hash::canonical_target_hash(
                &target(
                    AgentToolTargetKind::McpStreamableHttp,
                    "https://mcp.example.com/sse",
                ),
                0,
                "0/static",
                &tools_cfg,
            );

        assert_ne!(sse, streamable);
    }

    #[test]
    fn target_hash_omits_query_string_secrets() {
        let tools_cfg = AgentToolsConfig::default();
        let with_secret = canonical_mcp_sse_target_hash(
            &target(
                AgentToolTargetKind::McpSse,
                "https://mcp.example.com/sse?token=secret",
            ),
            &tools_cfg,
        );
        let without_secret = canonical_mcp_sse_target_hash(
            &target(AgentToolTargetKind::McpSse, "https://mcp.example.com/sse"),
            &tools_cfg,
        );

        assert_eq!(with_secret, without_secret);
    }
}
