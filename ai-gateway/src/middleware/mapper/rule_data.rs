use super::{profile_resolver::resolve_mapper_metadata, rules::ProviderRuleSet};
use crate::types::provider::InferenceProvider;

#[must_use]
pub fn default_named_provider_rules(provider_name: &str) -> ProviderRuleSet {
    default_provider_rules(&InferenceProvider::Named(provider_name.into()))
}

#[must_use]
pub fn default_provider_rules(provider: &InferenceProvider) -> ProviderRuleSet {
    resolve_mapper_metadata(provider, None)
        .expect("embedded provider mapper metadata must validate")
        .rules
}

#[cfg(test)]
mod tests {
    use crate::types::provider::InferenceProvider;

    fn assert_evidence_backed_openai_compatible_rule_profile(provider_name: &str) {
        let rules = super::default_named_provider_rules(provider_name);

        assert_eq!(
            rules.family,
            crate::middleware::mapper::families::ProviderProtocolFamily::OpenAiCompatible
        );
        assert_eq!(
            rules.request.tool_choice_mode,
            crate::middleware::mapper::rules::ToolChoiceMode::Native
        );
        assert_eq!(
            rules.request.response_format_mode,
            crate::middleware::mapper::rules::ResponseFormatMode::Passthrough
        );
        assert_eq!(
            rules.request.reasoning_mode,
            crate::middleware::mapper::rules::ReasoningMode::Unsupported
        );
        assert_eq!(
            rules.request.multimodal_mode,
            crate::middleware::mapper::rules::MultimodalMode::OpenAiStyle
        );
        assert_eq!(
            rules.stream.stream_mode,
            crate::middleware::mapper::rules::StreamMode::OpenAiSse
        );
    }

    #[test]
    fn default_named_provider_rules_qwen_is_explicit_profile() {
        let rules = super::default_named_provider_rules("qwen");

        assert_eq!(
            rules.family,
            crate::middleware::mapper::families::ProviderProtocolFamily::OpenAiCompatible
        );
        assert_eq!(
            rules.request.response_format_mode,
            crate::middleware::mapper::rules::ResponseFormatMode::Passthrough
        );
        assert_eq!(
            rules.request.reasoning_mode,
            crate::middleware::mapper::rules::ReasoningMode::Unsupported
        );
        assert_eq!(
            rules.request.multimodal_mode,
            crate::middleware::mapper::rules::MultimodalMode::OpenAiStyle
        );
    }

    #[test]
    fn default_named_provider_rules_unknown_falls_back_to_generic_profile() {
        let rules = super::default_named_provider_rules("custom-openai");

        assert_eq!(
            rules.family,
            crate::middleware::mapper::families::ProviderProtocolFamily::OpenAiCompatible
        );
        assert_eq!(
            rules.request.tool_choice_mode,
            crate::middleware::mapper::rules::ToolChoiceMode::Native
        );
        assert_eq!(
            rules.request.response_format_mode,
            crate::middleware::mapper::rules::ResponseFormatMode::Passthrough
        );
        assert_eq!(
            rules.request.reasoning_mode,
            crate::middleware::mapper::rules::ReasoningMode::Unsupported
        );
    }

    #[test]
    fn default_named_provider_rules_mistral_is_kept_at_evidence_backed_defaults() {
        assert_evidence_backed_openai_compatible_rule_profile("mistral");
    }

    #[test]
    fn default_named_provider_rules_groq_is_kept_at_evidence_backed_defaults() {
        assert_evidence_backed_openai_compatible_rule_profile("groq");
    }

    #[test]
    fn default_named_provider_rules_xai_is_kept_at_evidence_backed_defaults() {
        assert_evidence_backed_openai_compatible_rule_profile("xai");
    }

    #[test]
    fn default_named_provider_rules_hyperbolic_is_kept_at_evidence_backed_defaults() {
        assert_evidence_backed_openai_compatible_rule_profile("hyperbolic");
    }

    #[test]
    fn default_provider_rules_deepseek_use_openai_compatible_family() {
        let rules = super::default_provider_rules(&InferenceProvider::Named("deepseek".into()));

        assert_eq!(
            rules.family,
            crate::middleware::mapper::families::ProviderProtocolFamily::OpenAiCompatible
        );
    }

    #[test]
    fn default_provider_rules_anthropic_map_reasoning_to_thinking() {
        let rules = super::default_provider_rules(&InferenceProvider::Anthropic);

        assert_eq!(
            rules.request.reasoning_mode,
            crate::middleware::mapper::rules::ReasoningMode::MapToThinking
        );
    }

    #[test]
    fn default_provider_rules_bedrock_map_reasoning_to_additional_fields() {
        let rules = super::default_provider_rules(&InferenceProvider::Bedrock);

        assert_eq!(
            rules.request.reasoning_mode,
            crate::middleware::mapper::rules::ReasoningMode::MapToAdditionalFields
        );
    }
}
