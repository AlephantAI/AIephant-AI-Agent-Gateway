use std::str::FromStr;

use async_openai::types::CreateChatCompletionRequest;

use crate::types::model_id::ModelId;

#[derive(Debug, Clone)]
pub struct OpenAiRequestParams {
    pub raw_model: String,
    pub source_model: Option<ModelId>,
    pub is_stream: bool,
}

impl OpenAiRequestParams {
    #[must_use]
    pub fn from_request(request: &CreateChatCompletionRequest) -> Self {
        Self {
            raw_model: request.model.clone(),
            source_model: ModelId::from_str(&request.model).ok(),
            is_stream: request.stream.unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::CreateChatCompletionRequest;
    use serde_json::json;

    use super::OpenAiRequestParams;

    #[test]
    fn request_params_extract_stream_and_model_without_failing_on_unknown() {
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "some-model-without-provider",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "stream": false
        }))
        .expect("request should deserialize");

        let params = OpenAiRequestParams::from_request(&request);

        assert_eq!(params.raw_model, "some-model-without-provider");
        assert!(params.source_model.is_none());
        assert!(!params.is_stream);
    }
}
