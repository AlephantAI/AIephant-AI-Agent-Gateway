use serde_json::Value;

use super::{input_token_estimate, parse::ChatCompletionsPayload};
use crate::types::provider::InferenceProvider;

const DEFAULT_TOKENS_PER_CHAR: f32 = 0.25;
const GPT_35_TOKENS_PER_CHAR: f32 = 0.20;
const TRUNCATE_CHAR_RATIO: f32 = 0.90;
const ELLIPSIS: &str = "...";

fn tokens_per_char(model: &str) -> f32 {
    let normalized = model.trim().to_ascii_lowercase();
    let suffix = normalized
        .split_once('/')
        .map_or(normalized.as_str(), |(_, suffix)| suffix);
    if suffix.starts_with("gpt-3.5-turbo") {
        GPT_35_TOKENS_PER_CHAR
    } else {
        DEFAULT_TOKENS_PER_CHAR
    }
}

#[must_use]
pub fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn value_with_model(raw: &Value, model: &str) -> Value {
    let mut cloned = raw.clone();
    if let Some(object) = cloned.as_object_mut() {
        object.insert("model".to_string(), Value::String(model.to_string()));
    }
    cloned
}

fn total_message_chars(payload: &ChatCompletionsPayload) -> usize {
    payload
        .messages
        .iter()
        .map(|message| normalize_text(&message.content).chars().count())
        .sum()
}

fn budget_chars(input_budget_tokens: u32, primary_model: &str) -> usize {
    ((input_budget_tokens as f32 / tokens_per_char(primary_model))
        * TRUNCATE_CHAR_RATIO)
        .floor() as usize
}

fn proportional_char_budgets(
    payload: &ChatCompletionsPayload,
    total_budget_chars: usize,
) -> Vec<usize> {
    let total_chars = total_message_chars(payload);
    if total_chars == 0 || total_budget_chars == 0 {
        return vec![0; payload.messages.len()];
    }

    let mut remaining_chars = total_budget_chars;
    let mut remaining_source_chars = total_chars;
    let mut budgets = Vec::with_capacity(payload.messages.len());

    for (index, message) in payload.messages.iter().enumerate() {
        let message_chars = normalize_text(&message.content).chars().count();
        let message_budget = if index + 1 == payload.messages.len() {
            remaining_chars
        } else if remaining_source_chars == 0 {
            0
        } else {
            message_chars.saturating_mul(remaining_chars)
                / remaining_source_chars
        };
        budgets.push(message_budget);
        remaining_chars = remaining_chars.saturating_sub(message_budget);
        remaining_source_chars =
            remaining_source_chars.saturating_sub(message_chars);
    }

    budgets
}

fn truncate_text_to_limit(text: &str, max_chars: usize) -> String {
    let normalized = normalize_text(text);
    let normalized_chars = normalized.chars().count();
    if max_chars == 0 {
        return String::new();
    }
    if normalized_chars <= max_chars {
        return normalized;
    }
    if max_chars <= ELLIPSIS.len() {
        return normalized.chars().take(max_chars).collect();
    }

    let raw_limit = max_chars - ELLIPSIS.len();
    let candidate: String = normalized.chars().take(raw_limit).collect();
    let cut = candidate
        .char_indices()
        .rev()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))
        .filter(|index| *index >= raw_limit.saturating_sub(32))
        .unwrap_or(candidate.len());
    let trimmed = candidate[..cut].trim_end();
    let mut output = if trimmed.is_empty() {
        candidate
    } else {
        trimmed.to_string()
    };
    output.push_str(ELLIPSIS);
    output
}

fn middle_out_text_to_limit(text: &str, max_chars: usize) -> String {
    let normalized = normalize_text(text);
    let normalized_chars = normalized.chars().count();
    if max_chars == 0 {
        return String::new();
    }
    if normalized_chars <= max_chars {
        return normalized;
    }
    if max_chars <= ELLIPSIS.len() + 2 {
        return normalized.chars().take(max_chars).collect();
    }

    let visible_chars = max_chars - ELLIPSIS.len();
    let head_chars = visible_chars.div_ceil(2);
    let tail_chars = visible_chars / 2;
    let head: String = normalized.chars().take(head_chars).collect();
    let tail: String = normalized
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}{ELLIPSIS}{tail}")
}

#[must_use]
pub fn resolve_primary_model(
    body_model: Option<&str>,
    header_model_override: Option<&str>,
) -> Option<String> {
    let model_source = body_model.or(header_model_override)?;
    extract_fallback_model_candidates(model_source)
        .into_iter()
        .next()
}

pub fn extract_fallback_model_candidates(model: &str) -> Vec<String> {
    model
        .split(',')
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(str::to_string)
        .collect()
}

#[must_use]
pub fn estimate_input_tokens(
    payload: &ChatCompletionsPayload,
    primary_model: &str,
    provider_hint: Option<&InferenceProvider>,
) -> Option<u32> {
    input_token_estimate::estimate_chat_completion_input_tokens(
        payload,
        primary_model,
        provider_hint,
    )
}

#[must_use]
pub fn compute_input_budget_tokens(
    model_context_limit: u32,
    requested_completion_tokens: Option<u32>,
) -> u32 {
    let reserved_completion_tokens = requested_completion_tokens
        .unwrap_or_else(|| (model_context_limit / 10).max(1));
    model_context_limit.saturating_sub(reserved_completion_tokens)
}

pub fn apply_truncate(
    payload: &ChatCompletionsPayload,
    primary_model: &str,
    input_budget_tokens: u32,
) -> Option<Value> {
    if payload.has_non_text_message_content || payload.messages.is_empty() {
        return None;
    }

    let total_budget_chars = budget_chars(input_budget_tokens, primary_model);
    if total_budget_chars == 0 {
        return None;
    }
    let total_chars = total_message_chars(payload);
    if total_chars <= total_budget_chars {
        return None;
    }

    let mut cloned = value_with_model(&payload.raw, primary_model);
    let messages_array =
        cloned.get_mut("messages").and_then(Value::as_array_mut)?;
    for (message, message_budget) in payload
        .messages
        .iter()
        .zip(proportional_char_budgets(payload, total_budget_chars))
    {
        messages_array[message.index]["content"] = Value::String(
            truncate_text_to_limit(&message.content, message_budget),
        );
    }
    Some(cloned)
}

pub fn apply_middle_out(
    payload: &ChatCompletionsPayload,
    primary_model: &str,
    input_budget_tokens: u32,
) -> Option<Value> {
    if payload.has_non_text_message_content || payload.messages.is_empty() {
        return None;
    }

    let total_budget_chars = budget_chars(input_budget_tokens, primary_model);
    if total_budget_chars == 0 {
        return None;
    }
    let total_chars = total_message_chars(payload);
    if total_chars <= total_budget_chars {
        return None;
    }

    let mut cloned = value_with_model(&payload.raw, primary_model);
    let messages_array =
        cloned.get_mut("messages").and_then(Value::as_array_mut)?;
    for (message, message_budget) in payload
        .messages
        .iter()
        .zip(proportional_char_budgets(payload, total_budget_chars))
    {
        messages_array[message.index]["content"] = Value::String(
            middle_out_text_to_limit(&message.content, message_budget),
        );
    }
    Some(cloned)
}

#[must_use]
pub fn apply_fallback(
    payload: &ChatCompletionsPayload,
    model_source: &str,
    estimated_input_tokens: Option<u32>,
    input_budget_tokens: Option<u32>,
) -> Option<Value> {
    let candidates = extract_fallback_model_candidates(model_source);
    let fallback_model = candidates.get(1)?;
    if input_budget_tokens.is_none()
        || estimated_input_tokens.is_none()
        || estimated_input_tokens
            .zip(input_budget_tokens)
            .is_some_and(|(estimated, budget)| estimated >= budget)
    {
        Some(value_with_model(&payload.raw, fallback_model))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        apply_fallback, apply_middle_out, apply_truncate,
        compute_input_budget_tokens, estimate_input_tokens,
        extract_fallback_model_candidates, resolve_primary_model,
    };
    use crate::middleware::large_context::parse::parse_chat_completions_payload;

    fn payload_with_text(text: &str) -> super::ChatCompletionsPayload {
        parse_chat_completions_payload(
            &serde_json::to_vec(&json!({
                "model": "openai/gpt-4o-mini",
                "messages": [
                    {
                        "role": "user",
                        "content": text
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap()
        .expect("payload should parse")
    }

    #[test]
    fn resolves_primary_model_from_first_candidate() {
        assert_eq!(
            resolve_primary_model(
                Some("openai/gpt-4o-mini,openai/gpt-4o"),
                Some("openai/gpt-4o")
            )
            .as_deref(),
            Some("openai/gpt-4o-mini")
        );
    }

    #[test]
    fn splits_fallback_candidates() {
        assert_eq!(
            extract_fallback_model_candidates(
                " openai/gpt-4o-mini , openai/gpt-4o "
            ),
            vec!["openai/gpt-4o-mini", "openai/gpt-4o"]
        );
    }

    #[test]
    fn computes_input_budget_with_default_reserve() {
        assert_eq!(compute_input_budget_tokens(128_000, None), 115_200);
    }

    #[test]
    fn fallback_can_apply_without_budget() {
        let payload = payload_with_text("hello");
        let transformed = apply_fallback(
            &payload,
            "openai/gpt-4o-mini,openai/gpt-4o",
            None,
            None,
        )
        .expect("fallback should apply");
        assert_eq!(transformed["model"], "openai/gpt-4o");
    }

    #[test]
    fn fallback_without_second_candidate_returns_none() {
        let payload = payload_with_text("hello");
        let transformed = apply_fallback(
            &payload,
            "openai/gpt-4o-mini",
            Some(120_000),
            Some(115_200),
        );

        assert!(transformed.is_none());
    }

    #[test]
    fn truncate_shortens_text() {
        let payload = payload_with_text(&"hello world ".repeat(50));
        let transformed = apply_truncate(&payload, "openai/gpt-4o-mini", 20)
            .expect("truncate should apply");
        let content = transformed["messages"][0]["content"]
            .as_str()
            .expect("content should be text");
        assert!(content.len() < payload.messages[0].content.len());
        assert!(content.ends_with("..."));
    }

    #[test]
    fn middle_out_preserves_edges() {
        let payload = payload_with_text(
            "HEAD:abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz:TAIL",
        );
        let transformed = apply_middle_out(&payload, "openai/gpt-4o-mini", 10)
            .expect("middle-out should apply");
        let content = transformed["messages"][0]["content"]
            .as_str()
            .expect("content should be text");
        assert!(content.starts_with("HEAD:"));
        assert!(content.ends_with(":TAIL"));
    }

    #[test]
    fn middle_out_handles_long_unbroken_string() {
        let payload = payload_with_text(&"x".repeat(20_000));
        let transformed = apply_middle_out(&payload, "openai/gpt-4o-mini", 100)
            .expect("middle-out should apply");
        let content = transformed["messages"][0]["content"]
            .as_str()
            .expect("content should be text");
        assert!(content.len() < payload.messages[0].content.len());
        assert!(content.contains("..."));
    }

    #[test]
    fn estimate_returns_none_for_non_text_payload() {
        let payload = parse_chat_completions_payload(
            &serde_json::to_vec(&json!({
                "model": "openai/gpt-4o-mini",
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "text",
                                "text": "hello"
                            }
                        ]
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap()
        .expect("payload should parse");

        assert_eq!(
            estimate_input_tokens(&payload, "openai/gpt-4o-mini", None),
            None
        );
    }
}
