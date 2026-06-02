//! Routing and VK checks on unified `chat/completions` using only the top-level
//! `model`.
//!
//! IDE ingress bodies are normalized by `ClientProfile` in the mapper, not
//! earlier; router / VK must not hard-depend on deserializing
//! `CreateChatCompletionRequest`, or non-standard `messages` and similar fields
//! will break the path.

use std::str::FromStr;

use bytes::Bytes;
use http::uri::PathAndQuery;

use crate::{
    endpoints::{ApiEndpoint, openai::OpenAI},
    error::{
        api::ApiError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    types::extensions::UnifiedChatCompletionsResponsesBridge,
};

/// Parses JSON and returns a non-empty trimmed top-level `model` string for
/// unified `chat/completions` routing and VK checks, **without** validating
/// full OpenAI Chat Completions shape (that happens in mapper after IDE
/// ingress).
pub(crate) fn unified_chat_completions_routing_model(
    body: &[u8],
) -> Result<String, InvalidRequestError> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let s = v
        .get("model")
        .and_then(serde_json::Value::as_str)
        .ok_or(InvalidRequestError::MissingModelId)?;
    let t = s.trim();
    if t.is_empty() {
        return Err(InvalidRequestError::MissingModelId);
    }
    Ok(t.to_string())
}

/// For `POST .../chat/completions`, if the body looks like OpenAI Responses
/// (`input`, no top-level `messages`), route as `/responses`: update
/// `ApiEndpoint` and `PathAndQuery` in extensions so Chat Completions serde
/// does not fail (e.g. Cursor + `gpt-5.4` still POSTs to chat but sends
/// Responses JSON).
pub(crate) fn apply_chat_completions_body_redirect_if_needed(
    path: &str,
    body: &Bytes,
    parts: &mut http::request::Parts,
) -> Result<String, ApiError> {
    if path != "chat/completions" {
        return Ok(path.to_string());
    }
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Ok(path.to_string()),
    };
    let Some(obj) = value.as_object() else {
        return Ok(path.to_string());
    };
    if obj.contains_key("messages") {
        return Ok(path.to_string());
    }
    if !obj.contains_key("input") {
        return Ok(path.to_string());
    }
    parts
        .extensions
        .insert(UnifiedChatCompletionsResponsesBridge);
    parts
        .extensions
        .insert(ApiEndpoint::OpenAI(OpenAI::responses()));
    let pq = parts
        .extensions
        .get::<PathAndQuery>()
        .ok_or(InternalError::ExtensionNotFound("PathAndQuery"))?;
    let new_pq_str = match pq.query() {
        Some(q) => format!("responses?{q}"),
        None => "responses".to_string(),
    };
    let new_pq = PathAndQuery::from_str(&new_pq_str)
        .map_err(InternalError::InvalidUri)?;
    parts.extensions.insert(new_pq);
    tracing::debug!(
        "unified_api: chat/completions body has Responses API shape (input, \
         no messages); routing as responses"
    );
    Ok("responses".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_routing_model_accepts_non_openai_chat_messages_shape() {
        let body = br#"{"model":"anthropic/claude-opus-4.6","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"x"}]}]}"#;
        assert_eq!(
            unified_chat_completions_routing_model(body).unwrap(),
            "anthropic/claude-opus-4.6"
        );
    }

    #[test]
    fn chat_completions_redirects_to_responses_when_input_without_messages() {
        let body = Bytes::from(
            r#"{"model":"openai/gpt-5.4","input":[{"role":"user","content":"hi"}]}"#,
        );
        let mut parts =
            http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions").unwrap());
        parts
            .extensions
            .insert(ApiEndpoint::OpenAI(OpenAI::chat_completions()));

        let out = apply_chat_completions_body_redirect_if_needed(
            "chat/completions",
            &body,
            &mut parts,
        )
        .unwrap();
        assert_eq!(out, "responses");
        assert_eq!(
            parts.extensions.get::<ApiEndpoint>(),
            Some(&ApiEndpoint::OpenAI(OpenAI::responses()))
        );
        let pq = parts.extensions.get::<PathAndQuery>().unwrap();
        assert_eq!(pq.path(), "responses");
        assert!(pq.query().is_none());
    }

    #[test]
    fn chat_completions_redirect_preserves_query() {
        let body = Bytes::from(r#"{"model":"m","input":[]}"#);
        let mut parts =
            http::Request::builder().body(()).unwrap().into_parts().0;
        parts.extensions.insert(
            PathAndQuery::from_str("chat/completions?trace=1").unwrap(),
        );

        apply_chat_completions_body_redirect_if_needed(
            "chat/completions",
            &body,
            &mut parts,
        )
        .unwrap();

        let pq = parts.extensions.get::<PathAndQuery>().unwrap();
        assert_eq!(pq.path(), "responses");
        assert_eq!(pq.query(), Some("trace=1"));
    }

    #[test]
    fn chat_completions_no_redirect_when_messages_present() {
        let body = Bytes::from(
            r#"{"model":"openai/x","messages":[{"role":"user","content":"hi"}]}"#,
        );
        let mut parts =
            http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions").unwrap());

        let out = apply_chat_completions_body_redirect_if_needed(
            "chat/completions",
            &body,
            &mut parts,
        )
        .unwrap();
        assert_eq!(out, "chat/completions");
    }

    #[test]
    fn chat_completions_no_redirect_without_input() {
        let body = Bytes::from(r#"{"model":"openai/x"}"#);
        let mut parts =
            http::Request::builder().body(()).unwrap().into_parts().0;
        parts
            .extensions
            .insert(PathAndQuery::from_str("chat/completions").unwrap());

        let out = apply_chat_completions_body_redirect_if_needed(
            "chat/completions",
            &body,
            &mut parts,
        )
        .unwrap();
        assert_eq!(out, "chat/completions");
    }
}
