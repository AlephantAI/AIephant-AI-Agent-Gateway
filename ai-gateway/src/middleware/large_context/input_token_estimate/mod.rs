//! Hybrid input-token estimation for chat completions (policy + large context).
//!
//! These estimates are **not** guaranteed to match upstream
//! `usage.prompt_tokens`, especially when the gateway injects system or tool
//! metadata outside the parsed JSON body. Rows in `providers` without an
//! OpenAI-compat mapping use [`EstimateProfile::HeuristicDefault`].
//!
//! See `docs/superpowers/specs/2026-04-18-input-token-estimation-hybrid-design.
//! md`.

mod bedrock;
mod heuristic_layer_d;
mod openai_tiktoken;
mod profile;

pub use profile::{EstimateProfile, resolve_profile};

use crate::{
    middleware::large_context::parse::ChatCompletionsPayload, types::provider::InferenceProvider,
};

/// Estimates input tokens for a parsed Chat Completions payload.
#[must_use]
pub fn estimate_chat_completion_input_tokens(
    payload: &ChatCompletionsPayload,
    primary_model: &str,
    provider_hint: Option<&InferenceProvider>,
) -> Option<u32> {
    if payload.has_non_text_message_content {
        return None;
    }

    let profile = match resolve_profile(provider_hint, primary_model) {
        EstimateProfile::BedrockMultiFoundation => bedrock::refine_bedrock_profile(primary_model),
        p => p,
    };

    match profile {
        EstimateProfile::OpenAiCompatible => {
            let tik = openai_tiktoken::count_tokens_openai_profile(payload, primary_model);
            let heur = heuristic_layer_d::estimate_heuristic_layer_d(payload);
            match (tik, heur) {
                (Some(t), Some(h)) => Some(t.max(h)),
                (Some(t), None) => Some(t),
                (None, Some(h)) => Some(h),
                (None, None) => None,
            }
        }
        EstimateProfile::AnthropicMessages
        | EstimateProfile::GeminiText
        | EstimateProfile::HeuristicDefault
        | EstimateProfile::BedrockMultiFoundation => {
            heuristic_layer_d::estimate_heuristic_layer_d(payload)
        }
    }
}
