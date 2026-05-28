//! Heuristics for “first model token” in streamed LLM responses (TTFT).

/// Returns `true` when `bytes` is JSON from a provider stream chunk that
/// carries the first non-empty model text (OpenAI-style `delta.content` /
/// `delta.reasoning_content`, or Anthropic `content_block_delta`).
#[must_use]
pub fn chunk_has_first_model_token(bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };

    if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            let Some(delta) = choice.get("delta").and_then(|d| d.as_object())
            else {
                continue;
            };
            for key in ["content", "reasoning_content"] {
                if let Some(s) = delta.get(key).and_then(|x| x.as_str())
                    && !s.is_empty()
                {
                    return true;
                }
            }
        }
        return false;
    }

    if v.get("type").and_then(|t| t.as_str()) == Some("content_block_delta")
        && let Some(text) = v
            .pointer("/delta/text")
            .and_then(|t| t.as_str())
            .or_else(|| {
                v.get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
            })
    {
        return !text.is_empty();
    }

    // OpenAI Responses API: text/reasoning deltas carry a top-level `"delta"`
    // string.
    if let Some("response.output_text.delta" | "response.reasoning_text.delta") =
        v.get("type").and_then(|t| t.as_str())
        && let Some(delta) = v.get("delta").and_then(|d| d.as_str())
    {
        return !delta.is_empty();
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_empty_delta_is_not_first_token() {
        let j = br#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert!(!chunk_has_first_model_token(j));
    }

    #[test]
    fn openai_content_delta_is_first_token() {
        let j = br#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        assert!(chunk_has_first_model_token(j));
    }

    #[test]
    fn anthropic_text_delta_is_first_token() {
        let j = br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#;
        assert!(chunk_has_first_model_token(j));
    }

    #[test]
    fn responses_output_text_delta_is_first_token() {
        let j = br#"{"type":"response.output_text.delta","item_id":"item_0","output_index":0,"content_index":0,"delta":"Hello"}"#;
        assert!(chunk_has_first_model_token(j));
    }

    #[test]
    fn responses_reasoning_text_delta_is_first_token() {
        let j = br#"{"type":"response.reasoning_text.delta","item_id":"item_0","output_index":0,"content_index":0,"delta":"Let me"}"#;
        assert!(chunk_has_first_model_token(j));
    }

    #[test]
    fn responses_created_is_not_first_token() {
        let j = br#"{"type":"response.created","response":{"id":"resp_1","status":"in_progress"}}"#;
        assert!(!chunk_has_first_model_token(j));
    }

    #[test]
    fn responses_empty_delta_is_not_first_token() {
        let j = br#"{"type":"response.output_text.delta","item_id":"item_0","output_index":0,"content_index":0,"delta":""}"#;
        assert!(!chunk_has_first_model_token(j));
    }
}
