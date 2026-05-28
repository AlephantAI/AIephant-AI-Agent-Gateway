//! Helpers for Anthropic Messages SSE → OpenAI stream `usage` aggregation.

use async_openai::types::{
    CacheWriteDetails, CompletionTokensDetails, CompletionUsage, PromptTokensDetails,
};

use crate::types::extensions::AnthropicStreamOpenAiUsageState;

impl AnthropicStreamOpenAiUsageState {
    pub fn on_message_start(
        &mut self,
        message: &anthropic_ai_sdk::types::message::MessageStartContent,
    ) {
        self.stream_message_id.clone_from(&message.id);
        self.stream_model.clone_from(&message.model);
        let u = &message.usage;
        self.input_tokens = u.input_tokens;
        self.cache_read_input_tokens = u.cache_read_input_tokens.unwrap_or(0);
        self.cache_creation_input_tokens = u.cache_creation_input_tokens.unwrap_or(0);
        if let Some(c) = &u.cache_creation {
            self.cache_ephemeral_5m = c.ephemeral_5m_input_tokens;
            self.cache_ephemeral_1h = c.ephemeral_1h_input_tokens;
        } else {
            self.cache_ephemeral_5m = 0;
            self.cache_ephemeral_1h = 0;
        }
        self.output_tokens = u.output_tokens;
    }

    pub fn on_message_delta(
        &mut self,
        usage: &Option<anthropic_ai_sdk::types::message::StreamUsage>,
    ) {
        let Some(u) = usage else {
            return;
        };
        self.output_tokens = u.output_tokens;
        if let Some(v) = u.cache_read_input_tokens {
            self.cache_read_input_tokens = v;
        }
        if let Some(v) = u.cache_creation_input_tokens {
            self.cache_creation_input_tokens = v;
        }
        if let Some(c) = &u.cache_creation {
            self.cache_ephemeral_5m = c.ephemeral_5m_input_tokens;
            self.cache_ephemeral_1h = c.ephemeral_1h_input_tokens;
        }
        if u.input_tokens > 0 {
            self.input_tokens = u.input_tokens;
        }
    }

    /// Build OpenAI `CompletionUsage` from accumulated Anthropic stream fields.
    #[must_use]
    pub fn build_openai_completion_usage(&self) -> CompletionUsage {
        let cached = self.cache_read_input_tokens;
        let cache_write = self.cache_creation_input_tokens;
        let prompt_tokens = self.input_tokens.saturating_add(cached);
        let completion_tokens = self.output_tokens;
        let total = self
            .input_tokens
            .saturating_add(completion_tokens)
            .saturating_add(cached)
            .saturating_add(cache_write);

        let prompt_details = if cached > 0 || cache_write > 0 {
            Some(PromptTokensDetails {
                audio_tokens: None,
                cached_tokens: (cached > 0).then_some(cached),
                cache_write_tokens: (cache_write > 0).then_some(cache_write),
                cache_write_details: (cache_write > 0).then_some(CacheWriteDetails {
                    write_5m_tokens: if self.cache_ephemeral_5m > 0 {
                        self.cache_ephemeral_5m
                    } else {
                        cache_write
                    },
                    write_1h_tokens: self.cache_ephemeral_1h,
                }),
            })
        } else {
            None
        };

        CompletionUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: total,
            prompt_tokens_details: prompt_details,
            completion_tokens_details: Some(CompletionTokensDetails {
                reasoning_tokens: None,
                audio_tokens: Some(0),
                accepted_prediction_tokens: Some(0),
                rejected_prediction_tokens: Some(0),
            }),
        }
    }
}
