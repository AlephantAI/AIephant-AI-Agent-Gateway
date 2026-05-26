use async_openai::types::{CreateChatCompletionResponse, CreateChatCompletionStreamResponse};

use crate::endpoints::Endpoint;

use super::super::openai::OpenAICompatibleChatCompletionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GenerateContents;

impl Endpoint for GenerateContents {
    const PATH: &'static str = "generateContent";
    type RequestBody = OpenAICompatibleChatCompletionRequest;
    type ResponseBody = CreateChatCompletionResponse;
    type StreamResponseBody = CreateChatCompletionStreamResponse;
    type ErrorResponseBody = async_openai::error::WrappedError;
}
