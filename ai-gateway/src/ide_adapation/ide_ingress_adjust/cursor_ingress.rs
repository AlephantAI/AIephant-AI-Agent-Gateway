//! Cursor-specific IDE ingress normalization (Phase A + 9router parity).
//!
//! Ports a **safe subset** of 9router `open-sse/translator` behaviour for
//! Cursor-identified clients on the OpenAI Chat Completions ingress path:
//!
//! - [`super::cursor_openai_normalize`] — `ensureToolCallIds` /
//!   `fixMissingToolResponses` from `helpers/toolCallHelper.js`, plus stripping
//!   `index` on `tool_calls` (see `request/openai-to-cursor.js`).
//!
//! **Intentionally not ported:** `buildCursorRequest` / `convertMessages` in
//! `openai-to-cursor.js` (system→user + XML `tool_result` blocks, forced
//! `max_tokens`) — those target **FORMAT.CURSOR** upstream executors; this
//! gateway forwards to OpenAI-compatible providers via `EndpointConverter`.

use async_openai::types::CreateChatCompletionRequest;
use bytes::Bytes;
use serde_json::Value;

use crate::{
    endpoints::{ApiEndpoint, openai::OpenAI},
    error::{api::ApiError, invalid_req::InvalidRequestError},
};

/// Cursor ingress hook: **OpenAI chat completions** inbound only.
///
/// Returns `(body, applied)` where `applied` is `true` when this hook ran
/// (including semantic no-op on bytes).
pub fn adjust(source_endpoint: &ApiEndpoint, body: Bytes) -> Result<(Bytes, bool), ApiError> {
    if !matches!(
        source_endpoint,
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_))
    ) {
        return Ok((body, false));
    }

    let mut value: Value =
        serde_json::from_slice(&body).map_err(InvalidRequestError::InvalidRequestBody)?;

    let mutated =
        super::cursor_openai_normalize::normalize_cursor_openai_request_value(&mut value)?;

    let out = if mutated {
        Bytes::from(serde_json::to_vec(&value).map_err(InvalidRequestError::from)?)
    } else {
        body
    };

    let _: CreateChatCompletionRequest =
        serde_json::from_slice(&out).map_err(InvalidRequestError::InvalidRequestBody)?;

    tracing::trace!(
        mutated = mutated,
        "cursor_ingress: chat completions normalized (9router toolCallHelper \
         parity)"
    );

    Ok((out, true))
}
