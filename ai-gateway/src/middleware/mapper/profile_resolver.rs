use std::sync::OnceLock;

use super::{
    capabilities::ProviderCapabilities,
    families::ProviderProtocolFamily,
    non_stream_profile::{
        NonStreamFormatProfile, RequestProfile, ResponseProfile,
    },
    rule_validator::{ProviderRuleValidationError, validate_provider_rules},
    rules::{
        FinishReasonMapping, ProviderRequestRuleSet, ProviderResponseRuleSet,
        ProviderRuleSet, ProviderStreamRuleSet, ToolCallMapping, UsageMapping,
    },
};
use crate::{
    config::mapper_profiles::{
        MapperCapabilitiesPatch, MapperProfileCatalogEntry,
        MapperProfilesConfig, RequestProfilePatch, ResponseProfilePatch,
        StreamProfilePatch,
    },
    types::provider::InferenceProvider,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMapperMetadata {
    pub capabilities: ProviderCapabilities,
    pub rules: ProviderRuleSet,
    pub non_stream_profile: NonStreamFormatProfile,
}

pub fn resolve_mapper_metadata(
    provider: &InferenceProvider,
    model: Option<&str>,
) -> Result<ResolvedMapperMetadata, ProviderRuleValidationError> {
    let config = mapper_profiles_config();
    resolve_mapper_metadata_from_config(config, provider, model)
}

fn resolve_mapper_metadata_from_config(
    config: &MapperProfilesConfig,
    provider: &InferenceProvider,
    model: Option<&str>,
) -> Result<ResolvedMapperMetadata, ProviderRuleValidationError> {
    let provider_entry = config.provider_defaults.get(provider);
    let model_entry = model.and_then(|name| config.model_overrides.get(name));
    let family = model_entry
        .and_then(|entry| entry.family)
        .or_else(|| provider_entry.and_then(|entry| entry.family))
        .unwrap_or_else(|| ProviderProtocolFamily::for_provider(provider));
    let family_entry =
        config.family_defaults.get(&family).unwrap_or_else(|| {
            panic!("missing family mapper baseline for {family:?}")
        });
    let mut metadata =
        resolved_from_family_entry(provider.clone(), family, family_entry);

    apply_catalog_entry(&mut metadata, provider_entry);
    apply_catalog_entry(&mut metadata, model_entry);

    validate_provider_rules(&metadata.capabilities, &metadata.rules)?;

    Ok(metadata)
}

fn mapper_profiles_config() -> &'static MapperProfilesConfig {
    static CONFIG: OnceLock<MapperProfilesConfig> = OnceLock::new();
    CONFIG.get_or_init(MapperProfilesConfig::default)
}

fn resolved_from_family_entry(
    provider: InferenceProvider,
    family: ProviderProtocolFamily,
    entry: &MapperProfileCatalogEntry,
) -> ResolvedMapperMetadata {
    let capabilities_patch = entry.capabilities.as_ref().unwrap_or_else(|| {
        panic!("family mapper baseline missing capabilities for {family:?}")
    });
    let request_patch = entry.request.as_ref().unwrap_or_else(|| {
        panic!("family mapper baseline missing request profile for {family:?}")
    });
    let response_patch = entry.response.as_ref().unwrap_or_else(|| {
        panic!("family mapper baseline missing response profile for {family:?}")
    });
    let stream_patch = entry.stream.as_ref().unwrap_or_else(|| {
        panic!("family mapper baseline missing stream profile for {family:?}")
    });

    ResolvedMapperMetadata {
        capabilities: provider_capabilities_from_patch(
            provider.clone(),
            capabilities_patch,
        ),
        rules: provider_rules_from_patches(
            provider.clone(),
            family,
            request_patch,
            response_patch,
            stream_patch,
        ),
        non_stream_profile: non_stream_profile_from_patches(
            provider,
            family,
            request_patch,
            response_patch,
        ),
    }
}

fn provider_capabilities_from_patch(
    provider: InferenceProvider,
    patch: &MapperCapabilitiesPatch,
) -> ProviderCapabilities {
    ProviderCapabilities {
        provider,
        openai_compatible: patch
            .openai_compatible
            .expect("family capabilities must define openai_compatible"),
        supports_response_format: patch
            .supports_response_format
            .expect("family capabilities must define supports_response_format"),
        supports_tool_choice: patch
            .supports_tool_choice
            .expect("family capabilities must define supports_tool_choice"),
        supports_parallel_tool_calls: patch
            .supports_parallel_tool_calls
            .expect(
                "family capabilities must define supports_parallel_tool_calls",
            ),
        supports_thinking: patch
            .supports_thinking
            .expect("family capabilities must define supports_thinking"),
        supports_streaming_reasoning: patch
            .supports_streaming_reasoning
            .expect(
                "family capabilities must define supports_streaming_reasoning",
            ),
    }
}

fn provider_rules_from_patches(
    provider: InferenceProvider,
    family: ProviderProtocolFamily,
    request: &RequestProfilePatch,
    response: &ResponseProfilePatch,
    stream: &StreamProfilePatch,
) -> ProviderRuleSet {
    ProviderRuleSet {
        provider,
        family,
        request: ProviderRequestRuleSet {
            system_handling: request
                .system_handling
                .expect("family request patch must define system_handling"),
            tool_choice_mode: request
                .tool_choice_mode
                .expect("family request patch must define tool_choice_mode"),
            response_format_mode: request.response_format_mode.expect(
                "family request patch must define response_format_mode",
            ),
            reasoning_mode: request
                .reasoning_mode
                .expect("family request patch must define reasoning_mode"),
            multimodal_mode: request
                .multimodal_mode
                .expect("family request patch must define multimodal_mode"),
        },
        response: ProviderResponseRuleSet {
            finish_reason_mapping: finish_reason_mapping_from_mode(
                response.finish_reason_mapping_mode.expect(
                    "family response patch must define \
                     finish_reason_mapping_mode",
                ),
            ),
            tool_call_mapping: tool_call_mapping_from_mode(
                response.tool_call_mapping_mode.expect(
                    "family response patch must define tool_call_mapping_mode",
                ),
            ),
            usage_mapping: usage_mapping_from_mode(
                response.usage_mapping_mode.expect(
                    "family response patch must define usage_mapping_mode",
                ),
            ),
        },
        stream: ProviderStreamRuleSet {
            stream_mode: stream
                .stream_mode
                .expect("family stream patch must define stream_mode"),
        },
    }
}

fn non_stream_profile_from_patches(
    provider: InferenceProvider,
    family: ProviderProtocolFamily,
    request: &RequestProfilePatch,
    response: &ResponseProfilePatch,
) -> NonStreamFormatProfile {
    NonStreamFormatProfile {
        provider,
        family,
        request: RequestProfile {
            system_handling: request
                .system_handling
                .expect("family request patch must define system_handling"),
            message_content_mode: request.message_content_mode.expect(
                "family request patch must define message_content_mode",
            ),
            tool_choice_mode: request
                .tool_choice_mode
                .expect("family request patch must define tool_choice_mode"),
            response_format_mode: request.response_format_mode.expect(
                "family request patch must define response_format_mode",
            ),
            reasoning_mode: request
                .reasoning_mode
                .expect("family request patch must define reasoning_mode"),
            multimodal_mode: request
                .multimodal_mode
                .expect("family request patch must define multimodal_mode"),
        },
        response: ResponseProfile {
            content_mode: response
                .content_mode
                .expect("family response patch must define content_mode"),
            tool_call_mapping_mode: response.tool_call_mapping_mode.expect(
                "family response patch must define tool_call_mapping_mode",
            ),
            finish_reason_mapping_mode: response
                .finish_reason_mapping_mode
                .expect(
                    "family response patch must define \
                     finish_reason_mapping_mode",
                ),
            usage_mapping_mode: response
                .usage_mapping_mode
                .expect("family response patch must define usage_mapping_mode"),
        },
    }
}

fn apply_catalog_entry(
    metadata: &mut ResolvedMapperMetadata,
    entry: Option<&MapperProfileCatalogEntry>,
) {
    let Some(entry) = entry else {
        return;
    };

    if let Some(capabilities) = entry.capabilities.as_ref() {
        apply_capabilities_patch(&mut metadata.capabilities, capabilities);
    }
    if let Some(request) = entry.request.as_ref() {
        apply_request_patch(metadata, request);
    }
    if let Some(response) = entry.response.as_ref() {
        apply_response_patch(metadata, response);
    }
    if let Some(stream) = entry.stream.as_ref() {
        apply_stream_patch(&mut metadata.rules, stream);
    }
}

fn apply_capabilities_patch(
    capabilities: &mut ProviderCapabilities,
    patch: &MapperCapabilitiesPatch,
) {
    if let Some(value) = patch.openai_compatible {
        capabilities.openai_compatible = value;
    }
    if let Some(value) = patch.supports_response_format {
        capabilities.supports_response_format = value;
    }
    if let Some(value) = patch.supports_tool_choice {
        capabilities.supports_tool_choice = value;
    }
    if let Some(value) = patch.supports_parallel_tool_calls {
        capabilities.supports_parallel_tool_calls = value;
    }
    if let Some(value) = patch.supports_thinking {
        capabilities.supports_thinking = value;
    }
    if let Some(value) = patch.supports_streaming_reasoning {
        capabilities.supports_streaming_reasoning = value;
    }
}

fn apply_request_patch(
    metadata: &mut ResolvedMapperMetadata,
    patch: &RequestProfilePatch,
) {
    if let Some(value) = patch.system_handling {
        metadata.rules.request.system_handling = value;
        metadata.non_stream_profile.request.system_handling = value;
    }
    if let Some(value) = patch.message_content_mode {
        metadata.non_stream_profile.request.message_content_mode = value;
    }
    if let Some(value) = patch.tool_choice_mode {
        metadata.rules.request.tool_choice_mode = value;
        metadata.non_stream_profile.request.tool_choice_mode = value;
    }
    if let Some(value) = patch.response_format_mode {
        metadata.rules.request.response_format_mode = value;
        metadata.non_stream_profile.request.response_format_mode = value;
    }
    if let Some(value) = patch.reasoning_mode {
        metadata.rules.request.reasoning_mode = value;
        metadata.non_stream_profile.request.reasoning_mode = value;
    }
    if let Some(value) = patch.multimodal_mode {
        metadata.rules.request.multimodal_mode = value;
        metadata.non_stream_profile.request.multimodal_mode = value;
    }
}

fn apply_response_patch(
    metadata: &mut ResolvedMapperMetadata,
    patch: &ResponseProfilePatch,
) {
    if let Some(value) = patch.content_mode {
        metadata.non_stream_profile.response.content_mode = value;
    }
    if let Some(value) = patch.tool_call_mapping_mode {
        metadata.rules.response.tool_call_mapping =
            tool_call_mapping_from_mode(value);
        metadata.non_stream_profile.response.tool_call_mapping_mode = value;
    }
    if let Some(value) = patch.finish_reason_mapping_mode {
        metadata.rules.response.finish_reason_mapping =
            finish_reason_mapping_from_mode(value);
        metadata
            .non_stream_profile
            .response
            .finish_reason_mapping_mode = value;
    }
    if let Some(value) = patch.usage_mapping_mode {
        metadata.rules.response.usage_mapping = usage_mapping_from_mode(value);
        metadata.non_stream_profile.response.usage_mapping_mode = value;
    }
}

fn apply_stream_patch(rules: &mut ProviderRuleSet, patch: &StreamProfilePatch) {
    if let Some(value) = patch.stream_mode {
        rules.stream.stream_mode = value;
    }
}

fn finish_reason_mapping_from_mode(
    mode: crate::middleware::mapper::non_stream_profile::FinishReasonMappingMode,
) -> FinishReasonMapping {
    match mode {
        crate::middleware::mapper::non_stream_profile::FinishReasonMappingMode::Passthrough => {
            FinishReasonMapping::Passthrough
        }
        crate::middleware::mapper::non_stream_profile::FinishReasonMappingMode::ProviderSpecificHelper => {
            FinishReasonMapping::ProviderSpecific
        }
    }
}

fn tool_call_mapping_from_mode(
    mode: crate::middleware::mapper::non_stream_profile::ToolCallMappingMode,
) -> ToolCallMapping {
    match mode {
        crate::middleware::mapper::non_stream_profile::ToolCallMappingMode::Native => {
            ToolCallMapping::Native
        }
        crate::middleware::mapper::non_stream_profile::ToolCallMappingMode::ProviderSpecificHelper => {
            ToolCallMapping::ProviderSpecific
        }
    }
}

fn usage_mapping_from_mode(
    mode: crate::middleware::mapper::non_stream_profile::UsageMappingMode,
) -> UsageMapping {
    match mode {
        crate::middleware::mapper::non_stream_profile::UsageMappingMode::Passthrough => {
            UsageMapping::Passthrough
        }
        crate::middleware::mapper::non_stream_profile::UsageMappingMode::ProviderSpecificHelper => {
            UsageMapping::ProviderSpecific
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_mapper_metadata, resolve_mapper_metadata_from_config};
    use crate::{
        config::mapper_profiles::MapperProfilesConfig,
        middleware::mapper::{
            capability_data::default_provider_capabilities,
            non_stream_profile_data::default_non_stream_profile,
            rule_data::default_provider_rules,
            rule_validator::ProviderRuleValidationError, rules::ReasoningMode,
        },
        types::provider::InferenceProvider,
    };

    #[test]
    fn resolve_mapper_metadata_applies_model_override_after_provider_and_family_defaults()
     {
        let metadata = resolve_mapper_metadata(
            &InferenceProvider::Named("deepseek".into()),
            Some("deepseek/deepseek-reasoner"),
        )
        .expect("deepseek reasoner metadata should be valid");

        assert_eq!(
            metadata.rules.request.reasoning_mode,
            ReasoningMode::Passthrough
        );
        assert_eq!(
            metadata.non_stream_profile.request.reasoning_mode,
            ReasoningMode::Passthrough
        );
        assert!(metadata.capabilities.supports_thinking);
    }

    #[test]
    fn resolve_mapper_metadata_falls_back_to_provider_or_family_default_when_model_has_no_override()
     {
        let metadata = resolve_mapper_metadata(
            &InferenceProvider::Named("deepseek".into()),
            Some("deepseek/deepseek-chat"),
        )
        .expect("deepseek chat metadata should be valid");

        assert_eq!(
            metadata.rules.request.reasoning_mode,
            ReasoningMode::Unsupported
        );
        assert_eq!(
            metadata.non_stream_profile.request.reasoning_mode,
            ReasoningMode::Unsupported
        );
        assert!(!metadata.capabilities.supports_thinking);
    }

    #[test]
    fn provider_level_wrappers_match_resolver_defaults() {
        let provider = InferenceProvider::Anthropic;
        let metadata = resolve_mapper_metadata(&provider, None)
            .expect("provider defaults should be valid");

        assert_eq!(
            default_provider_capabilities(&provider),
            metadata.capabilities
        );
        assert_eq!(default_provider_rules(&provider), metadata.rules);
        assert_eq!(
            default_non_stream_profile(&provider),
            metadata.non_stream_profile
        );
    }

    #[test]
    fn resolve_mapper_metadata_from_config_rejects_invalid_merged_metadata() {
        let config: MapperProfilesConfig = serde_yml::from_str(
            r"
family-defaults:
  openai-compatible:
    family: openai-compatible
    capabilities:
      openai-compatible: true
      supports-response-format: true
      supports-tool-choice: true
      supports-parallel-tool-calls: true
      supports-thinking: false
      supports-streaming-reasoning: false
    request:
      system-handling: first-class-field
      message-content-mode: openai-style
      tool-choice-mode: native
      response-format-mode: passthrough
      reasoning-mode: unsupported
      multimodal-mode: openai-style
    response:
      content-mode: passthrough
      tool-call-mapping-mode: native
      finish-reason-mapping-mode: passthrough
      usage-mapping-mode: passthrough
    stream:
      stream-mode: openai-sse
provider-defaults: {}
model-overrides:
  custom/model:
    capabilities:
      supports-response-format: false
",
        )
        .expect("synthetic config should deserialize");

        let err = resolve_mapper_metadata_from_config(
            &config,
            &InferenceProvider::Named("custom".into()),
            Some("custom/model"),
        )
        .expect_err("invalid merged metadata must be rejected");

        assert!(matches!(
            err,
            ProviderRuleValidationError::ResponseFormatCapabilityMismatch { .. }
        ));
    }
}
