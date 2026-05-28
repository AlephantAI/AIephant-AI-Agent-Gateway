//! Layer D — generic heuristic (structure overhead + CJK-aware
//! `tokens_per_char`).

use crate::middleware::large_context::parse::ChatCompletionsPayload;

const TOKENS_PER_CHAR_BASE: f64 = 0.25;
/// Extra weight when the text is CJK-heavy (ideographs + fullwidth punctuation,
/// etc.). Pure ASCII uses `BASE` only.
const TOKENS_PER_CHAR_CJK_EXTRA: f64 = 0.55;
const TOKENS_PER_MESSAGE_STRUCTURE: u32 = 4;

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Characters that typically consume more tokens per codepoint than ASCII
/// (CJK blocks + fullwidth forms + kana; **not** only U+4E00–U+9FFF).
#[must_use]
fn char_is_cjk_related_dense(ch: char) -> bool {
    matches!(
        ch,
        '\u{4e00}'..='\u{9fff}' | // CJK Unified Ideographs
        '\u{3400}'..='\u{4dbf}' | // Extension A
        '\u{3000}'..='\u{303f}' | // CJK Symbols and Punctuation (U+3000–U+303F)
        '\u{ff00}'..='\u{ffef}' | // Halfwidth and Fullwidth Forms
        '\u{3040}'..='\u{309f}' | // Hiragana
        '\u{30a0}'..='\u{30ff}' // Katakana
    )
}

/// Returns `None` when the payload contains non-text message parts
/// (multimodal).
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
#[must_use]
pub fn estimate_heuristic_layer_d(
    payload: &ChatCompletionsPayload,
) -> Option<u32> {
    if payload.has_non_text_message_content {
        return None;
    }

    let mut total_chars = 0usize;
    let mut cjk_chars = 0usize;

    for message in &payload.messages {
        let normalized = normalize_text(&message.content);
        for ch in normalized.chars() {
            total_chars = total_chars.saturating_add(1);
            if char_is_cjk_related_dense(ch) {
                cjk_chars = cjk_chars.saturating_add(1);
            }
        }
    }

    if let Some(tools) = payload.tools.as_ref() {
        let tools_str = tools.to_string();
        for ch in tools_str.chars() {
            total_chars = total_chars.saturating_add(1);
            if char_is_cjk_related_dense(ch) {
                cjk_chars = cjk_chars.saturating_add(1);
            }
        }
    }

    let cjk_ratio = if total_chars == 0 {
        0.0
    } else {
        cjk_chars as f64 / total_chars as f64
    };

    let effective =
        TOKENS_PER_CHAR_BASE + TOKENS_PER_CHAR_CJK_EXTRA * cjk_ratio;
    let base_tokens = (total_chars as f64 * effective).ceil() as u32;
    let structure =
        payload.messages.len() as u32 * TOKENS_PER_MESSAGE_STRUCTURE;

    Some(base_tokens.saturating_add(structure))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::estimate_heuristic_layer_d;
    use crate::middleware::large_context::parse::{
        ChatCompletionsPayload, parse_chat_completions_payload,
    };

    fn payload_short_dense_heuristic() -> ChatCompletionsPayload {
        parse_chat_completions_payload(
            &serde_json::to_vec(&json!({
                "model": "openai/gpt-4",
                "messages": [
                    {
                        "role": "user",
                        "content": "Supercalifragilisticexpialidocious pseudopseudohypoparathyroidism \
                            floccinaucinihilipilification antidisestablishmentarianism \
                            electroencephalographically disproportionately miscellaneousness"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap()
        .expect("payload")
    }

    #[test]
    fn heuristic_layer_d_short_dense_not_trivially_three() {
        let payload = payload_short_dense_heuristic();
        let est = estimate_heuristic_layer_d(&payload).expect("estimate");
        // Old pure `chars * 0.25` gave ~3; layer D must be much larger.
        assert!(est >= 10, "expected >= 10, got {est}");
        assert!(est < 500, "expected < 500, got {est}");
    }

    #[test]
    fn heuristic_layer_d_multimodal_returns_none() {
        let payload = parse_chat_completions_payload(
            &serde_json::to_vec(&json!({
                "model": "openai/gpt-4o-mini",
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            { "type": "text", "text": "hello" }
                        ]
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap()
        .expect("payload");
        assert!(estimate_heuristic_layer_d(&payload).is_none());
    }
}
