use super::{
    families::ProviderProtocolFamily,
    rules::{MultimodalMode, ReasoningMode, ResponseFormatMode, SystemHandling, ToolChoiceMode},
};
use crate::types::provider::InferenceProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MessageContentMode {
    #[serde(rename = "openai-style")]
    OpenAiStyle,
    AnthropicBlocks,
    BedrockContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseContentMode {
    Passthrough,
    AnthropicMessageContent,
    BedrockOutputMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolCallMappingMode {
    Native,
    ProviderSpecificHelper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinishReasonMappingMode {
    Passthrough,
    ProviderSpecificHelper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageMappingMode {
    Passthrough,
    ProviderSpecificHelper,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RequestProfile {
    pub system_handling: SystemHandling,
    pub message_content_mode: MessageContentMode,
    pub tool_choice_mode: ToolChoiceMode,
    pub response_format_mode: ResponseFormatMode,
    pub reasoning_mode: ReasoningMode,
    pub multimodal_mode: MultimodalMode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ResponseProfile {
    pub content_mode: ResponseContentMode,
    pub tool_call_mapping_mode: ToolCallMappingMode,
    pub finish_reason_mapping_mode: FinishReasonMappingMode,
    pub usage_mapping_mode: UsageMappingMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonStreamFormatProfile {
    pub provider: InferenceProvider,
    pub family: ProviderProtocolFamily,
    pub request: RequestProfile,
    pub response: ResponseProfile,
}
