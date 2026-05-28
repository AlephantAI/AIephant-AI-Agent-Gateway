use crate::types::provider::InferenceProvider;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderProtocolFamily {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    AnthropicMessages,
    BedrockConverse,
    GeminiOpenAiLike,
    OllamaChat,
}

impl ProviderProtocolFamily {
    #[must_use]
    pub fn for_provider(provider: &InferenceProvider) -> Self {
        match provider {
            InferenceProvider::OpenAI
            | InferenceProvider::Custom
            | InferenceProvider::Named(_) => Self::OpenAiCompatible,
            InferenceProvider::Anthropic => Self::AnthropicMessages,
            InferenceProvider::Bedrock => Self::BedrockConverse,
            InferenceProvider::GoogleGemini => Self::GeminiOpenAiLike,
            InferenceProvider::Ollama => Self::OllamaChat,
        }
    }

    #[must_use]
    pub fn matches_provider(self, provider: &InferenceProvider) -> bool {
        Self::for_provider(provider) == self
    }
}

#[cfg(test)]
mod tests {
    use crate::types::provider::InferenceProvider;

    #[test]
    fn provider_protocol_family_maps_named_provider_to_openai_compatible() {
        let family = super::ProviderProtocolFamily::for_provider(
            &InferenceProvider::Named("deepseek".into()),
        );

        assert_eq!(family, super::ProviderProtocolFamily::OpenAiCompatible);
    }

    #[test]
    fn provider_protocol_family_maps_anthropic_provider() {
        let family = super::ProviderProtocolFamily::for_provider(
            &InferenceProvider::Anthropic,
        );

        assert_eq!(family, super::ProviderProtocolFamily::AnthropicMessages);
    }

    #[test]
    fn provider_protocol_family_maps_bedrock_provider() {
        let family = super::ProviderProtocolFamily::for_provider(
            &InferenceProvider::Bedrock,
        );

        assert_eq!(family, super::ProviderProtocolFamily::BedrockConverse);
    }

    #[test]
    fn provider_protocol_family_matches_provider() {
        assert!(
            super::ProviderProtocolFamily::GeminiOpenAiLike
                .matches_provider(&InferenceProvider::GoogleGemini)
        );
        assert!(
            !super::ProviderProtocolFamily::AnthropicMessages
                .matches_provider(&InferenceProvider::Bedrock)
        );
    }
}
