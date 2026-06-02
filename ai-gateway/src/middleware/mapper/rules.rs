use super::families::ProviderProtocolFamily;
use crate::types::provider::InferenceProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemHandling {
    Ignore,
    Merge,
    FirstClassField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolChoiceMode {
    Native,
    MapToAny,
    MapToTool,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseFormatMode {
    Passthrough,
    Unsupported,
    ProviderSpecificHelper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningMode {
    Passthrough,
    MapToThinking,
    MapToAdditionalFields,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MultimodalMode {
    #[serde(rename = "openai-style")]
    OpenAiStyle,
    ContentBlocks,
    ImagesOnly,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinishReasonMapping {
    Passthrough,
    ProviderSpecific,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolCallMapping {
    Native,
    ProviderSpecific,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageMapping {
    Passthrough,
    ProviderSpecific,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StreamMode {
    #[serde(rename = "openai-sse")]
    OpenAiSse,
    AnthropicEvents,
    BedrockEventStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestRuleSet {
    pub system_handling: SystemHandling,
    pub tool_choice_mode: ToolChoiceMode,
    pub response_format_mode: ResponseFormatMode,
    pub reasoning_mode: ReasoningMode,
    pub multimodal_mode: MultimodalMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponseRuleSet {
    pub finish_reason_mapping: FinishReasonMapping,
    pub tool_call_mapping: ToolCallMapping,
    pub usage_mapping: UsageMapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStreamRuleSet {
    pub stream_mode: StreamMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuleSet {
    pub provider: InferenceProvider,
    pub family: ProviderProtocolFamily,
    pub request: ProviderRequestRuleSet,
    pub response: ProviderResponseRuleSet,
    pub stream: ProviderStreamRuleSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRuleContext {
    pub provider: InferenceProvider,
    pub family: ProviderProtocolFamily,
    pub system_handling: SystemHandling,
    pub tool_choice_mode: ToolChoiceMode,
    pub response_format_mode: ResponseFormatMode,
    pub reasoning_mode: ReasoningMode,
    pub multimodal_mode: MultimodalMode,
    pub supports_parallel_tool_calls: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_protocol_family_rule_struct_exposes_reasoning_mode() {
        let rules = super::ProviderRuleSet {
            provider: crate::types::provider::InferenceProvider::Anthropic,
            family: crate::middleware::mapper::families::ProviderProtocolFamily::AnthropicMessages,
            request: super::ProviderRequestRuleSet {
                system_handling: super::SystemHandling::FirstClassField,
                tool_choice_mode: super::ToolChoiceMode::Native,
                response_format_mode: super::ResponseFormatMode::Unsupported,
                reasoning_mode: super::ReasoningMode::MapToThinking,
                multimodal_mode: super::MultimodalMode::ContentBlocks,
            },
            response: super::ProviderResponseRuleSet {
                finish_reason_mapping: super::FinishReasonMapping::ProviderSpecific,
                tool_call_mapping: super::ToolCallMapping::Native,
                usage_mapping: super::UsageMapping::ProviderSpecific,
            },
            stream: super::ProviderStreamRuleSet {
                stream_mode: super::StreamMode::AnthropicEvents,
            },
        };

        assert_eq!(
            rules.request.reasoning_mode,
            super::ReasoningMode::MapToThinking
        );
        assert_eq!(
            rules.request.system_handling,
            super::SystemHandling::FirstClassField
        );
        assert_eq!(
            rules.request.response_format_mode,
            super::ResponseFormatMode::Unsupported
        );
        assert_eq!(rules.stream.stream_mode, super::StreamMode::AnthropicEvents);
    }
}
