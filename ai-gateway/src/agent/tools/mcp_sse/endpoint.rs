#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MessageEndpointError {
    #[error("mcp sse message endpoint is invalid")]
    Invalid,
    #[error("mcp sse message endpoint must use same origin")]
    CrossOrigin,
    #[error("mcp sse message endpoint is unsafe")]
    UnsafeEndpoint,
}

pub fn resolve_message_endpoint(
    sse_url: &str,
    endpoint: &str,
) -> Result<url::Url, MessageEndpointError> {
    let base = url::Url::parse(sse_url).map_err(|_| MessageEndpointError::Invalid)?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(MessageEndpointError::UnsafeEndpoint);
    }

    let resolved = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        url::Url::parse(endpoint).map_err(|_| MessageEndpointError::Invalid)?
    } else {
        base.join(endpoint)
            .map_err(|_| MessageEndpointError::Invalid)?
    };

    if !matches!(resolved.scheme(), "http" | "https")
        || !resolved.username().is_empty()
        || resolved.password().is_some()
        || resolved.fragment().is_some()
    {
        return Err(MessageEndpointError::UnsafeEndpoint);
    }
    if base.scheme() != resolved.scheme()
        || base.host_str() != resolved.host_str()
        || base.port_or_known_default() != resolved.port_or_known_default()
    {
        return Err(MessageEndpointError::CrossOrigin);
    }

    Ok(resolved)
}

pub fn extract_endpoint_event(sse_event: &str) -> Result<String, MessageEndpointError> {
    let mut event_type = None;
    let mut data_lines = Vec::new();

    for line in sse_event.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start());
        }
    }

    if event_type != Some("endpoint") || data_lines.is_empty() {
        return Err(MessageEndpointError::Invalid);
    }

    let endpoint = data_lines.join("\n").trim().to_string();
    if endpoint.is_empty() {
        return Err(MessageEndpointError::Invalid);
    }
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::{MessageEndpointError, extract_endpoint_event};
    use crate::agent::tools::mcp_sse::endpoint::resolve_message_endpoint;

    #[test]
    fn resolves_relative_message_endpoint_against_sse_url() {
        let resolved =
            resolve_message_endpoint("https://mcp.example.com/sse", "/messages/session-1")
                .expect("endpoint");

        assert_eq!(
            resolved.as_str(),
            "https://mcp.example.com/messages/session-1"
        );
    }

    #[test]
    fn accepts_same_origin_absolute_message_endpoint() {
        let resolved = resolve_message_endpoint(
            "https://mcp.example.com/sse",
            "https://mcp.example.com/messages/session-1",
        )
        .expect("endpoint");

        assert_eq!(
            resolved.as_str(),
            "https://mcp.example.com/messages/session-1"
        );
    }

    #[test]
    fn rejects_cross_origin_message_endpoint() {
        let err = resolve_message_endpoint(
            "https://mcp.example.com/sse",
            "https://evil.example.com/messages/session-1",
        )
        .expect_err("cross origin endpoint");

        assert_eq!(err, MessageEndpointError::CrossOrigin);
    }

    #[test]
    fn rejects_endpoint_with_userinfo() {
        let err = resolve_message_endpoint(
            "https://mcp.example.com/sse",
            "https://user:pass@mcp.example.com/messages",
        )
        .expect_err("userinfo");

        assert_eq!(err, MessageEndpointError::UnsafeEndpoint);
    }

    #[test]
    fn rejects_endpoint_fragment() {
        let err =
            resolve_message_endpoint("https://mcp.example.com/sse", "/messages/session-1#secret")
                .expect_err("fragment");

        assert_eq!(err, MessageEndpointError::UnsafeEndpoint);
    }

    #[test]
    fn extracts_endpoint_from_sse_event() {
        let event = "event: endpoint\ndata: /messages/session-1\n\n";

        let endpoint = extract_endpoint_event(event).expect("endpoint");

        assert_eq!(endpoint, "/messages/session-1");
    }

    #[test]
    fn rejects_non_endpoint_sse_event() {
        let err = extract_endpoint_event("event: message\ndata: /messages\n\n")
            .expect_err("non-endpoint event");

        assert_eq!(err, MessageEndpointError::Invalid);
    }
}
