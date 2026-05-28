use std::fmt;

use super::{
    capabilities::ProviderCapabilities,
    families::ProviderProtocolFamily,
    rules::{ProviderRuleSet, ReasoningMode, ResponseFormatMode, ToolChoiceMode},
};
use crate::types::provider::InferenceProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRuleValidationError {
    FamilyCapabilityMismatch {
        provider: InferenceProvider,
        family: ProviderProtocolFamily,
        openai_compatible: bool,
    },
    ReasoningCapabilityMismatch {
        provider: InferenceProvider,
        reasoning_mode: ReasoningMode,
    },
    ResponseFormatCapabilityMismatch {
        provider: InferenceProvider,
        response_format_mode: ResponseFormatMode,
    },
    ToolChoiceCapabilityMismatch {
        provider: InferenceProvider,
        tool_choice_mode: ToolChoiceMode,
    },
}

impl fmt::Display for ProviderRuleValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FamilyCapabilityMismatch {
                provider,
                family,
                openai_compatible,
            } => write!(
                f,
                "provider {provider} has incompatible family {family:?} for \
                 openai_compatible={openai_compatible}"
            ),
            Self::ReasoningCapabilityMismatch {
                provider,
                reasoning_mode,
            } => write!(
                f,
                "provider {provider} does not support reasoning mode \
                 {reasoning_mode:?}"
            ),
            Self::ResponseFormatCapabilityMismatch {
                provider,
                response_format_mode,
            } => write!(
                f,
                "provider {provider} does not support response format mode \
                 {response_format_mode:?}"
            ),
            Self::ToolChoiceCapabilityMismatch {
                provider,
                tool_choice_mode,
            } => write!(
                f,
                "provider {provider} does not support tool choice mode \
                 {tool_choice_mode:?}"
            ),
        }
    }
}

impl std::error::Error for ProviderRuleValidationError {}

pub fn validate_provider_rules(
    capabilities: &ProviderCapabilities,
    rules: &ProviderRuleSet,
) -> Result<(), ProviderRuleValidationError> {
    let family_is_openai_compatible = matches!(
        rules.family,
        ProviderProtocolFamily::OpenAiCompatible
            | ProviderProtocolFamily::GeminiOpenAiLike
            | ProviderProtocolFamily::OllamaChat
    );

    if family_is_openai_compatible != capabilities.openai_compatible
        && !(rules.family == ProviderProtocolFamily::GeminiOpenAiLike
            && capabilities.provider == InferenceProvider::GoogleGemini)
        && !(rules.family == ProviderProtocolFamily::OllamaChat
            && capabilities.provider == InferenceProvider::Ollama)
    {
        return Err(ProviderRuleValidationError::FamilyCapabilityMismatch {
            provider: capabilities.provider.clone(),
            family: rules.family,
            openai_compatible: capabilities.openai_compatible,
        });
    }

    if matches!(
        rules.request.reasoning_mode,
        ReasoningMode::MapToThinking
            | ReasoningMode::MapToAdditionalFields
            | ReasoningMode::Passthrough
    ) && !capabilities.supports_thinking
        && !matches!(rules.request.reasoning_mode, ReasoningMode::Passthrough)
    {
        return Err(ProviderRuleValidationError::ReasoningCapabilityMismatch {
            provider: capabilities.provider.clone(),
            reasoning_mode: rules.request.reasoning_mode,
        });
    }

    if !capabilities.supports_response_format
        && !matches!(
            rules.request.response_format_mode,
            ResponseFormatMode::Unsupported
        )
    {
        return Err(
            ProviderRuleValidationError::ResponseFormatCapabilityMismatch {
                provider: capabilities.provider.clone(),
                response_format_mode: rules.request.response_format_mode,
            },
        );
    }

    if !capabilities.supports_tool_choice
        && !matches!(rules.request.tool_choice_mode, ToolChoiceMode::Unsupported)
    {
        return Err(ProviderRuleValidationError::ToolChoiceCapabilityMismatch {
            provider: capabilities.provider.clone(),
            tool_choice_mode: rules.request.tool_choice_mode,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        middleware::mapper::{
            capabilities::ProviderCapabilities,
            families::ProviderProtocolFamily,
            rules::{
                FinishReasonMapping, MultimodalMode, ProviderRequestRuleSet,
                ProviderResponseRuleSet, ProviderRuleSet, ProviderStreamRuleSet, ReasoningMode,
                ResponseFormatMode, StreamMode, SystemHandling, ToolCallMapping, ToolChoiceMode,
                UsageMapping,
            },
        },
        types::provider::InferenceProvider,
    };

    #[test]
    fn provider_rule_validator_rejects_incompatible_openai_family() {
        let capabilities = ProviderCapabilities {
            provider: InferenceProvider::Anthropic,
            openai_compatible: false,
            supports_response_format: false,
            supports_tool_choice: true,
            supports_parallel_tool_calls: true,
            supports_thinking: true,
            supports_streaming_reasoning: true,
        };
        let rules = ProviderRuleSet {
            provider: InferenceProvider::Anthropic,
            family: ProviderProtocolFamily::OpenAiCompatible,
            request: ProviderRequestRuleSet {
                system_handling: SystemHandling::FirstClassField,
                tool_choice_mode: ToolChoiceMode::Native,
                response_format_mode: ResponseFormatMode::Unsupported,
                reasoning_mode: ReasoningMode::MapToThinking,
                multimodal_mode: MultimodalMode::ContentBlocks,
            },
            response: ProviderResponseRuleSet {
                finish_reason_mapping: FinishReasonMapping::ProviderSpecific,
                tool_call_mapping: ToolCallMapping::Native,
                usage_mapping: UsageMapping::ProviderSpecific,
            },
            stream: ProviderStreamRuleSet {
                stream_mode: StreamMode::AnthropicEvents,
            },
        };

        let err = super::validate_provider_rules(&capabilities, &rules).expect_err("must fail");

        assert!(matches!(
            err,
            super::ProviderRuleValidationError::FamilyCapabilityMismatch { .. }
        ));
    }

    #[test]
    fn provider_rule_validator_rejects_response_format_mode_without_capability() {
        let capabilities = ProviderCapabilities {
            provider: InferenceProvider::Anthropic,
            openai_compatible: false,
            supports_response_format: false,
            supports_tool_choice: true,
            supports_parallel_tool_calls: true,
            supports_thinking: true,
            supports_streaming_reasoning: true,
        };
        let rules = ProviderRuleSet {
            provider: InferenceProvider::Anthropic,
            family: ProviderProtocolFamily::AnthropicMessages,
            request: ProviderRequestRuleSet {
                system_handling: SystemHandling::FirstClassField,
                tool_choice_mode: ToolChoiceMode::Native,
                response_format_mode: ResponseFormatMode::Passthrough,
                reasoning_mode: ReasoningMode::MapToThinking,
                multimodal_mode: MultimodalMode::ContentBlocks,
            },
            response: ProviderResponseRuleSet {
                finish_reason_mapping: FinishReasonMapping::ProviderSpecific,
                tool_call_mapping: ToolCallMapping::Native,
                usage_mapping: UsageMapping::ProviderSpecific,
            },
            stream: ProviderStreamRuleSet {
                stream_mode: StreamMode::AnthropicEvents,
            },
        };

        let err = super::validate_provider_rules(&capabilities, &rules).expect_err("must fail");

        assert!(matches!(
            err,
            super::ProviderRuleValidationError::ResponseFormatCapabilityMismatch { .. }
        ));
    }
}
