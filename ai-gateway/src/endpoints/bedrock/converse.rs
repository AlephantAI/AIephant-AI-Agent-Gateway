use aws_sdk_bedrockruntime::{
    operation::converse::ConverseInput, types::ConverseStreamOutput,
};
use serde::{Deserialize, Serialize};

use crate::{
    endpoints::{AiRequest, Endpoint},
    error::mapper::MapperError,
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Converse;

impl Endpoint for Converse {
    const PATH: &'static str = "model/{model_id}/converse";
    type RequestBody = ConverseInput;
    type ResponseBody = ConverseResponse;
    type StreamResponseBody = ConverseStreamOutput;
    type ErrorResponseBody = ConverseError;
}

impl AiRequest for ConverseInput {
    fn is_stream(&self) -> bool {
        false
    }

    fn model(&self) -> Result<ModelId, MapperError> {
        let model =
            self.model_id.as_ref().ok_or(MapperError::InvalidRequest)?;
        ModelId::from_str_and_provider(InferenceProvider::Bedrock, model)
    }
}

// The AWS SDK does not document the error format so instead we use a unit
// struct and simply rely on the http status codes to map to the OpenAI error.
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub struct ConverseError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseResponse {
    pub output: Option<ConverseResponseOutput>,
    pub stop_reason: String,
    pub usage: Option<ConverseTokenUsage>,
    pub trace: Option<ConverseTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConverseResponseOutput {
    pub message: ConverseMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConverseMessage {
    pub content: Vec<ConverseContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseContentBlock {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, rename = "toolUse")]
    pub tool_use: Option<ConverseToolUseBlock>,
    #[serde(default, rename = "toolResult")]
    pub tool_result: Option<ConverseToolResultBlock>,
    #[serde(default, rename = "reasoningContent")]
    pub reasoning_content: Option<ConverseReasoningContent>,
    #[serde(default, rename = "guardContent")]
    pub guard_content: Option<ConverseGuardContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseToolUseBlock {
    pub tool_use_id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseToolResultBlock {
    pub tool_use_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConverseReasoningContent {
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConverseGuardContent {
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseTokenUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    #[serde(default)]
    pub cache_read_input_tokens: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConverseTrace {
    #[serde(default)]
    pub prompt_router: Option<PromptRouterTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRouterTrace {
    #[serde(default)]
    pub invoked_model_id: Option<String>,
}
