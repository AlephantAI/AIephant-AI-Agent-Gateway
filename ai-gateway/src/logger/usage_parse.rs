//! Extract OpenAI-compatible `usage` counters from a provider response body
//! (JSON) or from SSE `data:` frames (streaming logs).
//!
//! Primary counts — **rule B** (Chat names win; otherwise Responses names):
//! - `usage.prompt_tokens` if the key is present and non-null, else
//!   `usage.input_tokens` → [`UsageTokenCounts::prompt_tokens`]
//! - `usage.completion_tokens` if present and non-null, else
//!   `usage.output_tokens` → [`UsageTokenCounts::completion_tokens`]
//!
//! Prompt-side details (`prompt_tokens_details` preferred; else
//! `input_tokens_details` for the same field when the Chat object omits that
//! key):
//! - `*.cached_tokens` → [`UsageTokenCounts::prompt_cache_read_tokens`]
//! - `*.cache_write_tokens` / `cache_write_input_tokens` →
//!   [`UsageTokenCounts::prompt_cache_write_tokens`]
//! - `*.audio_tokens` → [`UsageTokenCounts::prompt_audio_tokens`]
//!
//! Completion-side details (`completion_tokens_details` preferred; else
//! `output_tokens_details` with the same per-key fallback):
//! - `*.audio_tokens` → [`UsageTokenCounts::completion_audio_tokens`]
//! - `*.reasoning_tokens` → [`UsageTokenCounts::reasoning_tokens`]
//!
//! `prompt_cache_read_tokens` comes from `cached_tokens` (`OpenAI`) or
//! `cache_read_input_tokens` (Bedrock-style). `prompt_cache_write_tokens` from
//! `cache_write_input_tokens` when present, else `cache_write_tokens` when
//! present; otherwise **0**.
//!
//! `usage.total_tokens` is not used to derive prompt or completion counts.

use serde_json::Value;

pub use crate::types::usage_tokens::UsageTokenCounts;

/// Parse `usage` from a single JSON document (UTF-8). On any error, returns
/// [`UsageTokenCounts::default`]. Equivalent to
/// [`usage_counts_from_response_body_for_log`] with `is_stream: false` and no
/// SSE fallback when the root object has no `usage`.
#[must_use]
pub fn usage_counts_from_response_body(body: &[u8]) -> UsageTokenCounts {
    usage_counts_from_response_body_for_log(false, body)
}

/// Prefer a single JSON root when `is_stream` is false and `usage` is present
/// and non-zero; otherwise scan `data:` SSE lines and take the **last** frame
/// that carries a `usage` object.
#[must_use]
pub fn usage_counts_from_response_body_for_log(is_stream: bool, body: &[u8]) -> UsageTokenCounts {
    let from_single = std::str::from_utf8(body)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .map(|v| extract_usage_from_root(&v))
        .unwrap_or_default();

    if !is_stream && from_single != UsageTokenCounts::default() {
        return from_single;
    }

    let from_sse = usage_counts_from_sse_data_frames(body);
    if from_sse != UsageTokenCounts::default() {
        return from_sse;
    }

    from_single
}

/// Scan UTF-8 for lines starting with `data:`; parse each payload as JSON and
/// keep the last [`UsageTokenCounts`] derived from a `usage` field.
#[must_use]
pub fn usage_counts_from_sse_data_frames(body: &[u8]) -> UsageTokenCounts {
    let Ok(s) = std::str::from_utf8(body) else {
        return UsageTokenCounts::default();
    };
    let mut last = UsageTokenCounts::default();
    for line in s.lines() {
        let line = line.trim();
        let payload = if let Some(rest) = line.strip_prefix("data:") {
            rest.trim()
        } else {
            continue;
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        if let Some(u) = v.get("usage").filter(|x| !x.is_null()) {
            last = extract_usage_from_usage_object(u);
        }
    }
    last
}

#[must_use]
pub fn extract_usage_from_root(root: &Value) -> UsageTokenCounts {
    let Some(usage) = root.get("usage") else {
        return UsageTokenCounts::default();
    };
    if usage.is_null() {
        return UsageTokenCounts::default();
    }
    extract_usage_from_usage_object(usage)
}

fn extract_usage_from_usage_object(usage: &Value) -> UsageTokenCounts {
    let mut out = UsageTokenCounts {
        prompt_tokens: usage_count_chat_or_responses(usage, "prompt_tokens", "input_tokens"),
        completion_tokens: usage_count_chat_or_responses(
            usage,
            "completion_tokens",
            "output_tokens",
        ),
        ..Default::default()
    };

    apply_merged_prompt_token_details(usage, &mut out);
    apply_merged_completion_token_details(usage, &mut out);

    out
}

/// Rule B: if `chat_key` is present and non-null, use its numeric value;
/// otherwise use `responses_key`.
fn usage_count_chat_or_responses(usage: &Value, chat_key: &str, responses_key: &str) -> i64 {
    match usage.get(chat_key) {
        Some(v) if !v.is_null() => value_as_i64(v),
        _ => json_i64(usage, responses_key),
    }
}

fn fill_prompt_details_from(details: &Value, out: &mut UsageTokenCounts) {
    out.prompt_cache_read_tokens = json_i64(details, "cached_tokens");
    out.prompt_audio_tokens = json_i64(details, "audio_tokens");
    if out.prompt_cache_read_tokens == 0
        && let Some(cache) = details.get("cache_read_input_tokens")
    {
        out.prompt_cache_read_tokens = value_as_i64(cache);
    }
    if out.prompt_cache_write_tokens == 0
        && let Some(cache) = details.get("cache_write_input_tokens")
    {
        out.prompt_cache_write_tokens = value_as_i64(cache);
    }
    if out.prompt_cache_write_tokens == 0 {
        out.prompt_cache_write_tokens = json_i64(details, "cache_write_tokens");
    }
}

fn fill_completion_details_from(details: &Value, out: &mut UsageTokenCounts) {
    out.reasoning_tokens = json_i64(details, "reasoning_tokens");
    out.completion_audio_tokens = json_i64(details, "audio_tokens");
}

fn apply_merged_prompt_token_details(usage: &Value, out: &mut UsageTokenCounts) {
    let chat = usage
        .get("prompt_tokens_details")
        .filter(|v| !v.is_null() && v.is_object());
    let resp = usage
        .get("input_tokens_details")
        .filter(|v| !v.is_null() && v.is_object());

    match (chat, resp) {
        (Some(c), Some(r)) => {
            fill_prompt_details_from(c, out);
            merge_prompt_details_fallback(c, r, out);
        }
        (Some(c), None) => fill_prompt_details_from(c, out),
        (None, Some(r)) => fill_prompt_details_from(r, out),
        (None, None) => {}
    }
}

/// When both Chat and Responses detail objects exist, keep Chat-filled values
/// unless Chat omits a key that Responses supplies (per design §3).
fn merge_prompt_details_fallback(
    chat_details: &Value,
    resp_details: &Value,
    out: &mut UsageTokenCounts,
) {
    let Some(chat_obj) = chat_details.as_object() else {
        return;
    };

    let cache_read_unset_on_chat = !chat_obj.contains_key("cached_tokens")
        && !chat_obj.contains_key("cache_read_input_tokens");
    if cache_read_unset_on_chat && out.prompt_cache_read_tokens == 0 {
        let mut tmp = UsageTokenCounts::default();
        fill_prompt_details_from(resp_details, &mut tmp);
        out.prompt_cache_read_tokens = tmp.prompt_cache_read_tokens;
    }

    if !chat_obj.contains_key("audio_tokens") && out.prompt_audio_tokens == 0 {
        out.prompt_audio_tokens = json_i64(resp_details, "audio_tokens");
    }

    let cache_write_unset_on_chat = !chat_obj.contains_key("cache_write_tokens")
        && !chat_obj.contains_key("cache_write_input_tokens");
    if cache_write_unset_on_chat && out.prompt_cache_write_tokens == 0 {
        out.prompt_cache_write_tokens = json_i64(resp_details, "cache_write_tokens");
        if out.prompt_cache_write_tokens == 0
            && let Some(cache) = resp_details.get("cache_write_input_tokens")
        {
            out.prompt_cache_write_tokens = value_as_i64(cache);
        }
    }
}

fn apply_merged_completion_token_details(usage: &Value, out: &mut UsageTokenCounts) {
    let chat = usage
        .get("completion_tokens_details")
        .filter(|v| !v.is_null() && v.is_object());
    let resp = usage
        .get("output_tokens_details")
        .filter(|v| !v.is_null() && v.is_object());

    match (chat, resp) {
        (Some(c), Some(r)) => {
            fill_completion_details_from(c, out);
            merge_completion_details_fallback(c, r, out);
        }
        (Some(c), None) => fill_completion_details_from(c, out),
        (None, Some(r)) => fill_completion_details_from(r, out),
        (None, None) => {}
    }
}

fn merge_completion_details_fallback(
    chat_details: &Value,
    resp_details: &Value,
    out: &mut UsageTokenCounts,
) {
    let Some(chat_obj) = chat_details.as_object() else {
        return;
    };

    if !chat_obj.contains_key("reasoning_tokens") && out.reasoning_tokens == 0 {
        out.reasoning_tokens = json_i64(resp_details, "reasoning_tokens");
    }

    if !chat_obj.contains_key("audio_tokens") && out.completion_audio_tokens == 0 {
        out.completion_audio_tokens = json_i64(resp_details, "audio_tokens");
    }
}

fn json_i64(obj: &Value, key: &str) -> i64 {
    obj.get(key).map_or(0, value_as_i64)
}

fn value_as_i64(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_parse_invalid_utf8_yields_zeros() {
        let counts = usage_counts_from_response_body(&[0xFF, 0xFE]);
        assert_eq!(counts, UsageTokenCounts::default());
    }

    #[test]
    fn usage_parse_non_json_yields_zeros() {
        let counts = usage_counts_from_response_body(b"not json");
        assert_eq!(counts, UsageTokenCounts::default());
    }

    #[test]
    fn usage_parse_openai_completion_usage() {
        let json = r#"{"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_tokens, 10);
        assert_eq!(c.completion_tokens, 20);
        assert_eq!(c.prompt_cache_read_tokens, 0);
    }

    #[test]
    fn usage_parse_cached_and_reasoning_details() {
        let json = r#"{"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":40,"audio_tokens":2},"completion_tokens_details":{"reasoning_tokens":10,"audio_tokens":3}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_tokens, 100);
        assert_eq!(c.completion_tokens, 50);
        assert_eq!(c.prompt_cache_read_tokens, 40);
        assert_eq!(c.prompt_audio_tokens, 2);
        assert_eq!(c.reasoning_tokens, 10);
        assert_eq!(c.completion_audio_tokens, 3);
    }

    #[test]
    fn usage_parse_bedrock_style_cache_split() {
        let json = r#"{"usage":{"prompt_tokens":1,"completion_tokens":2,"prompt_tokens_details":{"cache_read_input_tokens":50,"cache_write_input_tokens":60}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_cache_read_tokens, 50);
        assert_eq!(c.prompt_cache_write_tokens, 60);
    }

    #[test]
    fn usage_from_sse_log_body_picks_last_usage() {
        let frames = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"\
             completion_tokens\":7,\"total_tokens\":10,\"\
             prompt_tokens_details\":{\"cached_tokens\":2,\"\
             cache_write_tokens\":5,\"cache_write_details\":{\"\
             write_5m_tokens\":5,\"write_1h_tokens\":0}}}}\n\n",
        );
        let c = usage_counts_from_response_body_for_log(false, frames.as_bytes());
        assert_eq!(c.prompt_tokens, 3);
        assert_eq!(c.completion_tokens, 7);
        assert_eq!(c.prompt_cache_read_tokens, 2);
        assert_eq!(c.prompt_cache_write_tokens, 5);
    }

    #[test]
    fn usage_parse_responses_style_input_output_tokens() {
        let json = r#"{"usage":{"input_tokens":99407,"input_tokens_details":{"cached_tokens":98944},"output_tokens":688,"output_tokens_details":{"reasoning_tokens":139},"total_tokens":100095}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_tokens, 99_407);
        assert_eq!(c.completion_tokens, 688);
        assert_eq!(c.prompt_cache_read_tokens, 98_944);
        assert_eq!(c.reasoning_tokens, 139);
    }

    #[test]
    fn usage_parse_rule_b_chat_counts_win_when_both_present() {
        let json = r#"{"usage":{"prompt_tokens":1,"input_tokens":99999,"completion_tokens":2,"output_tokens":88888}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_tokens, 1);
        assert_eq!(c.completion_tokens, 2);
    }

    #[test]
    fn usage_parse_chat_details_win_when_both_detail_objects_exist() {
        let json = r#"{"usage":{"prompt_tokens":10,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":5},"input_tokens_details":{"cached_tokens":999}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_cache_read_tokens, 5);
    }

    #[test]
    fn usage_parse_merge_input_details_when_chat_details_empty_object() {
        let json = r#"{"usage":{"prompt_tokens":10,"prompt_tokens_details":{},"input_tokens_details":{"cached_tokens":42,"audio_tokens":3}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_cache_read_tokens, 42);
        assert_eq!(c.prompt_audio_tokens, 3);
    }

    #[test]
    fn usage_parse_merge_output_details_when_completion_details_omit_keys() {
        let json = r#"{"usage":{"completion_tokens":5,"completion_tokens_details":{},"output_tokens_details":{"reasoning_tokens":7,"audio_tokens":9}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.reasoning_tokens, 7);
        assert_eq!(c.completion_audio_tokens, 9);
    }

    #[test]
    fn usage_parse_responses_style_in_sse_last_frame() {
        let frames = concat!(
            "data: {\"type\":\"response.output_text.delta\"}\n\n",
            "data: {\"usage\":{\"input_tokens\":100,\"output_tokens\":200,\"\
             input_tokens_details\":{\"cached_tokens\":50},\"\
             output_tokens_details\":{\"reasoning_tokens\":10}}}\n\n",
        );
        let c = usage_counts_from_response_body_for_log(true, frames.as_bytes());
        assert_eq!(c.prompt_tokens, 100);
        assert_eq!(c.completion_tokens, 200);
        assert_eq!(c.prompt_cache_read_tokens, 50);
        assert_eq!(c.reasoning_tokens, 10);
    }
}
