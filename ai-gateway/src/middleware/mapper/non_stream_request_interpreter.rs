use async_openai::types::CreateChatCompletionRequest;
use serde_json::Value;

use super::{
    non_stream_profile::NonStreamFormatProfile,
    rules::{
        MultimodalMode, ReasoningMode, ResponseFormatMode, ToolChoiceMode,
    },
};
use crate::error::mapper::MapperError;

pub fn apply_non_stream_request_profile(
    profile: &NonStreamFormatProfile,
    value: &mut CreateChatCompletionRequest,
) -> Result<(), MapperError> {
    if matches!(
        profile.request.response_format_mode,
        ResponseFormatMode::Unsupported
    ) {
        value.response_format = None;
    }

    if matches!(
        profile.request.tool_choice_mode,
        ToolChoiceMode::Unsupported
    ) {
        value.tools = None;
        value.tool_choice = None;
        value.parallel_tool_calls = None;
    }

    if matches!(profile.request.reasoning_mode, ReasoningMode::Unsupported) {
        value.reasoning_effort = None;
    }

    if matches!(profile.request.multimodal_mode, MultimodalMode::Unsupported)
        && request_contains_multimodal_content(value)?
    {
        return Err(MapperError::ImageMappingInvalid(format!(
            "provider {} does not support multimodal requests",
            profile.provider
        )));
    }

    Ok(())
}

fn request_contains_multimodal_content(
    value: &CreateChatCompletionRequest,
) -> Result<bool, MapperError> {
    let messages = serde_json::to_value(&value.messages)
        .map_err(MapperError::SerdeError)?;
    Ok(contains_image_url_part(&messages))
}

fn contains_image_url_part(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("image_url") {
                return true;
            }
            map.values().any(contains_image_url_part)
        }
        Value::Array(values) => values.iter().any(contains_image_url_part),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        error::mapper::MapperError,
        middleware::mapper::{
            non_stream_profile_data::default_non_stream_profile,
            rules::{
                MultimodalMode, ReasoningMode, ResponseFormatMode,
                ToolChoiceMode,
            },
        },
        types::provider::InferenceProvider,
    };

    #[test]
    fn request_interpreter_strips_response_format_when_profile_marks_it_unsupported()
     {
        let mut profile = default_non_stream_profile(
            &InferenceProvider::Named("qwen".into()),
        );
        profile.request.response_format_mode = ResponseFormatMode::Unsupported;

        let mut request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "qwen/qwen-plus",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "response_format": {
                    "type": "json_object"
                }
            }))
            .expect("request should deserialize");

        super::apply_non_stream_request_profile(&profile, &mut request)
            .expect("request interpretation should succeed");

        assert!(request.response_format.is_none());
    }

    #[test]
    fn request_interpreter_strips_tool_fields_when_profile_marks_tool_choice_unsupported()
     {
        let mut profile = default_non_stream_profile(
            &InferenceProvider::Named("qwen".into()),
        );
        profile.request.tool_choice_mode = ToolChoiceMode::Unsupported;

        let mut request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "qwen/qwen-plus",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "weather"
                        }
                    }
                ],
                "tool_choice": "auto",
                "parallel_tool_calls": true
            }))
            .expect("request should deserialize");

        super::apply_non_stream_request_profile(&profile, &mut request)
            .expect("request interpretation should succeed");

        assert!(request.tools.is_none());
        assert!(request.tool_choice.is_none());
        assert!(request.parallel_tool_calls.is_none());
    }

    #[test]
    fn request_interpreter_strips_reasoning_effort_when_profile_marks_reasoning_unsupported()
     {
        let mut profile = default_non_stream_profile(
            &InferenceProvider::Named("qwen".into()),
        );
        profile.request.reasoning_mode = ReasoningMode::Unsupported;

        let mut request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "qwen/qwen-plus",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request should deserialize");

        super::apply_non_stream_request_profile(&profile, &mut request)
            .expect("request interpretation should succeed");

        assert!(request.reasoning_effort.is_none());
    }

    #[test]
    fn request_interpreter_rejects_multimodal_when_profile_marks_it_unsupported()
     {
        let mut profile = default_non_stream_profile(
            &InferenceProvider::Named("qwen".into()),
        );
        profile.request.multimodal_mode = MultimodalMode::Unsupported;

        let mut request: async_openai::types::CreateChatCompletionRequest =
            serde_json::from_value(json!({
                "model": "qwen/qwen-plus",
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "text",
                                "text": "describe the image"
                            },
                            {
                                "type": "image_url",
                                "image_url": {
                                    "url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/a1sAAAAASUVORK5CYII="
                                }
                            }
                        ]
                    }
                ]
            }))
            .expect("request should deserialize");

        let err =
            super::apply_non_stream_request_profile(&profile, &mut request)
                .expect_err("multimodal request should fail");

        assert!(matches!(err, MapperError::ImageMappingInvalid(_)));
    }
}
