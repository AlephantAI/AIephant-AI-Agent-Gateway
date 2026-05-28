use anthropic_ai_sdk::types::message as anthropic;
use async_openai::types::CreateChatCompletionResponse;
use http::response::Parts;
use uuid::Uuid;

use super::non_stream_profile::NonStreamFormatProfile;
use crate::{
    endpoints::bedrock::converse::{ConverseResponse, ConverseTokenUsage},
    error::mapper::MapperError,
    middleware::mapper::response_normalizer::{
        build_chat_response, build_usage, normalize_tool_calls,
    },
    types::{extensions::MapperProfileContext, provider::InferenceProvider},
};

pub fn apply_non_stream_response_profile(
    _profile: &NonStreamFormatProfile,
    value: CreateChatCompletionResponse,
) -> Result<CreateChatCompletionResponse, MapperError> {
    Ok(value)
}

#[must_use]
pub fn profile_from_response_parts<'a>(
    resp_parts: &'a Parts,
    fallback: &'a NonStreamFormatProfile,
) -> &'a NonStreamFormatProfile {
    resp_parts
        .extensions
        .get::<MapperProfileContext>()
        .map_or(fallback, |ctx| &ctx.non_stream_profile)
}

pub fn apply_non_stream_response_profile_from_parts(
    resp_parts: &Parts,
    fallback: &NonStreamFormatProfile,
    value: CreateChatCompletionResponse,
) -> Result<CreateChatCompletionResponse, MapperError> {
    apply_non_stream_response_profile(
        profile_from_response_parts(resp_parts, fallback),
        value,
    )
}

pub fn convert_anthropic_response(
    profile: &NonStreamFormatProfile,
    value: anthropic::CreateMessageResponse,
) -> Result<CreateChatCompletionResponse, MapperError> {
    validate_anthropic_response_profile(profile)?;

    let id = value.id;
    let model = value.model;
    let usage =
        build_usage(value.usage.input_tokens, value.usage.output_tokens, None);

    let (content, tool_calls) = extract_anthropic_content(value.content)?;
    let finish_reason = map_anthropic_finish_reason(value.stop_reason.as_ref());

    Ok(build_chat_response(
        id,
        model,
        content,
        normalize_tool_calls(tool_calls),
        finish_reason,
        usage,
    ))
}

pub fn convert_anthropic_response_from_parts(
    resp_parts: &Parts,
    fallback: &NonStreamFormatProfile,
    value: anthropic::CreateMessageResponse,
) -> Result<CreateChatCompletionResponse, MapperError> {
    convert_anthropic_response(
        profile_from_response_parts(resp_parts, fallback),
        value,
    )
}

pub fn convert_bedrock_response(
    profile: &NonStreamFormatProfile,
    value: ConverseResponse,
) -> Result<CreateChatCompletionResponse, MapperError> {
    use async_openai::types as openai;

    validate_bedrock_response_profile(profile)?;

    let model = value
        .trace
        .and_then(|t| t.prompt_router)
        .and_then(|r| r.invoked_model_id)
        .unwrap_or_default();

    let usage = value.usage.unwrap_or(ConverseTokenUsage {
        input_tokens: super::DEFAULT_MAX_TOKENS as i32,
        output_tokens: super::DEFAULT_MAX_TOKENS as i32,
        total_tokens: super::DEFAULT_MAX_TOKENS as i32,
        cache_read_input_tokens: None,
    });

    let usage = build_usage(
        usage.input_tokens.try_into().unwrap_or(0),
        usage.output_tokens.try_into().unwrap_or(0),
        usage
            .cache_read_input_tokens
            .and_then(|i| i.try_into().ok()),
    );

    let mut tool_calls: Vec<openai::ChatCompletionMessageToolCall> = Vec::new();
    let mut content = None;
    let mut reasoning_text = None;
    let contents = if let Some(output) = value.output {
        output.message.content
    } else {
        Vec::new()
    };

    for bedrock_content in contents {
        match (
            bedrock_content.tool_use,
            bedrock_content.tool_result,
            bedrock_content.text,
            bedrock_content.reasoning_content,
            bedrock_content.guard_content,
        ) {
            (Some(tool_use_block), _, _, _, _) => {
                tool_calls.push(openai::ChatCompletionMessageToolCall {
                    id: tool_use_block.tool_use_id.clone(),
                    r#type: openai::ChatCompletionToolType::Function,
                    function: openai::FunctionCall {
                        name: tool_use_block.name.clone(),
                        arguments: serde_json::to_string(
                            &tool_use_block.input,
                        )?,
                    },
                });
            }
            (_, Some(tool_result_block), _, _, _) => {
                tool_calls.push(openai::ChatCompletionMessageToolCall {
                    id: tool_result_block.tool_use_id.clone(),
                    r#type: openai::ChatCompletionToolType::Function,
                    function: openai::FunctionCall {
                        name: tool_result_block.tool_use_id.clone(),
                        arguments: serde_json::to_string(&content)?,
                    },
                });
            }
            (_, _, Some(text), _, _) => {
                content = Some(text);
            }
            (_, _, _, Some(reasoning), _) => {
                if let Some(text) = reasoning.text {
                    reasoning_text = Some(text);
                }
            }
            (_, _, _, _, Some(guard)) => {
                if let Some(guard_content) = guard.text {
                    content = Some(guard_content);
                }
            }
            _ => {}
        }
    }

    let mut response = build_chat_response(
        Uuid::new_v4().to_string(),
        model,
        content,
        normalize_tool_calls(tool_calls),
        map_bedrock_finish_reason(&value.stop_reason),
        usage,
    );

    if let Some(reasoning) = reasoning_text {
        response.choices[0].message.reasoning_content = Some(reasoning);
    }

    Ok(response)
}

pub fn convert_bedrock_response_from_parts(
    resp_parts: &Parts,
    fallback: &NonStreamFormatProfile,
    value: ConverseResponse,
) -> Result<CreateChatCompletionResponse, MapperError> {
    convert_bedrock_response(
        profile_from_response_parts(resp_parts, fallback),
        value,
    )
}

fn validate_anthropic_response_profile(
    profile: &NonStreamFormatProfile,
) -> Result<(), MapperError> {
    if profile.provider != InferenceProvider::Anthropic {
        return Err(MapperError::ProviderNotSupported(format!(
            "anthropic response interpreter does not support provider {}",
            profile.provider
        )));
    }

    if !matches!(
        profile.response.content_mode,
        super::non_stream_profile::ResponseContentMode::AnthropicMessageContent
    ) {
        return Err(MapperError::InvalidRequest);
    }

    if !matches!(
        profile.response.tool_call_mapping_mode,
        super::non_stream_profile::ToolCallMappingMode::ProviderSpecificHelper
    ) {
        return Err(MapperError::InvalidRequest);
    }

    if !matches!(
        profile.response.finish_reason_mapping_mode,
        super::non_stream_profile::FinishReasonMappingMode::ProviderSpecificHelper
    ) {
        return Err(MapperError::InvalidRequest);
    }

    if !matches!(
        profile.response.usage_mapping_mode,
        super::non_stream_profile::UsageMappingMode::ProviderSpecificHelper
    ) {
        return Err(MapperError::InvalidRequest);
    }

    Ok(())
}

fn validate_bedrock_response_profile(
    profile: &NonStreamFormatProfile,
) -> Result<(), MapperError> {
    if profile.provider != InferenceProvider::Bedrock {
        return Err(MapperError::ProviderNotSupported(format!(
            "bedrock response interpreter does not support provider {}",
            profile.provider
        )));
    }

    if !matches!(
        profile.response.content_mode,
        super::non_stream_profile::ResponseContentMode::BedrockOutputMessage
    ) {
        return Err(MapperError::InvalidRequest);
    }

    if !matches!(
        profile.response.tool_call_mapping_mode,
        super::non_stream_profile::ToolCallMappingMode::ProviderSpecificHelper
    ) {
        return Err(MapperError::InvalidRequest);
    }

    if !matches!(
        profile.response.finish_reason_mapping_mode,
        super::non_stream_profile::FinishReasonMappingMode::ProviderSpecificHelper
    ) {
        return Err(MapperError::InvalidRequest);
    }

    if !matches!(
        profile.response.usage_mapping_mode,
        super::non_stream_profile::UsageMappingMode::ProviderSpecificHelper
    ) {
        return Err(MapperError::InvalidRequest);
    }

    Ok(())
}

fn extract_anthropic_content(
    blocks: Vec<anthropic::ContentBlock>,
) -> Result<
    (
        Option<String>,
        Vec<async_openai::types::ChatCompletionMessageToolCall>,
    ),
    MapperError,
> {
    use async_openai::types as openai;

    let mut tool_calls = Vec::new();
    let mut content = None;

    for block in blocks {
        match block {
            anthropic::ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(openai::ChatCompletionMessageToolCall {
                    id,
                    r#type: openai::ChatCompletionToolType::Function,
                    function: openai::FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input)?,
                    },
                });
            }
            anthropic::ContentBlock::ToolResult {
                tool_use_id,
                content: tool_content,
            } => tool_calls.push(openai::ChatCompletionMessageToolCall {
                id: tool_use_id.clone(),
                r#type: openai::ChatCompletionToolType::Function,
                function: openai::FunctionCall {
                    name: tool_use_id,
                    arguments: serde_json::to_string(&tool_content)?,
                },
            }),
            anthropic::ContentBlock::Text { text, .. } => {
                content = Some(text);
            }
            anthropic::ContentBlock::Image { .. }
            | anthropic::ContentBlock::Thinking { .. }
            | anthropic::ContentBlock::RedactedThinking { .. } => {}
        }
    }

    Ok((content, tool_calls))
}

fn map_anthropic_finish_reason(
    stop_reason: Option<&anthropic::StopReason>,
) -> Option<async_openai::types::FinishReason> {
    use async_openai::types::FinishReason;

    match stop_reason {
        Some(
            anthropic::StopReason::EndTurn
            | anthropic::StopReason::StopSequence,
        ) => Some(FinishReason::Stop),
        Some(anthropic::StopReason::MaxTokens) => Some(FinishReason::Length),
        Some(anthropic::StopReason::ToolUse) => Some(FinishReason::ToolCalls),
        Some(anthropic::StopReason::Refusal) => {
            Some(FinishReason::ContentFilter)
        }
        None => None,
    }
}

fn map_bedrock_finish_reason(
    value: &str,
) -> Option<async_openai::types::FinishReason> {
    use async_openai::types::FinishReason;

    match value {
        "end_turn" | "stop_sequence" => Some(FinishReason::Stop),
        "max_tokens" => Some(FinishReason::Length),
        "tool_use" => Some(FinishReason::ToolCalls),
        "content_filtered" | "guardrail_intervened" => {
            Some(FinishReason::ContentFilter)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use anthropic_ai_sdk::types::message as anthropic;
    use http::Response;

    use crate::{
        endpoints::bedrock::converse::{
            ConverseContentBlock, ConverseMessage, ConverseResponse,
            ConverseResponseOutput, ConverseTokenUsage, ConverseToolUseBlock,
            ConverseTrace, PromptRouterTrace,
        },
        middleware::mapper::{
            non_stream_profile::{ResponseContentMode, ToolCallMappingMode},
            non_stream_profile_data::default_non_stream_profile,
        },
        types::{
            extensions::MapperProfileContext, provider::InferenceProvider,
        },
    };

    #[test]
    fn profile_from_response_parts_prefers_mapper_profile_context() {
        let mut parts = Response::builder()
            .status(200)
            .body(())
            .expect("response should build")
            .into_parts()
            .0;
        let mut profile = default_non_stream_profile(
            &InferenceProvider::Named("deepseek".into()),
        );
        profile.response.content_mode = ResponseContentMode::Passthrough;
        parts.extensions.insert(MapperProfileContext {
            provider: InferenceProvider::Named("deepseek".into()),
            raw_model: "deepseek/deepseek-reasoner".into(),
            non_stream_profile: profile.clone(),
        });

        let fallback = default_non_stream_profile(&InferenceProvider::OpenAI);
        let resolved = super::profile_from_response_parts(&parts, &fallback);

        assert_eq!(
            resolved.provider,
            InferenceProvider::Named("deepseek".into())
        );
    }

    #[test]
    fn anthropic_response_from_parts_prefers_mapper_profile_context() {
        let mut parts = Response::builder()
            .status(200)
            .body(())
            .expect("response should build")
            .into_parts()
            .0;
        let anthropic_profile =
            default_non_stream_profile(&InferenceProvider::Anthropic);
        parts.extensions.insert(MapperProfileContext {
            provider: InferenceProvider::Anthropic,
            raw_model: "anthropic/claude-sonnet-4-0".into(),
            non_stream_profile: anthropic_profile.clone(),
        });

        let fallback = default_non_stream_profile(&InferenceProvider::Bedrock);
        let response = anthropic::CreateMessageResponse {
            id: "msg_text_ctx_1".to_string(),
            type_: "message".to_string(),
            role: anthropic::Role::Assistant,
            content: vec![anthropic::ContentBlock::Text {
                text: "Hello from Anthropic.".to_string(),
            }],
            model: "claude-sonnet-4-0".to_string(),
            stop_reason: Some(anthropic::StopReason::EndTurn),
            stop_sequence: None,
            usage: anthropic::Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            },
        };

        let converted = super::convert_anthropic_response_from_parts(
            &parts, &fallback, response,
        )
        .expect("response context should win over fallback");

        assert_eq!(
            converted.choices[0].message.content.as_deref(),
            Some("Hello from Anthropic.")
        );
    }

    #[test]
    fn anthropic_response_profile_maps_tool_use_blocks_and_finish_reason() {
        let profile = default_non_stream_profile(&InferenceProvider::Anthropic);
        let response = anthropic::CreateMessageResponse {
            id: "msg_tool_1".to_string(),
            type_: "message".to_string(),
            role: anthropic::Role::Assistant,
            content: vec![anthropic::ContentBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "lookup_weather".to_string(),
                input: serde_json::json!({
                    "city": "Paris"
                }),
            }],
            model: "claude-sonnet-4-0".to_string(),
            stop_reason: Some(anthropic::StopReason::ToolUse),
            stop_sequence: None,
            usage: anthropic::Usage {
                input_tokens: 12,
                output_tokens: 6,
                ..Default::default()
            },
        };

        let converted = super::convert_anthropic_response(&profile, response)
            .expect("conversion should succeed");

        assert_eq!(
            converted.choices[0].finish_reason,
            Some(async_openai::types::FinishReason::ToolCalls)
        );
        assert_eq!(
            converted.choices[0]
                .message
                .tool_calls
                .as_ref()
                .expect("tool calls")[0]
                .function
                .name,
            "lookup_weather"
        );
        assert_eq!(converted.usage.as_ref().expect("usage").prompt_tokens, 12);
        assert_eq!(
            converted.usage.as_ref().expect("usage").completion_tokens,
            6
        );
    }

    #[test]
    fn anthropic_response_profile_maps_text_blocks_to_assistant_content() {
        let profile = default_non_stream_profile(&InferenceProvider::Anthropic);
        let response = anthropic::CreateMessageResponse {
            id: "msg_text_1".to_string(),
            type_: "message".to_string(),
            role: anthropic::Role::Assistant,
            content: vec![anthropic::ContentBlock::Text {
                text: "Hello from Anthropic.".to_string(),
            }],
            model: "claude-sonnet-4-0".to_string(),
            stop_reason: Some(anthropic::StopReason::EndTurn),
            stop_sequence: None,
            usage: anthropic::Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            },
        };

        let converted = super::convert_anthropic_response(&profile, response)
            .expect("conversion should succeed");

        assert_eq!(
            converted.choices[0].message.content.as_deref(),
            Some("Hello from Anthropic.")
        );
        assert_eq!(
            converted.choices[0].finish_reason,
            Some(async_openai::types::FinishReason::Stop)
        );
    }

    #[test]
    fn anthropic_response_profile_rejects_non_anthropic_profile() {
        let profile = default_non_stream_profile(&InferenceProvider::Bedrock);
        let response = anthropic::CreateMessageResponse {
            id: "msg_wrong_profile".to_string(),
            type_: "message".to_string(),
            role: anthropic::Role::Assistant,
            content: vec![anthropic::ContentBlock::Text {
                text: "Hello from Anthropic.".to_string(),
            }],
            model: "claude-sonnet-4-0".to_string(),
            stop_reason: Some(anthropic::StopReason::EndTurn),
            stop_sequence: None,
            usage: anthropic::Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            },
        };

        let err = super::convert_anthropic_response(&profile, response)
            .expect_err("non-anthropic profile should fail");

        assert!(matches!(
            err,
            crate::error::mapper::MapperError::ProviderNotSupported(_)
        ));
    }

    #[test]
    fn anthropic_response_profile_rejects_invalid_tool_call_mapping_mode() {
        let mut profile =
            default_non_stream_profile(&InferenceProvider::Anthropic);
        profile.response.tool_call_mapping_mode = ToolCallMappingMode::Native;
        let response = anthropic::CreateMessageResponse {
            id: "msg_invalid_mode".to_string(),
            type_: "message".to_string(),
            role: anthropic::Role::Assistant,
            content: vec![anthropic::ContentBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "lookup_weather".to_string(),
                input: serde_json::json!({
                    "city": "Paris"
                }),
            }],
            model: "claude-sonnet-4-0".to_string(),
            stop_reason: Some(anthropic::StopReason::ToolUse),
            stop_sequence: None,
            usage: anthropic::Usage {
                input_tokens: 12,
                output_tokens: 6,
                ..Default::default()
            },
        };

        let err = super::convert_anthropic_response(&profile, response)
            .expect_err("invalid tool_call mapping mode should fail");

        assert!(matches!(
            err,
            crate::error::mapper::MapperError::InvalidRequest
        ));
    }

    #[test]
    fn bedrock_response_profile_maps_tool_use_and_finish_reason() {
        let profile = default_non_stream_profile(&InferenceProvider::Bedrock);
        let response = ConverseResponse {
            output: Some(ConverseResponseOutput {
                message: ConverseMessage {
                    content: vec![ConverseContentBlock {
                        text: None,
                        tool_use: Some(ConverseToolUseBlock {
                            tool_use_id: "toolu_bedrock_1".to_string(),
                            name: "lookup_weather".to_string(),
                            input: serde_json::json!({
                                "city": "Paris"
                            }),
                        }),
                        tool_result: None,
                        reasoning_content: None,
                        guard_content: None,
                    }],
                },
            }),
            stop_reason: "tool_use".to_string(),
            usage: Some(ConverseTokenUsage {
                input_tokens: 12,
                output_tokens: 6,
                total_tokens: 18,
                cache_read_input_tokens: None,
            }),
            trace: Some(ConverseTrace {
                prompt_router: Some(PromptRouterTrace {
                    invoked_model_id: Some(
                        "anthropic.claude-3-5-sonnet-20240620-v1:0".to_string(),
                    ),
                }),
            }),
        };

        let converted = super::convert_bedrock_response(&profile, response)
            .expect("conversion should succeed");

        assert_eq!(
            converted.choices[0].finish_reason,
            Some(async_openai::types::FinishReason::ToolCalls)
        );
        assert_eq!(
            converted.choices[0]
                .message
                .tool_calls
                .as_ref()
                .expect("tool calls")[0]
                .function
                .name,
            "lookup_weather"
        );
        assert_eq!(converted.usage.as_ref().expect("usage").prompt_tokens, 12);
        assert_eq!(
            converted.usage.as_ref().expect("usage").completion_tokens,
            6
        );
    }

    #[test]
    fn bedrock_response_profile_rejects_non_bedrock_profile() {
        let profile = default_non_stream_profile(&InferenceProvider::Anthropic);
        let response = ConverseResponse {
            output: Some(ConverseResponseOutput {
                message: ConverseMessage {
                    content: vec![ConverseContentBlock {
                        text: Some("Hello from Bedrock.".to_string()),
                        tool_use: None,
                        tool_result: None,
                        reasoning_content: None,
                        guard_content: None,
                    }],
                },
            }),
            stop_reason: "end_turn".to_string(),
            usage: Some(ConverseTokenUsage {
                input_tokens: 10,
                output_tokens: 4,
                total_tokens: 14,
                cache_read_input_tokens: None,
            }),
            trace: None,
        };

        let err = super::convert_bedrock_response(&profile, response)
            .expect_err("non-bedrock profile should fail");

        assert!(matches!(
            err,
            crate::error::mapper::MapperError::ProviderNotSupported(_)
        ));
    }
}
