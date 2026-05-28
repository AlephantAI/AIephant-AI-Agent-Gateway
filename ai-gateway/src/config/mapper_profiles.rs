use derive_more::AsRef;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::{
    middleware::mapper::{
        capabilities::ProviderCapabilities,
        families::ProviderProtocolFamily,
        non_stream_profile::{
            FinishReasonMappingMode, MessageContentMode, ResponseContentMode,
            ToolCallMappingMode, UsageMappingMode,
        },
        rules::{
            MultimodalMode, ReasoningMode, ResponseFormatMode, StreamMode,
            SystemHandling, ToolChoiceMode,
        },
    },
    types::provider::InferenceProvider,
};

const MAPPER_PROFILES_YAML: &str =
    include_str!("../../config/embedded/mapper-profiles.yaml");

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MapperCapabilitiesPatch {
    pub openai_compatible: Option<bool>,
    pub supports_response_format: Option<bool>,
    pub supports_tool_choice: Option<bool>,
    pub supports_parallel_tool_calls: Option<bool>,
    pub supports_thinking: Option<bool>,
    pub supports_streaming_reasoning: Option<bool>,
}

impl From<ProviderCapabilities> for MapperCapabilitiesPatch {
    fn from(value: ProviderCapabilities) -> Self {
        Self {
            openai_compatible: Some(value.openai_compatible),
            supports_response_format: Some(value.supports_response_format),
            supports_tool_choice: Some(value.supports_tool_choice),
            supports_parallel_tool_calls: Some(
                value.supports_parallel_tool_calls,
            ),
            supports_thinking: Some(value.supports_thinking),
            supports_streaming_reasoning: Some(
                value.supports_streaming_reasoning,
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct RequestProfilePatch {
    pub system_handling: Option<SystemHandling>,
    pub message_content_mode: Option<MessageContentMode>,
    pub tool_choice_mode: Option<ToolChoiceMode>,
    pub response_format_mode: Option<ResponseFormatMode>,
    pub reasoning_mode: Option<ReasoningMode>,
    pub multimodal_mode: Option<MultimodalMode>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ResponseProfilePatch {
    pub content_mode: Option<ResponseContentMode>,
    pub tool_call_mapping_mode: Option<ToolCallMappingMode>,
    pub finish_reason_mapping_mode: Option<FinishReasonMappingMode>,
    pub usage_mapping_mode: Option<UsageMappingMode>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct StreamProfilePatch {
    pub stream_mode: Option<StreamMode>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct MapperProfileCatalogEntry {
    pub family: Option<ProviderProtocolFamily>,
    pub capabilities: Option<MapperCapabilitiesPatch>,
    pub request: Option<RequestProfilePatch>,
    pub response: Option<ResponseProfilePatch>,
    pub stream: Option<StreamProfilePatch>,
}

#[derive(Debug, Clone, Deserialize, Serialize, AsRef, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MapperProfilesConfig {
    pub family_defaults:
        IndexMap<ProviderProtocolFamily, MapperProfileCatalogEntry>,
    pub provider_defaults:
        IndexMap<InferenceProvider, MapperProfileCatalogEntry>,
    pub model_overrides: IndexMap<String, MapperProfileCatalogEntry>,
}

impl Default for MapperProfilesConfig {
    fn default() -> Self {
        serde_yml::from_str(MAPPER_PROFILES_YAML)
            .expect("Always valid if tests pass")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::mapper::rules::ReasoningMode;

    #[test]
    fn test_default_mapper_profiles_config_loads_from_yaml_string() {
        let _default_config = MapperProfilesConfig::default();
    }

    #[test]
    fn test_default_mapper_profiles_config_contains_required_entries() {
        let default_config = MapperProfilesConfig::default();

        assert!(
            default_config
                .family_defaults
                .contains_key(&ProviderProtocolFamily::OpenAiCompatible),
            "family-defaults should include openai-compatible"
        );
        assert!(
            default_config
                .provider_defaults
                .contains_key(&InferenceProvider::Anthropic),
            "provider-defaults should include anthropic"
        );
        assert!(
            default_config
                .model_overrides
                .contains_key("deepseek/deepseek-reasoner"),
            "model-overrides should include deepseek/deepseek-reasoner"
        );
    }

    #[test]
    fn test_default_openai_compatible_reasoning_mode_is_unsupported() {
        let default_config = MapperProfilesConfig::default();
        let family_entry = default_config
            .family_defaults
            .get(&ProviderProtocolFamily::OpenAiCompatible)
            .expect("openai-compatible family default should exist");
        let request = family_entry
            .request
            .as_ref()
            .expect("family defaults should include request defaults");

        assert_eq!(
            request.reasoning_mode,
            Some(ReasoningMode::Unsupported),
            "family-defaults.openai-compatible.request.reasoning-mode must be \
             unsupported"
        );
    }

    #[test]
    fn test_mapper_profiles_support_partial_provider_and_model_overrides() {
        let yaml = r"
family-defaults:
  openai-compatible:
    request:
      reasoning-mode: unsupported
provider-defaults:
  anthropic:
    request:
      reasoning-mode: map-to-thinking
model-overrides:
  deepseek/deepseek-reasoner:
    request:
      reasoning-mode: passthrough
";

        let config: MapperProfilesConfig = serde_yml::from_str(yaml)
            .expect("partial layered entries should deserialize");

        let provider_entry = config
            .provider_defaults
            .get(&InferenceProvider::Anthropic)
            .expect("anthropic provider default should exist");
        let provider_request = provider_entry
            .request
            .as_ref()
            .expect("provider override should include request patch");

        assert_eq!(
            provider_request.reasoning_mode,
            Some(ReasoningMode::MapToThinking)
        );
        assert_eq!(provider_request.system_handling, None);
    }

    #[test]
    fn test_family_defaults_include_all_supported_families() {
        let config = MapperProfilesConfig::default();
        let families = [
            ProviderProtocolFamily::OpenAiCompatible,
            ProviderProtocolFamily::AnthropicMessages,
            ProviderProtocolFamily::BedrockConverse,
            ProviderProtocolFamily::GeminiOpenAiLike,
            ProviderProtocolFamily::OllamaChat,
        ];

        for family in families {
            let entry = config
                .family_defaults
                .get(&family)
                .expect("all supported families should have a baseline entry");
            assert!(
                entry.capabilities.is_some(),
                "family baseline should include capabilities: {family:?}"
            );
            assert!(
                entry.request.is_some(),
                "family baseline should include request profile: {family:?}"
            );
            assert!(
                entry.response.is_some(),
                "family baseline should include response profile: {family:?}"
            );
            assert!(
                entry.stream.is_some(),
                "family baseline should include stream profile: {family:?}"
            );
        }
    }

    #[test]
    fn test_openai_provider_default_overrides_reasoning_and_capabilities() {
        let config = MapperProfilesConfig::default();
        let openai = config
            .provider_defaults
            .get(&InferenceProvider::OpenAI)
            .expect("provider-defaults.openai should exist");
        let request = openai
            .request
            .as_ref()
            .expect("provider-defaults.openai should include request patch");
        let capabilities = openai.capabilities.as_ref().expect(
            "provider-defaults.openai should include capabilities patch",
        );

        assert_eq!(request.reasoning_mode, Some(ReasoningMode::Passthrough));
        assert_eq!(capabilities.supports_thinking, Some(true));
        assert_eq!(capabilities.supports_streaming_reasoning, Some(true));
    }
}
