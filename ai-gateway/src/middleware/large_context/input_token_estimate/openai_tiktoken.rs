//! Layer A — OpenAI chat token count via `tiktoken-rs`
//! (`num_tokens_from_messages` + tools JSON).
//!
//! **Why `prompt_tokens` can be much larger (e.g. ~20 vs ~96):** the API often
//! counts **system/developer/tool** text the provider injects **after** the
//! gateway. We only see the JSON body, so the estimate tracks **body-only**
//! chat tokens (plus `tools` in the body). Compare apples to apples when
//! validating.

use tiktoken_rs::{
    ChatCompletionRequestMessage, bpe_for_model, num_tokens_from_messages,
    tokenizer::get_tokenizer,
};

use crate::middleware::large_context::parse::ChatCompletionsPayload;

/// Mirrors `tiktoken_rs::num_tokens_from_messages` when that function bails on
/// the tokenizer check but [`bpe_for_model`] still works (same cookbook logic).
fn chat_tokens_cookbook_fallback(
    model: &str,
    messages: &[ChatCompletionRequestMessage],
) -> Option<usize> {
    let bpe = bpe_for_model(model).ok()?;
    const REPLY_PRIMING: i32 = 3;
    let (tokens_per_message, tokens_per_name) = if model == "gpt-3.5-turbo-0301"
    {
        (4, -1)
    } else {
        (3, 1)
    };

    let mut num_tokens: i32 = 0;
    for message in messages {
        num_tokens += tokens_per_message;
        num_tokens += bpe.count_with_special_tokens(&message.role) as i32;
        if let Some(content) = &message.content {
            num_tokens += bpe.count_with_special_tokens(content) as i32;
        }
        if let Some(name) = &message.name {
            num_tokens += bpe.count_with_special_tokens(name) as i32;
            num_tokens += tokens_per_name;
        }
    }
    num_tokens += REPLY_PRIMING;
    Some(num_tokens.max(0) as usize)
}

/// Strips `provider/model` and fallback list to a single model id for tiktoken
/// lookup.
#[must_use]
pub fn openai_model_suffix(primary_model: &str) -> &str {
    let after_slash = primary_model
        .split_once('/')
        .map_or(primary_model, |(_, m)| m);
    after_slash.split(',').next().map_or(after_slash, str::trim)
}

/// Returns `None` if the model is unknown to tiktoken or chat counting is
/// unsupported.
#[must_use]
pub fn count_tokens_openai_profile(
    payload: &ChatCompletionsPayload,
    primary_model: &str,
) -> Option<u32> {
    let model = openai_model_suffix(primary_model);
    get_tokenizer(model)?;

    let messages: Vec<ChatCompletionRequestMessage> = payload
        .messages
        .iter()
        .map(|m| ChatCompletionRequestMessage {
            role: m.role.clone().unwrap_or_else(|| "user".to_string()),
            content: Some(m.content.clone()),
            ..Default::default()
        })
        .collect();

    let mut n = match num_tokens_from_messages(model, &messages) {
        Ok(v) => v,
        Err(_) => chat_tokens_cookbook_fallback(model, &messages)?,
    };

    if let Some(tools) = &payload.tools {
        let bpe = bpe_for_model(model).ok()?;
        let tools_str = tools.to_string();
        n = n.saturating_add(bpe.count_with_special_tokens(&tools_str));
    }

    u32::try_from(n).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{count_tokens_openai_profile, openai_model_suffix};
    use crate::middleware::large_context::parse::parse_chat_completions_payload;

    #[test]
    fn openai_model_suffix_strips_provider() {
        assert_eq!(openai_model_suffix("openai/gpt-4o-mini"), "gpt-4o-mini");
    }

    #[test]
    fn tiktoken_short_dense_sentence_gt_char_heuristic() {
        let payload = parse_chat_completions_payload(
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
        .expect("payload");
        let tik = count_tokens_openai_profile(&payload, "openai/gpt-4")
            .expect("tiktoken");
        let chars = payload.messages[0].content.chars().count();
        let naive = u32::try_from(chars.div_ceil(4)).unwrap_or(u32::MAX);
        assert!(tik > naive, "tik={tik} naive={naive}");
        assert!(tik < 500);
    }
}
