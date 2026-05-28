//! Maps [`InferenceProvider`] / model id strings to estimation profiles.

use std::str::FromStr as _;

use crate::types::{model_id::ModelId, provider::InferenceProvider};

/// High-level estimation strategy (see design spec §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateProfile {
    /// Layer A — OpenAI-compatible chat completions; prefer tiktoken when
    /// available.
    OpenAiCompatible,
    /// Layer B — Gemini text; heuristic until remote count is wired.
    GeminiText,
    /// Layer B′ — Anthropic Messages; heuristic (no cl100k).
    AnthropicMessages,
    /// Layer B″ — Bedrock; refined by
    /// [`super::bedrock::refine_bedrock_profile`].
    BedrockMultiFoundation,
    /// Layer D — generic heuristic (incl. unknown `providers.code` / `Named`).
    HeuristicDefault,
}

#[must_use]
pub fn resolve_profile(
    provider_hint: Option<&InferenceProvider>,
    primary_model: &str,
) -> EstimateProfile {
    if let Some(p) = provider_hint {
        return profile_for_inference_provider(p);
    }
    ModelId::from_str(primary_model)
        .map(|mid| profile_from_model_id(&mid))
        .unwrap_or(EstimateProfile::HeuristicDefault)
}

fn profile_from_model_id(mid: &ModelId) -> EstimateProfile {
    match mid {
        ModelId::ModelIdWithVersion { provider, .. } => {
            profile_for_inference_provider(provider)
        }
        ModelId::Bedrock(_) => EstimateProfile::BedrockMultiFoundation,
        ModelId::Ollama(_) | ModelId::Unknown(_) => {
            EstimateProfile::HeuristicDefault
        }
    }
}

fn profile_for_inference_provider(p: &InferenceProvider) -> EstimateProfile {
    match p {
        InferenceProvider::OpenAI => EstimateProfile::OpenAiCompatible,
        InferenceProvider::Anthropic => EstimateProfile::AnthropicMessages,
        InferenceProvider::GoogleGemini => EstimateProfile::GeminiText,
        InferenceProvider::Bedrock => EstimateProfile::BedrockMultiFoundation,
        InferenceProvider::Ollama => EstimateProfile::HeuristicDefault,
        InferenceProvider::Custom => EstimateProfile::OpenAiCompatible,
        InferenceProvider::Named(name) => {
            if is_openai_compatible_code(name.as_str()) {
                EstimateProfile::OpenAiCompatible
            } else {
                EstimateProfile::HeuristicDefault
            }
        }
    }
}

/// DB / gateway `providers.code` values that use OpenAI-compatible Chat
/// Completions.
#[must_use]
pub(crate) fn is_openai_compatible_code(code: &str) -> bool {
    let c = code.trim();
    c.eq_ignore_ascii_case("openai")
        || c.eq_ignore_ascii_case("azure")
        || c.eq_ignore_ascii_case("deepseek")
        || c.eq_ignore_ascii_case("mistral")
        || c.eq_ignore_ascii_case("groq")
        || c.eq_ignore_ascii_case("moonshotai")
        || c.eq_ignore_ascii_case("qwen")
        || c.eq_ignore_ascii_case("minimax")
        || c.eq_ignore_ascii_case("xai")
        || c.eq_ignore_ascii_case("openrouter")
        || c.eq_ignore_ascii_case("nvidia")
        || c.eq_ignore_ascii_case("hyperbolic")
}

#[cfg(test)]
mod tests {
    use super::{EstimateProfile, resolve_profile};
    use crate::types::provider::InferenceProvider;

    #[test]
    fn profile_prefers_inference_provider_when_some() {
        assert_eq!(
            resolve_profile(
                Some(&InferenceProvider::Anthropic),
                "openai/gpt-4o-mini",
            ),
            EstimateProfile::AnthropicMessages,
        );
    }

    #[test]
    fn profile_from_model_id_openai() {
        assert_eq!(
            resolve_profile(None, "openai/gpt-4o-mini"),
            EstimateProfile::OpenAiCompatible,
        );
    }

    #[test]
    fn profile_from_model_id_bedrock() {
        assert_eq!(
            resolve_profile(
                None,
                "bedrock/anthropic.claude-3-sonnet-20240229-v1:0"
            ),
            EstimateProfile::BedrockMultiFoundation,
        );
    }

    #[test]
    fn profile_named_deepseek_maps_to_openai_compatible() {
        assert_eq!(
            resolve_profile(
                Some(&InferenceProvider::Named("deepseek".into())),
                "deepseek/deepseek-chat",
            ),
            EstimateProfile::OpenAiCompatible,
        );
    }

    #[test]
    fn profile_unknown_named_falls_back_to_heuristic() {
        assert_eq!(
            resolve_profile(
                Some(&InferenceProvider::Named(
                    "totally-unknown-vendor".into()
                )),
                "totally-unknown-vendor/foo",
            ),
            EstimateProfile::HeuristicDefault,
        );
    }
}
