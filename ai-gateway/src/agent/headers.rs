use http::HeaderMap;

use crate::agent::context::{
    AgentConfidence, AgentContext, AgentStepKind, AgentStepSource, AgentTrustLevel,
};

pub const AGENT_ID: &str = "alephant-agent-id";
pub const AGENT_NAME: &str = "alephant-agent-name";
pub const RUN_ID: &str = "alephant-run-id";
pub const STEP_ID: &str = "alephant-step-id";
pub const PARENT_STEP_ID: &str = "alephant-parent-step-id";
pub const TOOL_CALL_ID: &str = "alephant-tool-call-id";
pub const HANDOFF_ID: &str = "alephant-handoff-id";
pub const GRAPH_NODE: &str = "alephant-graph-node";
pub const ITERATION: &str = "alephant-iteration";
pub const STATE_HASH: &str = "alephant-state-hash";
pub const STEP_KIND: &str = "alephant-step-kind";
pub const STEP_SOURCE: &str = "alephant-step-source";
pub const STEP_ATTEMPT: &str = "alephant-step-attempt";
pub const STEP_INPUT_HASH: &str = "alephant-step-input-hash";

const AGENT_HEADER_PREFIX: &str = "alephant-agent-";
const EXTRA_AGENT_HEADERS: [&str; 9] = [
    RUN_ID,
    STEP_ID,
    PARENT_STEP_ID,
    TOOL_CALL_ID,
    HANDOFF_ID,
    GRAPH_NODE,
    ITERATION,
    STATE_HASH,
    STEP_KIND,
];

#[must_use]
pub fn is_agent_header_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(AGENT_HEADER_PREFIX)
        || lower == STEP_SOURCE
        || lower == STEP_ATTEMPT
        || lower == STEP_INPUT_HASH
        || EXTRA_AGENT_HEADERS.contains(&lower.as_str())
}

#[must_use]
pub fn parse_agent_context_from_headers(
    headers: &HeaderMap,
    max_value_bytes: usize,
) -> Option<AgentContext> {
    let get = |name: &str| header_string(headers, name, max_value_bytes);

    let mut ctx = AgentContext {
        agent_id_external: get(AGENT_ID),
        agent_name: get(AGENT_NAME),
        run_id: get(RUN_ID),
        step_id: get(STEP_ID),
        parent_step_id: get(PARENT_STEP_ID),
        tool_call_id: get(TOOL_CALL_ID),
        handoff_id: get(HANDOFF_ID),
        graph_node: get(GRAPH_NODE),
        iteration: get(ITERATION).and_then(|s| s.parse::<u32>().ok()),
        state_hash: get(STATE_HASH),
        step_kind: get(STEP_KIND).map(|s| {
            s.parse::<AgentStepKind>()
                .expect("AgentStepKind parser is infallible")
        }),
        step_source: get(STEP_SOURCE)
            .map(|s| {
                s.parse::<AgentStepSource>()
                    .expect("AgentStepSource parser is infallible")
            })
            .unwrap_or_default(),
        ..AgentContext::default()
    };

    if ctx.is_empty() {
        return None;
    }

    ctx.partial = ctx.agent_id_external.is_none() || ctx.run_id.is_none();
    ctx.step_confidence = AgentConfidence::High;
    ctx.trust_level = AgentTrustLevel::SelfReported;

    Some(ctx)
}

fn header_string(headers: &HeaderMap, name: &str, max_value_bytes: usize) -> Option<String> {
    let value = headers.get(name)?;
    let text = value.to_str().ok()?.trim();
    if text.is_empty() || text.len() > max_value_bytes {
        return None;
    }
    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};

    use super::*;
    use crate::agent::context::{AgentStepKind, AgentStepSource, AgentTrustLevel};

    #[test]
    fn parses_full_agent_context_from_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID, HeaderValue::from_static("coding-agent"));
        headers.insert(AGENT_NAME, HeaderValue::from_static("Support Bot"));
        headers.insert(RUN_ID, HeaderValue::from_static("run-1"));
        headers.insert(STEP_ID, HeaderValue::from_static("step-1"));
        headers.insert(PARENT_STEP_ID, HeaderValue::from_static("step-0"));
        headers.insert(TOOL_CALL_ID, HeaderValue::from_static("call-1"));
        headers.insert(GRAPH_NODE, HeaderValue::from_static("planner"));
        headers.insert(ITERATION, HeaderValue::from_static("3"));
        headers.insert(STATE_HASH, HeaderValue::from_static("sha256:abc"));
        headers.insert(STEP_KIND, HeaderValue::from_static("planning"));
        headers.insert(STEP_SOURCE, HeaderValue::from_static("runtime"));

        let ctx = parse_agent_context_from_headers(&headers, 256).unwrap();

        assert_eq!(ctx.agent_id_external.as_deref(), Some("coding-agent"));
        assert_eq!(ctx.agent_name.as_deref(), Some("Support Bot"));
        assert_eq!(ctx.run_id.as_deref(), Some("run-1"));
        assert_eq!(ctx.step_id.as_deref(), Some("step-1"));
        assert_eq!(ctx.parent_step_id.as_deref(), Some("step-0"));
        assert_eq!(ctx.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(ctx.graph_node.as_deref(), Some("planner"));
        assert_eq!(ctx.iteration, Some(3));
        assert_eq!(ctx.state_hash.as_deref(), Some("sha256:abc"));
        assert_eq!(ctx.step_kind, Some(AgentStepKind::Planning));
        assert_eq!(ctx.step_source, AgentStepSource::Runtime);
        assert_eq!(ctx.trust_level, AgentTrustLevel::SelfReported);
        assert!(!ctx.partial);
    }

    #[test]
    fn empty_headers_return_none() {
        let headers = HeaderMap::new();
        assert!(parse_agent_context_from_headers(&headers, 256).is_none());
    }

    #[test]
    fn agent_name_only_returns_partial_context_without_identity() {
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_NAME, HeaderValue::from_static("Support Bot"));

        let ctx = parse_agent_context_from_headers(&headers, 256).unwrap();

        assert_eq!(ctx.agent_name.as_deref(), Some("Support Bot"));
        assert_eq!(ctx.agent_id_external, None);
        assert_eq!(ctx.run_id, None);
        assert!(ctx.partial);
        assert_eq!(ctx.agent_identity_for_namespace(None, None), "unknown");
    }

    #[test]
    fn invalid_numbers_are_ignored_without_panicking() {
        let mut headers = HeaderMap::new();
        headers.insert(RUN_ID, HeaderValue::from_static("run-1"));
        headers.insert(ITERATION, HeaderValue::from_static("abc"));

        let ctx = parse_agent_context_from_headers(&headers, 256).unwrap();

        assert_eq!(ctx.run_id.as_deref(), Some("run-1"));
        assert_eq!(ctx.iteration, None);
        assert!(ctx.partial);
    }

    #[test]
    fn only_invalid_numbers_return_none() {
        let mut headers = HeaderMap::new();
        headers.insert(ITERATION, HeaderValue::from_static("abc"));

        assert!(parse_agent_context_from_headers(&headers, 256).is_none());
    }

    #[test]
    fn oversized_header_value_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID, HeaderValue::from_static("coding-agent"));
        headers.insert(RUN_ID, HeaderValue::from_static("run-1"));
        headers.insert(STEP_ID, HeaderValue::from_static("s1"));

        let ctx = parse_agent_context_from_headers(&headers, 3).unwrap();

        assert_eq!(ctx.agent_id_external, None);
        assert_eq!(ctx.run_id, None);
        assert_eq!(ctx.step_id.as_deref(), Some("s1"));
    }

    #[test]
    fn only_oversized_known_headers_return_none() {
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_ID, HeaderValue::from_static("coding-agent"));
        headers.insert(RUN_ID, HeaderValue::from_static("run-1"));

        assert!(parse_agent_context_from_headers(&headers, 3).is_none());
    }

    #[test]
    fn only_unknown_agent_header_returns_none() {
        let mut headers = HeaderMap::new();
        headers.insert("alephant-agent-foo", HeaderValue::from_static("ignored"));

        assert!(parse_agent_context_from_headers(&headers, 256).is_none());
    }

    #[test]
    fn identifies_agent_header_names() {
        assert!(is_agent_header_name("Alephant-Agent-Id"));
        assert!(is_agent_header_name("Alephant-Agent-Name"));
        assert!(is_agent_header_name("alephant-run-id"));
        assert!(is_agent_header_name("ALEPHANT-STEP-ID"));
        assert!(!is_agent_header_name("alephant-api-key"));
    }
}
