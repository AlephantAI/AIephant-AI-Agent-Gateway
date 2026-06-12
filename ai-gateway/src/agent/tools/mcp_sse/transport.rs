use futures::TryStreamExt;

use crate::agent::tools::mcp_streamable_http::{
    sse::{SseAccumulator, SseLimits, SseParseError},
    transport::SseReadOptions,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpSseTransportError {
    #[error("mcp sse transport unavailable")]
    TargetUnavailable,
    #[error("mcp sse response timed out while waiting for an event")]
    IdleTimeout,
    #[error("mcp sse response did not include matching json-rpc response")]
    Incomplete,
    #[error("mcp sse response exceeds size limit")]
    ResponseTooLarge,
    #[error("mcp sse response is invalid")]
    Parse,
    #[error("mcp sse server request is unsupported")]
    ServerRequestUnsupported,
}

pub async fn read_matching_json_rpc_from_sse(
    response: reqwest::Response,
    expected_id: &str,
    options: &SseReadOptions,
) -> Result<serde_json::Value, McpSseTransportError> {
    let mut stream = response.bytes_stream();
    let mut accumulator = SseAccumulator::default();

    while let Some(chunk) = tokio::time::timeout(options.idle_timeout, stream.try_next())
        .await
        .map_err(|_| McpSseTransportError::IdleTimeout)?
        .map_err(|_| McpSseTransportError::TargetUnavailable)?
    {
        if let Some(value) =
            push_chunk_and_find(&mut accumulator, &chunk, expected_id, &options.limits)?
        {
            return Ok(value);
        }
    }

    Err(McpSseTransportError::Incomplete)
}

fn push_chunk_and_find(
    accumulator: &mut SseAccumulator,
    chunk: &[u8],
    expected_id: &str,
    limits: &SseLimits,
) -> Result<Option<serde_json::Value>, McpSseTransportError> {
    accumulator
        .push_and_try_find(chunk, expected_id, limits)
        .map_err(map_sse_error)
}

fn map_sse_error(error: SseParseError) -> McpSseTransportError {
    match error {
        SseParseError::TotalTooLarge
        | SseParseError::EventTooLarge
        | SseParseError::LineTooLarge
        | SseParseError::BatchTooLarge => McpSseTransportError::ResponseTooLarge,
        SseParseError::MatchingResponseMissing => McpSseTransportError::Incomplete,
        SseParseError::ServerRequestUnsupported => McpSseTransportError::ServerRequestUnsupported,
        SseParseError::InvalidUtf8 | SseParseError::InvalidJson => McpSseTransportError::Parse,
        SseParseError::TooManyEvents => McpSseTransportError::ResponseTooLarge,
    }
}

#[cfg(test)]
async fn read_matching_response_from_chunks(
    chunks: Vec<bytes::Bytes>,
    expected_id: &str,
    options: SseReadOptions,
) -> Result<serde_json::Value, McpSseTransportError> {
    let mut accumulator = SseAccumulator::default();
    for chunk in chunks {
        if let Some(value) =
            push_chunk_and_find(&mut accumulator, &chunk, expected_id, &options.limits)?
        {
            return Ok(value);
        }
        tokio::time::sleep(std::time::Duration::from_millis(0)).await;
    }

    Err(McpSseTransportError::Incomplete)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{McpSseTransportError, read_matching_response_from_chunks};
    use crate::agent::tools::mcp_streamable_http::{sse::SseLimits, transport::SseReadOptions};

    fn test_sse_options() -> SseReadOptions {
        SseReadOptions {
            limits: SseLimits::default(),
            idle_timeout: std::time::Duration::from_millis(50),
        }
    }

    #[tokio::test]
    async fn sse_stream_reader_waits_for_matching_json_rpc_id() {
        let chunks = vec![
            Bytes::from_static(b": keepalive\n\n"),
            Bytes::from_static(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"other\",\"result\":{}}\n\n",
            ),
            Bytes::from_static(
                b"data: {\"jsonrpc\":\"2.0\",\"id\":\"call-1\",\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n\n",
            ),
        ];

        let value = read_matching_response_from_chunks(chunks, "call-1", test_sse_options())
            .await
            .expect("matching response");

        assert_eq!(value["result"]["content"][0]["text"], "ok");
    }

    #[tokio::test]
    async fn sse_stream_reader_rejects_server_requests() {
        let chunks = vec![Bytes::from_static(
            b"data: {\"jsonrpc\":\"2.0\",\"id\":\"server-1\",\"method\":\"tools/list\"}\n\n",
        )];

        let err = read_matching_response_from_chunks(chunks, "call-1", test_sse_options())
            .await
            .expect_err("server request");

        assert_eq!(err, McpSseTransportError::ServerRequestUnsupported);
    }
}
