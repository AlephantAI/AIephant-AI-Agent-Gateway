//! Normalizes invalid OpenAI Chat Completions `role` values in JSON before
//! strict deserialization into `async_openai` types.
//!
//! Also patches missing top-level fields required by `async_openai` chat
//! completion types (`id`, `choices`, `created`, `model`, `object`) when
//! upstream sends incomplete OpenAI-shaped JSON.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use uuid::Uuid;

use crate::endpoints::{ApiEndpoint, openai::OpenAI};

const ALLOWED_ROLES: [&str; 5] =
    ["system", "user", "assistant", "tool", "function"];

const ORIGINAL_ROLE_MAX_CHARS: usize = 64;

#[derive(Debug, Default, Clone, Copy)]
pub struct NormalizeRoleStats {
    pub empty_normalized: u32,
    pub unknown_normalized: u32,
}

/// `stream_chunk`: true → normalize `choices[].delta.role`; false →
/// `choices[].message.role`.
///
/// Only walks `choices` when `root` is an Object and `choices` is an Array;
/// otherwise no-op.
pub fn normalize_chat_completion_roles_in_place(
    root: &mut Value,
    stream_chunk: bool,
) -> NormalizeRoleStats {
    let mut stats = NormalizeRoleStats::default();
    let Some(root_obj) = root.as_object_mut() else {
        return stats;
    };
    let Some(choices) =
        root_obj.get_mut("choices").and_then(Value::as_array_mut)
    else {
        return stats;
    };

    let container_key = if stream_chunk { "delta" } else { "message" };

    for choice in choices.iter_mut() {
        let Some(choice_obj) = choice.as_object_mut() else {
            continue;
        };
        let Some(container) = choice_obj
            .get_mut(container_key)
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let Some(role_slot) = container.get_mut("role") else {
            continue;
        };
        let Value::String(s) = role_slot else {
            continue;
        };

        let trimmed = s.trim();
        if trimmed.is_empty() {
            *role_slot = Value::String("assistant".into());
            stats.empty_normalized += 1;
            tracing::warn!(
                reason = "empty",
                empty_normalized = stats.empty_normalized,
                "chat completion role normalized"
            );
            continue;
        }

        if ALLOWED_ROLES.contains(&trimmed) {
            continue;
        }

        let original_display =
            truncate_original(s.as_str(), ORIGINAL_ROLE_MAX_CHARS);
        tracing::warn!(
            reason = "unknown",
            original_role = %original_display,
            unknown_normalized = stats.unknown_normalized.saturating_add(1),
            "chat completion role normalized"
        );
        *role_slot = Value::String("assistant".into());
        stats.unknown_normalized += 1;
    }

    stats
}

fn unix_now_u32() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// Fills required top-level keys for `CreateChatCompletionResponse` /
/// `CreateChatCompletionStreamResponse` when upstream omits them. Each patch
/// is logged at warn for auditing.
///
/// `stream_chunk`: use `chat.completion.chunk` for `object` when inserting.
pub fn ensure_openai_chat_completion_required_fields_in_place(
    root: &mut Value,
    stream_chunk: bool,
) {
    let Some(root_obj) = root.as_object_mut() else {
        return;
    };

    let expected_object = if stream_chunk {
        "chat.completion.chunk"
    } else {
        "chat.completion"
    };

    let id_needs_patch = match root_obj.get("id") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };
    if id_needs_patch {
        let id = format!("chatcmpl-{}", Uuid::new_v4());
        root_obj.insert("id".to_string(), Value::String(id));
        tracing::warn!(
            "chat completion upstream response missing id; inserted synthetic \
             id"
        );
    }

    let choices_needs_patch =
        matches!(root_obj.get("choices"), None | Some(Value::Null));
    if choices_needs_patch {
        root_obj.insert("choices".to_string(), Value::Array(vec![]));
        tracing::warn!(
            "chat completion upstream response missing choices; inserted \
             empty choices array"
        );
    }

    let created_needs_patch =
        matches!(root_obj.get("created"), None | Some(Value::Null));
    if created_needs_patch {
        let ts = unix_now_u32();
        root_obj.insert("created".to_string(), Value::Number(ts.into()));
        tracing::warn!(
            "chat completion upstream response missing created; inserted \
             current unix time"
        );
    }

    let model_needs_patch = match root_obj.get("model") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };
    if model_needs_patch {
        root_obj.insert("model".to_string(), Value::String(String::new()));
        tracing::warn!(
            "chat completion upstream response missing model; inserted empty \
             string"
        );
    }

    let object_needs_patch = match root_obj.get("object") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };
    if object_needs_patch {
        root_obj.insert(
            "object".to_string(),
            Value::String(expected_object.to_string()),
        );
        tracing::warn!(
            object = expected_object,
            "chat completion upstream response missing object; inserted \
             default object tag"
        );
    }
}

fn truncate_original(s: &str, max_chars: usize) -> String {
    let mut iter = s.chars();
    let taken: String = iter.by_ref().take(max_chars).collect();
    if iter.next().is_some() {
        format!("{taken}...")
    } else {
        taken
    }
}

#[must_use]
pub(crate) fn lenient_openai_chat_roles_for_target_endpoint(
    target_endpoint: &ApiEndpoint,
) -> bool {
    match target_endpoint {
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)) => true,
        ApiEndpoint::OpenAICompatible {
            openai_endpoint: OpenAI::ChatCompletions(_),
            ..
        } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        endpoints::{ApiEndpoint, anthropic::Anthropic, openai::OpenAI},
        types::provider::InferenceProvider,
    };

    #[test]
    fn lenient_true_for_openai_official_chat_completions() {
        assert!(lenient_openai_chat_roles_for_target_endpoint(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
        ));
    }

    #[test]
    fn lenient_true_for_named_openai_compatible_chat_completions() {
        assert!(lenient_openai_chat_roles_for_target_endpoint(
            &ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named("qwen".into()),
                openai_endpoint: OpenAI::chat_completions(),
            },
        ));
    }

    #[test]
    fn lenient_true_for_custom_openai_compatible_chat_completions() {
        assert!(lenient_openai_chat_roles_for_target_endpoint(
            &ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Custom,
                openai_endpoint: OpenAI::chat_completions(),
            },
        ));
    }

    #[test]
    fn lenient_false_for_openai_embeddings() {
        assert!(!lenient_openai_chat_roles_for_target_endpoint(
            &ApiEndpoint::OpenAI(OpenAI::embeddings()),
        ));
    }

    #[test]
    fn lenient_false_for_anthropic_messages() {
        assert!(!lenient_openai_chat_roles_for_target_endpoint(
            &ApiEndpoint::Anthropic(Anthropic::messages()),
        ));
    }

    #[test]
    fn lenient_false_for_openai_compatible_embeddings() {
        assert!(!lenient_openai_chat_roles_for_target_endpoint(
            &ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Custom,
                openai_endpoint: OpenAI::embeddings(),
            },
        ));
    }

    #[test]
    fn normalizes_empty_message_role_non_stream() {
        let mut v = serde_json::json!({
            "choices": [{ "message": { "role": "", "content": "hi" } }]
        });
        let s = normalize_chat_completion_roles_in_place(&mut v, false);
        assert_eq!(s.empty_normalized, 1);
        assert_eq!(v["choices"][0]["message"]["role"], "assistant");
    }

    #[test]
    fn normalizes_unknown_message_role_non_stream() {
        let mut v = serde_json::json!({
            "choices": [{ "message": { "role": "model", "content": "x" } }]
        });
        let s = normalize_chat_completion_roles_in_place(&mut v, false);
        assert_eq!(s.unknown_normalized, 1);
        assert_eq!(v["choices"][0]["message"]["role"], "assistant");
    }

    #[test]
    fn normalizes_delta_role_stream_chunk() {
        let mut v = serde_json::json!({
            "choices": [{ "delta": { "role": "" } }]
        });
        let s = normalize_chat_completion_roles_in_place(&mut v, true);
        assert_eq!(s.empty_normalized, 1);
        assert_eq!(v["choices"][0]["delta"]["role"], "assistant");
    }

    #[test]
    fn leaves_valid_role_unchanged() {
        let mut v = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "" } }]
        });
        let s = normalize_chat_completion_roles_in_place(&mut v, false);
        assert_eq!(s.empty_normalized + s.unknown_normalized, 0);
    }

    #[test]
    fn fills_missing_top_level_id() {
        let mut v = serde_json::json!({ "choices": [] });
        ensure_openai_chat_completion_required_fields_in_place(&mut v, false);
        let id = v["id"].as_str().expect("id");
        assert!(
            id.starts_with("chatcmpl-"),
            "expected chatcmpl- prefix, got {id:?}"
        );
    }

    #[test]
    fn fills_null_id() {
        let mut v = serde_json::json!({ "id": null, "choices": [] });
        ensure_openai_chat_completion_required_fields_in_place(&mut v, false);
        assert!(v["id"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn leaves_non_empty_id_unchanged() {
        let mut v =
            serde_json::json!({ "id": "chatcmpl-upstream", "choices": [] });
        ensure_openai_chat_completion_required_fields_in_place(&mut v, false);
        assert_eq!(v["id"], "chatcmpl-upstream");
    }

    #[test]
    fn fills_missing_choices_created_model_object_non_stream() {
        let mut v = serde_json::json!({ "id": "x" });
        ensure_openai_chat_completion_required_fields_in_place(&mut v, false);
        assert_eq!(v["choices"], serde_json::json!([]));
        assert!(v["created"].as_u64().is_some());
        assert_eq!(v["model"], "");
        assert_eq!(v["object"], "chat.completion");
    }

    #[test]
    fn stream_chunk_inserts_chunk_object_tag() {
        let mut v = serde_json::json!({});
        ensure_openai_chat_completion_required_fields_in_place(&mut v, true);
        assert_eq!(v["object"], "chat.completion.chunk");
    }
}
