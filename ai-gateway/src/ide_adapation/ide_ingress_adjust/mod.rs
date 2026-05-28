//! IDE inbound preprocessing keyed by [`ClientProfile`]. Phase A runs Cursor
//! behaviour only; other profiles are strict no-ops without touching bytes.
//!
//! OpenAI `chat/completions` wire normalizations that are safe for **all**
//! clients (assistant content folding, tool-call hygiene) run from
//! [`apply_global_chat_completions_wire_normalize`] in `map_request` **before**
//! VK policy / strict `CreateChatCompletionRequest` serde — so unidentified
//! Cursor stacks still get the same body fixes.

mod cursor_ingress;
mod cursor_openai_normalize;

use bytes::Bytes;
use http::Extensions;
use serde_json::Value;

use crate::{
    endpoints::ApiEndpoint,
    error::{api::ApiError, invalid_req::InvalidRequestError},
    ide_adapation::client_profile::ClientProfile,
};

/// Applies [`cursor_openai_normalize::normalize_cursor_openai_request_value`]
/// to any `POST …/chat/completions` JSON body before mapper strict parsing.
/// No-op when JSON has no `messages` array or normalization yields no change.
pub fn apply_global_chat_completions_wire_normalize(
    body: Bytes,
) -> Result<Bytes, ApiError> {
    let mut value: Value = serde_json::from_slice(&body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let changed =
        cursor_openai_normalize::normalize_cursor_openai_request_value(
            &mut value,
        )?;
    if !changed {
        return Ok(body);
    }
    Ok(Bytes::from(
        serde_json::to_vec(&value).map_err(InvalidRequestError::from)?,
    ))
}

/// Metadata from one `apply_ide_ingress_adjust` call (logging / metrics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdeIngressAdjustMeta {
    pub applied: bool,
    pub profile_label: &'static str,
}

/// Applies profile-specific IDE ingress adjustments to `body`.
///
/// Inserted in `map_request` after VK policy and before `RequestEnvelope`
/// construction (see implementation plan §0).
pub fn apply_ide_ingress_adjust(
    profile: ClientProfile,
    source_endpoint: &ApiEndpoint,
    body: Bytes,
    _extensions: &Extensions,
) -> Result<(Bytes, IdeIngressAdjustMeta), ApiError> {
    let profile_label = profile.as_otel_label();
    match profile {
        ClientProfile::CursorIde => {
            let (body, applied) =
                cursor_ingress::adjust(source_endpoint, body)?;
            Ok((
                body,
                IdeIngressAdjustMeta {
                    applied,
                    profile_label,
                },
            ))
        }
        _ => Ok((
            body,
            IdeIngressAdjustMeta {
                applied: false,
                profile_label,
            },
        )),
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::CreateChatCompletionRequest;
    use bytes::Bytes;
    use http::Extensions;
    use serde_json::json;

    use super::{
        ClientProfile, apply_global_chat_completions_wire_normalize,
        apply_ide_ingress_adjust,
    };
    use crate::endpoints::{ApiEndpoint, openai::OpenAI};

    #[test]
    fn global_wire_normalize_enables_strict_de_without_cursor_profile() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "anthropic/claude-sonnet-4.5",
                "messages": [
                    {
                    "role": "assistant",
                    "content": [
                        {"type": "reasoning", "text": "r"},
                        {"type": "text", "text": "ok"}
                    ]
                    },
                    {
                        "role": "user",
                        "content": [
                            {"type": "input_text", "text": "u"},
                            {"type": "reasoning", "text": "hidden"}
                        ]
                    }
                ]
            }))
            .unwrap(),
        );
        let out = apply_global_chat_completions_wire_normalize(body).unwrap();
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&out).expect("strict OpenAI serde");
    }

    #[test]
    fn unknown_profile_leaves_body_unchanged_even_on_chat_path() {
        let raw = r#"{"model":"x","messages":[{"role":"user","content":"y"}]}"#;
        let body = Bytes::from(raw);
        let ext = Extensions::new();
        let ep = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let (out, meta) = apply_ide_ingress_adjust(
            ClientProfile::Unknown,
            &ep,
            body.clone(),
            &ext,
        )
        .expect("unknown profile should not parse or error");
        assert_eq!(out, body);
        assert!(!meta.applied);
    }

    #[test]
    fn unknown_profile_non_chat_path_bytes_unchanged() {
        let body = Bytes::from(r#"{"not":"chat"}"#);
        let ext = Extensions::new();
        let ep = ApiEndpoint::OpenAI(OpenAI::embeddings());
        let (out, meta) = apply_ide_ingress_adjust(
            ClientProfile::Unknown,
            &ep,
            body.clone(),
            &ext,
        )
        .expect("unknown should be no-op");
        assert_eq!(out, body);
        assert!(!meta.applied);
    }

    #[test]
    fn cursor_ide_min_chat_completion_ok() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "hi"}]
            }))
            .expect("json"),
        );
        let ext = Extensions::new();
        let ep = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let (out, meta) = apply_ide_ingress_adjust(
            ClientProfile::CursorIde,
            &ep,
            body.clone(),
            &ext,
        )
        .expect("minimal chat completion should deserialize");
        assert_eq!(out, body);
        assert!(meta.applied);
    }

    #[test]
    fn cursor_ide_non_chat_path_no_json_parse() {
        let body = Bytes::from("not-json-at-all");
        let ext = Extensions::new();
        let ep = ApiEndpoint::OpenAI(OpenAI::embeddings());
        let (out, meta) = apply_ide_ingress_adjust(
            ClientProfile::CursorIde,
            &ep,
            body.clone(),
            &ext,
        )
        .expect("non-chat path must not parse body");
        assert_eq!(out, body);
        assert!(!meta.applied);
    }
}
