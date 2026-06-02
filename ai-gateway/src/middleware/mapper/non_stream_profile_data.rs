use super::{
    non_stream_profile::NonStreamFormatProfile, profile_resolver::resolve_mapper_metadata,
};
use crate::types::provider::InferenceProvider;

#[must_use]
pub fn default_non_stream_profile(provider: &InferenceProvider) -> NonStreamFormatProfile {
    resolve_mapper_metadata(provider, None)
        .expect("embedded provider mapper metadata must validate")
        .non_stream_profile
}

#[cfg(test)]
mod tests {
    use crate::{
        middleware::mapper::{
            families::ProviderProtocolFamily,
            non_stream_profile::{
                FinishReasonMappingMode, MessageContentMode, ResponseContentMode,
                ToolCallMappingMode,
            },
        },
        types::provider::InferenceProvider,
    };

    #[test]
    fn default_non_stream_profile_for_named_provider_uses_openai_modes() {
        let profile = super::default_non_stream_profile(&InferenceProvider::Named("qwen".into()));

        assert_eq!(profile.family, ProviderProtocolFamily::OpenAiCompatible);
        assert_eq!(
            profile.request.message_content_mode,
            MessageContentMode::OpenAiStyle
        );
        assert_eq!(
            profile.response.content_mode,
            ResponseContentMode::Passthrough
        );
    }

    #[test]
    fn default_non_stream_profile_for_anthropic_uses_blocks_and_provider_specific_response() {
        let profile = super::default_non_stream_profile(&InferenceProvider::Anthropic);

        assert_eq!(profile.family, ProviderProtocolFamily::AnthropicMessages);
        assert_eq!(
            profile.request.message_content_mode,
            MessageContentMode::AnthropicBlocks
        );
        assert_eq!(
            profile.response.tool_call_mapping_mode,
            ToolCallMappingMode::ProviderSpecificHelper
        );
        assert_eq!(
            profile.response.finish_reason_mapping_mode,
            FinishReasonMappingMode::ProviderSpecificHelper
        );
    }
}
