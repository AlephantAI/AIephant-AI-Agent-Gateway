use std::time::Duration;

use futures::TryStreamExt;
use http::{HeaderMap, header};
use serde::de::DeserializeOwned;

use crate::agent::tools::mcp_streamable_http::sse::{SseAccumulator, SseLimits};

#[derive(Debug, Clone, PartialEq)]
pub struct JsonRpcTransportResponse<T> {
    pub value: T,
    pub headers: HeaderMap,
    pub sse_used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseReadOptions {
    pub limits: SseLimits,
    pub idle_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("mcp response exceeds size limit")]
    ResponseTooLarge,
    #[error("mcp transport unavailable")]
    TargetUnavailable,
    #[error("mcp response is invalid json")]
    ProtocolParse,
    #[error("mcp sse response is invalid")]
    SseParse,
    #[error("mcp sse server request is unsupported")]
    SseServerRequestUnsupported,
    #[error("mcp sse response did not include matching json-rpc response")]
    SseIncomplete,
    #[error("mcp sse response timed out while waiting for an event")]
    SseIdleTimeout,
}

pub async fn read_json_rpc_response<T>(
    response: reqwest::Response,
    expected_id: &str,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
) -> Result<JsonRpcTransportResponse<T>, TransportError>
where
    T: DeserializeOwned,
{
    let headers = response.headers().clone();
    if is_sse_response(&headers) {
        let value =
            read_sse_json_rpc_response(response, expected_id, max_response_bytes, sse_options)
                .await?;
        return Ok(JsonRpcTransportResponse {
            value,
            headers,
            sse_used: true,
        });
    }

    let body = read_limited_body(response, max_response_bytes).await?;
    let value = serde_json::from_slice(&body).map_err(|_| TransportError::ProtocolParse)?;
    Ok(JsonRpcTransportResponse {
        value,
        headers,
        sse_used: false,
    })
}

async fn read_sse_json_rpc_response<T>(
    response: reqwest::Response,
    expected_id: &str,
    max_response_bytes: usize,
    sse_options: &SseReadOptions,
) -> Result<T, TransportError>
where
    T: DeserializeOwned,
{
    let mut accumulator = SseAccumulator::default();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::time::timeout(sse_options.idle_timeout, stream.try_next())
        .await
        .map_err(|_| TransportError::SseIdleTimeout)?
        .map_err(|_| TransportError::TargetUnavailable)?
    {
        if let Some(value) = accumulator
            .push_and_try_find(&chunk, expected_id, &sse_options.limits)
            .map_err(map_sse_error)?
        {
            return serde_json::from_value(value).map_err(|_| TransportError::ProtocolParse);
        }
        if chunk.len() > max_response_bytes {
            return Err(TransportError::ResponseTooLarge);
        }
    }

    Err(TransportError::SseIncomplete)
}

async fn read_limited_body(
    response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<Vec<u8>, TransportError> {
    if response
        .content_length()
        .is_some_and(|len| len > max_response_bytes as u64)
    {
        return Err(TransportError::ResponseTooLarge);
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|_| TransportError::TargetUnavailable)?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or(TransportError::ResponseTooLarge)?;
        if next_len > max_response_bytes {
            return Err(TransportError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

fn is_sse_response(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| {
            content_type
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        })
}

fn map_sse_error(
    error: crate::agent::tools::mcp_streamable_http::sse::SseParseError,
) -> TransportError {
    match error {
        crate::agent::tools::mcp_streamable_http::sse::SseParseError::TotalTooLarge
        | crate::agent::tools::mcp_streamable_http::sse::SseParseError::EventTooLarge
        | crate::agent::tools::mcp_streamable_http::sse::SseParseError::LineTooLarge => {
            TransportError::ResponseTooLarge
        }
        crate::agent::tools::mcp_streamable_http::sse::SseParseError::MatchingResponseMissing => {
            TransportError::SseIncomplete
        }
        crate::agent::tools::mcp_streamable_http::sse::SseParseError::ServerRequestUnsupported => {
            TransportError::SseServerRequestUnsupported
        }
        _ => TransportError::SseParse,
    }
}
