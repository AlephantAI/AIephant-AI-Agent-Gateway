use bytes::Bytes;
use http::request::Parts;
use serde_json::Value;

use crate::{
    error::invalid_req::InvalidRequestError,
    middleware::large_context::{
        headers::parse_large_context_headers,
        heuristics::{
            estimate_input_tokens, normalize_text, resolve_primary_model,
        },
        parse::{
            ChatCompletionsPayload, parse_chat_completions_payload,
            serialize_payload,
        },
    },
    types::{
        extensions::PromptCompressionTokenPair, provider::InferenceProvider,
    },
};

fn patch_messages_whitespace(
    raw: &mut Value,
    payload: &ChatCompletionsPayload,
) {
    let Some(object) = raw.as_object_mut() else {
        return;
    };
    let Some(array) = object
        .get_mut("messages")
        .and_then(|message| message.as_array_mut())
    else {
        return;
    };
    for message in &payload.messages {
        let Some(obj) = array
            .get_mut(message.index)
            .and_then(|message| message.as_object_mut())
        else {
            continue;
        };
        if let Some(content) = obj.get_mut("content")
            && let Value::String(text) = content
        {
            *content = Value::String(normalize_text(text));
        }
    }
}

/// Apply the same whitespace normalization as `heuristics::normalize_text` to
/// `chat/completions` body; when applicable store pre/post compression token
/// estimates in `extensions` and return the updated body.
pub fn apply_chat_completions(
    parts: &mut Parts,
    body: Bytes,
    provider: &InferenceProvider,
) -> Result<Bytes, InvalidRequestError> {
    let Some(payload) = (match parse_chat_completions_payload(&body) {
        Ok(v) => v,
        Err(_) => return Ok(body),
    }) else {
        return Ok(body);
    };

    let headers = parse_large_context_headers(&parts.headers)?;
    let model_in_body = payload.model.as_deref();
    let Some(primary) =
        resolve_primary_model(model_in_body, headers.model_override.as_deref())
    else {
        return Ok(body);
    };
    if payload.has_non_text_message_content || payload.messages.is_empty() {
        return Ok(body);
    }
    let Some(before) =
        estimate_input_tokens(&payload, &primary, Some(provider))
    else {
        return Ok(body);
    };
    let mut raw = payload.raw.clone();
    patch_messages_whitespace(&mut raw, &payload);
    let after_bytes = serialize_payload(&raw)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let after_pl = match parse_chat_completions_payload(&after_bytes) {
        Ok(Some(payload)) => payload,
        Ok(None) | Err(_) => return Ok(body),
    };
    let Some(after) =
        estimate_input_tokens(&after_pl, &primary, Some(provider))
    else {
        return Ok(body);
    };

    parts.extensions.insert(PromptCompressionTokenPair {
        origin_prompt_token: before,
        compression_prompt_token: after,
    });
    Ok(after_bytes)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::Request;

    use super::apply_chat_completions;
    use crate::types::{
        extensions::PromptCompressionTokenPair, provider::InferenceProvider,
    };

    fn empty_parts() -> http::request::Parts {
        Request::new(()).into_parts().0
    }

    #[test]
    fn compresses_whitespace_and_inserts_token_pair() {
        const BODY: &[u8] = br#"{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"  a   b  "}]}"#;
        let mut parts = empty_parts();
        let out = apply_chat_completions(
            &mut parts,
            Bytes::from_static(BODY),
            &InferenceProvider::OpenAI,
        )
        .expect("apply");

        let pair = parts
            .extensions
            .get::<PromptCompressionTokenPair>()
            .expect("extension set");
        assert!(pair.origin_prompt_token >= pair.compression_prompt_token);
        let v: serde_json::Value = serde_json::from_slice(&out).expect("out");
        assert_eq!(v["messages"][0]["content"].as_str(), Some("a b"));
    }

    #[test]
    fn invalid_json_leaves_body_and_extension_unchanged() {
        let mut parts = empty_parts();
        let body = Bytes::from_static(b"not json {");
        let out = apply_chat_completions(
            &mut parts,
            body.clone(),
            &InferenceProvider::OpenAI,
        )
        .expect("ok body passthrough");
        assert_eq!(out, body);
        assert!(
            parts
                .extensions
                .get::<PromptCompressionTokenPair>()
                .is_none()
        );
    }
}
