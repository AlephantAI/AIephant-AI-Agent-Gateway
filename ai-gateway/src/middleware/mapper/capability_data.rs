use super::{capabilities::ProviderCapabilities, profile_resolver::resolve_mapper_metadata};
use crate::types::provider::InferenceProvider;

#[must_use]
pub fn default_named_provider_capabilities(provider_name: &str) -> ProviderCapabilities {
    default_provider_capabilities(&InferenceProvider::Named(provider_name.into()))
}

#[must_use]
pub fn default_provider_capabilities(provider: &InferenceProvider) -> ProviderCapabilities {
    resolve_mapper_metadata(provider, None)
        .expect("embedded provider mapper metadata must validate")
        .capabilities
}

#[cfg(test)]
mod tests {
    fn assert_openai_compatible_passthrough_profile(provider_name: &str) {
        let capabilities = super::default_named_provider_capabilities(provider_name);

        assert!(capabilities.openai_compatible);
        assert!(capabilities.supports_response_format);
        assert!(capabilities.supports_tool_choice);
        assert!(capabilities.supports_parallel_tool_calls);
        assert!(!capabilities.supports_thinking);
        assert!(!capabilities.supports_streaming_reasoning);
    }

    #[test]
    fn default_named_provider_capabilities_qwen_is_explicit_profile() {
        let capabilities = super::default_named_provider_capabilities("qwen");

        assert!(capabilities.openai_compatible);
        assert!(capabilities.supports_response_format);
        assert!(capabilities.supports_tool_choice);
        assert!(!capabilities.supports_thinking);
    }

    #[test]
    fn default_named_provider_capabilities_unknown_falls_back_to_generic_profile() {
        let capabilities = super::default_named_provider_capabilities("custom-openai");

        assert!(capabilities.openai_compatible);
        assert!(capabilities.supports_parallel_tool_calls);
        assert!(!capabilities.supports_streaming_reasoning);
    }

    #[test]
    fn default_named_provider_capabilities_mistral_is_kept_at_evidence_backed_defaults() {
        assert_openai_compatible_passthrough_profile("mistral");
    }

    #[test]
    fn default_named_provider_capabilities_groq_is_kept_at_evidence_backed_defaults() {
        assert_openai_compatible_passthrough_profile("groq");
    }

    #[test]
    fn default_named_provider_capabilities_xai_is_kept_at_evidence_backed_defaults() {
        assert_openai_compatible_passthrough_profile("xai");
    }

    #[test]
    fn default_named_provider_capabilities_hyperbolic_is_kept_at_evidence_backed_defaults() {
        assert_openai_compatible_passthrough_profile("hyperbolic");
    }
}
