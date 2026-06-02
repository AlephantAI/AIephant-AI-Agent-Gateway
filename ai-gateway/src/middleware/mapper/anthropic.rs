use std::{collections::HashMap, str::FromStr};

use http::response::Parts;

use super::{TryConvert, TryConvertStreamData};
use crate::{
    endpoints::openai::chat_completions::system_prompt,
    error::mapper::MapperError,
    middleware::mapper::{
        DEFAULT_MAX_TOKENS, ResponseBodyConverter, TryConvertError,
        capabilities::ProviderCapabilities,
        families::ProviderProtocolFamily,
        mime_from_data_uri,
        model::ModelMapper,
        non_stream_profile::NonStreamFormatProfile,
        non_stream_profile_data::default_non_stream_profile,
        rules::ProviderRuleSet,
        stream_normalizer::{
            build_finish_choice, build_reasoning_choice, build_stream_response, build_stream_usage,
            build_text_choice, build_tool_call_chunk, build_tool_choice,
        },
    },
    types::{
        model_id::{ModelId, Version},
        provider::InferenceProvider,
    },
};

pub const OPENAI_CHAT_COMPLETION_OBJECT: &str = "chat.completion";

pub struct AnthropicConverter {
    non_stream_profile: NonStreamFormatProfile,
    model_mapper: ModelMapper,
}

impl AnthropicConverter {
    fn derived_profile_from_metadata(
        provider: &InferenceProvider,
        rules: &ProviderRuleSet,
    ) -> NonStreamFormatProfile {
        let mut non_stream_profile = default_non_stream_profile(provider);
        non_stream_profile.family = rules.family;
        non_stream_profile.request.system_handling = rules.request.system_handling;
        non_stream_profile.request.tool_choice_mode = rules.request.tool_choice_mode;
        non_stream_profile.request.response_format_mode = rules.request.response_format_mode;
        non_stream_profile.request.reasoning_mode = rules.request.reasoning_mode;
        non_stream_profile.request.multimodal_mode = rules.request.multimodal_mode;
        non_stream_profile
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn new(model_mapper: ModelMapper) -> Self {
        let capabilities = ProviderCapabilities::for_provider(&InferenceProvider::Anthropic);
        let rules = super::rule_data::default_provider_rules(&InferenceProvider::Anthropic);
        Self::new_with_metadata(capabilities, rules, model_mapper)
    }

    #[must_use]
    pub fn new_with_profile(
        non_stream_profile: NonStreamFormatProfile,
        model_mapper: ModelMapper,
    ) -> Self {
        Self {
            non_stream_profile,
            model_mapper,
        }
    }

    #[must_use]
    pub fn new_with_metadata(
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Self {
        Self::try_new_with_metadata(capabilities, rules, model_mapper)
            .expect("anthropic converter metadata must be valid")
    }

    pub fn try_new_with_metadata(
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Result<Self, MapperError> {
        if capabilities.provider != InferenceProvider::Anthropic
            || rules.provider != InferenceProvider::Anthropic
            || rules.family != ProviderProtocolFamily::AnthropicMessages
            || !rules.family.matches_provider(&capabilities.provider)
        {
            return Err(MapperError::ProviderNotSupported(
                capabilities.provider.to_string(),
            ));
        }

        let non_stream_profile =
            Self::derived_profile_from_metadata(&capabilities.provider, &rules);
        Ok(Self::new_with_profile(non_stream_profile, model_mapper))
    }
}

fn map_reasoning_effort_to_thinking(
    reasoning_effort: Option<&async_openai::types::ReasoningEffort>,
    max_tokens: u32,
) -> Option<anthropic_ai_sdk::types::message::Thinking> {
    use anthropic_ai_sdk::types::message::{Thinking, ThinkingType};
    use async_openai::types::ReasoningEffort;

    if max_tokens < 1024 {
        return None;
    }

    let budget_tokens = match reasoning_effort? {
        ReasoningEffort::None => return None,
        ReasoningEffort::Low => 1024,
        ReasoningEffort::Medium => usize::max(1024, (max_tokens as usize * 2) / 3),
        ReasoningEffort::High | ReasoningEffort::XHigh => max_tokens as usize,
    };

    Some(Thinking {
        budget_tokens,
        type_: ThinkingType::Enabled,
    })
}

impl
    TryConvert<
        async_openai::types::CreateChatCompletionRequest,
        anthropic_ai_sdk::types::message::CreateMessageParams,
    > for AnthropicConverter
{
    type Error = MapperError;

    #[allow(clippy::too_many_lines)]
    fn try_convert(
        &self,
        value: async_openai::types::CreateChatCompletionRequest,
    ) -> std::result::Result<anthropic_ai_sdk::types::message::CreateMessageParams, Self::Error>
    {
        use anthropic_ai_sdk::types::message as anthropic;
        use async_openai::types as openai;
        let source_model = ModelId::from_str(&value.model)?;
        let mut target_model = self
            .model_mapper
            .map_model(&source_model, &InferenceProvider::Anthropic)?;
        tracing::trace!(source_model = ?source_model, target_model = ?target_model, "mapped model");

        // for claude 3-x, anthropic required an explicit `-latest` suffix for
        // aliases but for claude 4-x, the aliases must be implicit, ie they
        // must not have the `-latest` suffix

        if let ModelId::ModelIdWithVersion {
            provider: InferenceProvider::Anthropic,
            id: model,
        } = &mut target_model
        {
            if model.model.contains("claude-3") {
                model.version = Version::Latest;
            } else {
                model.version = Version::ImplicitLatest;
            }
        }

        let system_prompt = system_prompt(&value);
        #[allow(deprecated)]
        let max_tokens = value
            .max_completion_tokens
            .unwrap_or_else(|| value.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS));
        let temperature = value.temperature;
        let stop_sequences = match value.stop {
            Some(openai::Stop::String(stop)) => Some(vec![stop]),
            Some(openai::Stop::StringArray(stops)) => Some(stops),
            None => None,
        };
        let stream = value.stream;
        let top_p = value.top_p;
        let tools = if let Some(tools) = value.tools {
            let mapped_tools: Vec<_> = tools
                .iter()
                .filter_map(|tool| {
                    let func = tool.function.as_ref()?;
                    Some(anthropic::Tool {
                        name: func.name.clone(),
                        description: func.description.clone(),
                        input_schema: func.parameters.clone().unwrap_or_default(),
                    })
                })
                .collect();
            if mapped_tools.is_empty() {
                None
            } else {
                Some(mapped_tools)
            }
        } else {
            None
        };
        let metadata = value.user.map(|user| anthropic::Metadata {
            fields: HashMap::from([("user_id".to_string(), user)]),
        });
        let thinking =
            map_reasoning_effort_to_thinking(value.reasoning_effort.as_ref(), max_tokens);

        let tool_choice = match value.tool_choice {
            Some(openai::ChatCompletionToolChoiceOption::Named(tool)) => {
                Some(anthropic::ToolChoice::Tool {
                    name: tool.function.name,
                })
            }
            Some(openai::ChatCompletionToolChoiceOption::Auto) => Some(anthropic::ToolChoice::Auto),
            Some(openai::ChatCompletionToolChoiceOption::Required) => {
                Some(anthropic::ToolChoice::Any)
            }
            Some(openai::ChatCompletionToolChoiceOption::None) => Some(anthropic::ToolChoice::None),
            None => None,
        };

        let mut mapped_messages = Vec::with_capacity(value.messages.len());
        for message in value.messages {
            match message {
                // we've already set the system prompt
                openai::ChatCompletionRequestMessage::Developer(_)
                | openai::ChatCompletionRequestMessage::System(_) => {}
                openai::ChatCompletionRequestMessage::User(message) => {
                    let mapped_content = match message.content {
                        openai::ChatCompletionRequestUserMessageContent::Text(content) => {
                            anthropic::MessageContent::Text { content }
                        }
                        openai::ChatCompletionRequestUserMessageContent::Array(content) => {
                            let mapped_content_blocks = content.into_iter().filter_map(|part| {
                                match part {
                                    openai::ChatCompletionRequestUserMessageContentPart::Text(text) => {
                                        Some(anthropic::ContentBlock::Text { text: text.text })
                                    },
                                    openai::ChatCompletionRequestUserMessageContentPart::ImageUrl(image) => {
                                        let mapped_image = if image.image_url.url.starts_with("http") {
                                            anthropic::ImageSource {
                                                type_: "url".to_string(),
                                                media_type: String::new(),
                                                data: image.image_url.url,
                                            }
                                        } else {
                                            let mime = mime_from_data_uri(&image.image_url.url)?;
                                            let (_, b64) = image.image_url.url.split_once(',')?;
                                            anthropic::ImageSource {
                                                type_: "base64".to_string(),
                                                media_type: mime.mime_type().to_string(),
                                                data: b64.to_string(),
                                            }
                                        };
                                        Some(anthropic::ContentBlock::Image { source: mapped_image })
                                    },
                                    openai::ChatCompletionRequestUserMessageContentPart::InputAudio(_audio) => {                                         // Anthropic API does not support audio
                                        // Anthropic does not support audio
                                        None
                                    },
                                }
                            }).collect();
                            anthropic::MessageContent::Blocks {
                                content: mapped_content_blocks,
                            }
                        }
                    };
                    let mapped_message = anthropic::Message {
                        role: anthropic::Role::User,
                        content: mapped_content,
                    };
                    mapped_messages.push(mapped_message);
                }
                openai::ChatCompletionRequestMessage::Assistant(message) => {
                    let mut content_blocks = Vec::new();

                    // Handle text content
                    match message.content {
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Text(
                            content,
                        )) => {
                            if !content.is_empty() {
                                content_blocks
                                    .push(anthropic::ContentBlock::Text { text: content });
                            }
                        }
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Array(
                            content,
                        )) => {
                            for part in content {
                                match part {
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Text(text) => {
                                        content_blocks.push(anthropic::ContentBlock::Text { text: text.text });
                                    },
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Refusal(text) => {
                                        content_blocks.push(anthropic::ContentBlock::Text { text: text.refusal.clone() });
                                    },
                                }
                            }
                        }
                        None => {} // No content, but we might have tool_calls
                    }

                    // Handle tool calls
                    if let Some(tool_calls) = message.tool_calls {
                        for tool_call in tool_calls {
                            let input = if tool_call.function.arguments.is_empty() {
                                serde_json::Value::Object(serde_json::Map::new())
                            } else {
                                serde_json::from_str(&tool_call.function.arguments).unwrap_or_else(
                                    |_| serde_json::Value::Object(serde_json::Map::new()),
                                )
                            };

                            content_blocks.push(anthropic::ContentBlock::ToolUse {
                                id: tool_call.id,
                                name: tool_call.function.name,
                                input,
                            });
                        }
                    }

                    // Only create message if we have some content
                    if !content_blocks.is_empty() {
                        let mapped_content = anthropic::MessageContent::Blocks {
                            content: content_blocks,
                        };
                        let mapped_message = anthropic::Message {
                            role: anthropic::Role::Assistant,
                            content: mapped_content,
                        };
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Tool(message) => {
                    tracing::info!(message = ?message, "tool message");
                    let mapped_content = match message.content {
                        openai::ChatCompletionRequestToolMessageContent::Text(content) => {
                            let block = anthropic::ContentBlock::ToolResult {
                                tool_use_id: message.tool_call_id,
                                content,
                            };
                            anthropic::MessageContent::Blocks {
                                content: vec![block],
                            }
                        }
                        openai::ChatCompletionRequestToolMessageContent::Array(content) => {
                            let mapped_content_blocks = content
                                .into_iter()
                                .map(|part| match part {
                                    openai::ChatCompletionRequestToolMessageContentPart::Text(
                                        text,
                                    ) => anthropic::ContentBlock::ToolResult {
                                        tool_use_id: message.tool_call_id.clone(),
                                        content: text.text,
                                    },
                                })
                                .collect();
                            anthropic::MessageContent::Blocks {
                                content: mapped_content_blocks,
                            }
                        }
                    };
                    let mapped_message = anthropic::Message {
                        role: anthropic::Role::User,
                        content: mapped_content,
                    };
                    mapped_messages.push(mapped_message);
                }
                openai::ChatCompletionRequestMessage::Function(message) => {
                    let Some(tool) = tools
                        .as_ref()
                        .and_then(|tools| tools.iter().find(|tool| tool.name == message.name))
                    else {
                        continue;
                    };
                    let mapped_content = anthropic::MessageContent::Blocks {
                        content: vec![anthropic::ContentBlock::ToolUse {
                            id: message.name.clone(),
                            name: tool.name.clone(),
                            input: tool.input_schema.clone(),
                        }],
                    };
                    let mapped_message = anthropic::Message {
                        role: anthropic::Role::Assistant,
                        content: mapped_content,
                    };
                    mapped_messages.push(mapped_message);
                }
            }
        }

        Ok(anthropic::CreateMessageParams {
            max_tokens,
            messages: mapped_messages,
            model: target_model.to_string(),
            system: system_prompt,
            temperature,
            stop_sequences,
            stream,
            top_k: None,
            top_p,
            tools,
            tool_choice,
            metadata,
            thinking,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn try_convert_model_passthrough(
        &self,
        value: async_openai::types::CreateChatCompletionRequest,
    ) -> std::result::Result<anthropic_ai_sdk::types::message::CreateMessageParams, Self::Error>
    {
        use anthropic_ai_sdk::types::message as anthropic;
        use async_openai::types as openai;

        let target_model = value.model.clone();
        let system_prompt = system_prompt(&value);
        #[allow(deprecated)]
        let max_tokens = value
            .max_completion_tokens
            .unwrap_or_else(|| value.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS));
        let temperature = value.temperature;
        let stop_sequences = match value.stop {
            Some(openai::Stop::String(stop)) => Some(vec![stop]),
            Some(openai::Stop::StringArray(stops)) => Some(stops),
            None => None,
        };
        let stream = value.stream;
        let top_p = value.top_p;
        let tools = if let Some(tools) = value.tools {
            let mapped_tools: Vec<_> = tools
                .iter()
                .filter_map(|tool| {
                    let func = tool.function.as_ref()?;
                    Some(anthropic::Tool {
                        name: func.name.clone(),
                        description: func.description.clone(),
                        input_schema: func.parameters.clone().unwrap_or_default(),
                    })
                })
                .collect();
            if mapped_tools.is_empty() {
                None
            } else {
                Some(mapped_tools)
            }
        } else {
            None
        };
        let metadata = value.user.map(|user| anthropic::Metadata {
            fields: HashMap::from([("user_id".to_string(), user)]),
        });
        let thinking =
            map_reasoning_effort_to_thinking(value.reasoning_effort.as_ref(), max_tokens);

        let tool_choice = match value.tool_choice {
            Some(openai::ChatCompletionToolChoiceOption::Named(tool)) => {
                Some(anthropic::ToolChoice::Tool {
                    name: tool.function.name,
                })
            }
            Some(openai::ChatCompletionToolChoiceOption::Auto) => Some(anthropic::ToolChoice::Auto),
            Some(openai::ChatCompletionToolChoiceOption::Required) => {
                Some(anthropic::ToolChoice::Any)
            }
            Some(openai::ChatCompletionToolChoiceOption::None) => Some(anthropic::ToolChoice::None),
            None => None,
        };

        let mut mapped_messages = Vec::with_capacity(value.messages.len());
        for message in value.messages {
            match message {
                openai::ChatCompletionRequestMessage::Developer(_)
                | openai::ChatCompletionRequestMessage::System(_) => {}
                openai::ChatCompletionRequestMessage::User(message) => {
                    let mapped_content = match message.content {
                        openai::ChatCompletionRequestUserMessageContent::Text(content) => {
                            anthropic::MessageContent::Text { content }
                        }
                        openai::ChatCompletionRequestUserMessageContent::Array(content) => {
                            let mapped_content_blocks = content.into_iter().filter_map(|part| {
                                match part {
                                    openai::ChatCompletionRequestUserMessageContentPart::Text(text) => {
                                        Some(anthropic::ContentBlock::Text { text: text.text })
                                    }
                                    openai::ChatCompletionRequestUserMessageContentPart::ImageUrl(image) => {
                                        let mapped_image = if image.image_url.url.starts_with("http") {
                                            anthropic::ImageSource {
                                                type_: "url".to_string(),
                                                media_type: String::new(),
                                                data: image.image_url.url,
                                            }
                                        } else {
                                            let mime = mime_from_data_uri(&image.image_url.url)?;
                                            let (_, b64) = image.image_url.url.split_once(',')?;
                                            anthropic::ImageSource {
                                                type_: "base64".to_string(),
                                                media_type: mime.mime_type().to_string(),
                                                data: b64.to_string(),
                                            }
                                        };
                                        Some(anthropic::ContentBlock::Image { source: mapped_image })
                                    }
                                    openai::ChatCompletionRequestUserMessageContentPart::InputAudio(_) => None,
                                }
                            }).collect();
                            anthropic::MessageContent::Blocks {
                                content: mapped_content_blocks,
                            }
                        }
                    };
                    mapped_messages.push(anthropic::Message {
                        role: anthropic::Role::User,
                        content: mapped_content,
                    });
                }
                openai::ChatCompletionRequestMessage::Assistant(message) => {
                    let mut content_blocks = Vec::new();
                    match message.content {
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Text(
                            content,
                        )) => {
                            if !content.is_empty() {
                                content_blocks
                                    .push(anthropic::ContentBlock::Text { text: content });
                            }
                        }
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Array(
                            content,
                        )) => {
                            for part in content {
                                match part {
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Text(text) => {
                                        content_blocks.push(anthropic::ContentBlock::Text { text: text.text });
                                    }
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Refusal(text) => {
                                        content_blocks.push(anthropic::ContentBlock::Text { text: text.refusal.clone() });
                                    }
                                }
                            }
                        }
                        None => {}
                    }
                    if let Some(tool_calls) = message.tool_calls {
                        for tool_call in tool_calls {
                            let input = if tool_call.function.arguments.is_empty() {
                                serde_json::Value::Object(serde_json::Map::new())
                            } else {
                                serde_json::from_str(&tool_call.function.arguments).unwrap_or_else(
                                    |_| serde_json::Value::Object(serde_json::Map::new()),
                                )
                            };

                            content_blocks.push(anthropic::ContentBlock::ToolUse {
                                id: tool_call.id,
                                name: tool_call.function.name,
                                input,
                            });
                        }
                    }
                    if !content_blocks.is_empty() {
                        mapped_messages.push(anthropic::Message {
                            role: anthropic::Role::Assistant,
                            content: anthropic::MessageContent::Blocks {
                                content: content_blocks,
                            },
                        });
                    }
                }
                openai::ChatCompletionRequestMessage::Tool(message) => {
                    let mapped_content = match message.content {
                        openai::ChatCompletionRequestToolMessageContent::Text(content) => {
                            anthropic::MessageContent::Blocks {
                                content: vec![anthropic::ContentBlock::ToolResult {
                                    tool_use_id: message.tool_call_id,
                                    content,
                                }],
                            }
                        }
                        openai::ChatCompletionRequestToolMessageContent::Array(content) => {
                            let mapped_content_blocks = content
                                .into_iter()
                                .map(|part| match part {
                                    openai::ChatCompletionRequestToolMessageContentPart::Text(
                                        text,
                                    ) => anthropic::ContentBlock::ToolResult {
                                        tool_use_id: message.tool_call_id.clone(),
                                        content: text.text,
                                    },
                                })
                                .collect();
                            anthropic::MessageContent::Blocks {
                                content: mapped_content_blocks,
                            }
                        }
                    };
                    mapped_messages.push(anthropic::Message {
                        role: anthropic::Role::User,
                        content: mapped_content,
                    });
                }
                openai::ChatCompletionRequestMessage::Function(message) => {
                    let Some(tool) = tools
                        .as_ref()
                        .and_then(|tools| tools.iter().find(|tool| tool.name == message.name))
                    else {
                        continue;
                    };
                    mapped_messages.push(anthropic::Message {
                        role: anthropic::Role::Assistant,
                        content: anthropic::MessageContent::Blocks {
                            content: vec![anthropic::ContentBlock::ToolUse {
                                id: message.name.clone(),
                                name: tool.name.clone(),
                                input: tool.input_schema.clone(),
                            }],
                        },
                    });
                }
            }
        }

        Ok(anthropic::CreateMessageParams {
            max_tokens,
            messages: mapped_messages,
            model: target_model,
            system: system_prompt,
            temperature,
            stop_sequences,
            stream,
            top_k: None,
            top_p,
            tools,
            tool_choice,
            metadata,
            thinking,
        })
    }
}

impl
    TryConvert<
        anthropic_ai_sdk::types::message::CreateMessageResponse,
        async_openai::types::CreateChatCompletionResponse,
    > for AnthropicConverter
{
    type Error = MapperError;

    #[allow(clippy::too_many_lines)]
    fn try_convert(
        &self,
        value: anthropic_ai_sdk::types::message::CreateMessageResponse,
    ) -> std::result::Result<async_openai::types::CreateChatCompletionResponse, Self::Error> {
        super::non_stream_response_interpreter::convert_anthropic_response(
            &self.non_stream_profile,
            value,
        )
    }
}

impl
    ResponseBodyConverter<
        anthropic_ai_sdk::types::message::CreateMessageResponse,
        async_openai::types::CreateChatCompletionResponse,
    > for AnthropicConverter
{
    fn try_convert_response(
        &self,
        resp_parts: &Parts,
        value: anthropic_ai_sdk::types::message::CreateMessageResponse,
    ) -> std::result::Result<async_openai::types::CreateChatCompletionResponse, Self::Error> {
        super::non_stream_response_interpreter::convert_anthropic_response_from_parts(
            resp_parts,
            &self.non_stream_profile,
            value,
        )
    }
}

impl
    TryConvertStreamData<
        anthropic_ai_sdk::types::message::StreamEvent,
        async_openai::types::CreateChatCompletionStreamResponse,
    > for AnthropicConverter
{
    type Error = MapperError;

    #[allow(clippy::too_many_lines)]
    fn try_convert_chunk(
        &self,
        value: anthropic_ai_sdk::types::message::StreamEvent,
        anthropic_openai_usage: Option<&crate::types::extensions::AnthropicOpenAiUsageCell>,
    ) -> std::result::Result<
        Option<async_openai::types::CreateChatCompletionStreamResponse>,
        Self::Error,
    > {
        use anthropic_ai_sdk::types::message as anthropic;
        use async_openai::types as openai;

        // TODO: These placeholder values for id, model, and created should be
        // replaced by actual values from the MessageStart event,
        // propagated by the stream handling logic.
        const PLACEHOLDER_STREAM_ID: &str = "anthropic-stream-id";
        const PLACEHOLDER_MODEL_NAME: &str = "anthropic-model";
        #[allow(deprecated)]
        match value {
            anthropic::StreamEvent::MessageStart { ref message } => {
                if let Some(cell) = anthropic_openai_usage {
                    cell.lock()
                        .expect("anthropic stream usage lock")
                        .on_message_start(message);
                }
                let mut current_text_content = String::new();
                let mut tool_calls = Vec::new();

                for (idx, content_block) in message.content.iter().enumerate() {
                    match content_block {
                        anthropic::ContentBlock::Text { text, .. } => {
                            current_text_content.push_str(text);
                        }
                        anthropic::ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(build_tool_call_chunk(
                                u32::try_from(idx).unwrap_or(0),
                                Some(id.clone()),
                                Some(name.clone()),
                                Some(
                                    serde_json::to_string(input)
                                        .map_err(MapperError::SerdeError)?,
                                ),
                            ));
                        }
                        anthropic::ContentBlock::ToolResult {
                            tool_use_id: _,
                            content,
                        } => {
                            current_text_content.push('\n');
                            current_text_content.push_str(content);
                        }
                        _ => {}
                    }
                }

                let finish_reason = match message.stop_reason {
                    Some(anthropic::StopReason::EndTurn | anthropic::StopReason::StopSequence) => {
                        Some(openai::FinishReason::Stop)
                    }
                    Some(anthropic::StopReason::MaxTokens) => Some(openai::FinishReason::Length),
                    Some(anthropic::StopReason::ToolUse) => Some(openai::FinishReason::ToolCalls),
                    Some(anthropic::StopReason::Refusal) => {
                        Some(openai::FinishReason::ContentFilter)
                    }
                    None => None,
                };

                let refusal_content =
                    if matches!(message.stop_reason, Some(anthropic::StopReason::Refusal)) {
                        message.stop_sequence.clone() // stop_sequence is Option<String>
                    } else {
                        None
                    };

                let choice = openai::ChatChoiceStream {
                    index: 0,
                    delta: openai::ChatCompletionStreamResponseDelta {
                        role: Some(match message.role {
                            anthropic::Role::User => openai::Role::User,
                            anthropic::Role::Assistant => openai::Role::Assistant,
                        }),
                        content: Some(current_text_content),
                        tool_calls: Some(tool_calls),
                        refusal: refusal_content,
                        function_call: None,
                        reasoning_content: None,
                    },
                    finish_reason,
                    logprobs: None,
                };
                Ok(Some(build_stream_response(
                    message.id.clone(),
                    message.model.clone(),
                    vec![choice],
                    None,
                )))
            }
            anthropic::StreamEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                match content_block {
                    anthropic::ContentBlock::ToolUse { id, name, input } => {
                        let tool_call_chunk = build_tool_call_chunk(
                            u32::try_from(index).unwrap_or(0),
                            Some(id),
                            Some(name),
                            Some(serde_json::to_string(&input).map_err(MapperError::SerdeError)?),
                        );
                        let choice = build_tool_choice(0, tool_call_chunk);
                        Ok(Some(build_stream_response(
                            PLACEHOLDER_STREAM_ID.to_string(),
                            PLACEHOLDER_MODEL_NAME.to_string(),
                            vec![choice],
                            None,
                        )))
                    }
                    _ => Ok(None), // Text start, etc., content comes in delta
                }
            }
            anthropic::StreamEvent::ContentBlockDelta { index, delta } => match delta {
                anthropic::ContentBlockDelta::TextDelta { text } => {
                    let choice = build_text_choice(u32::try_from(index).unwrap_or(0), text);
                    Ok(Some(build_stream_response(
                        PLACEHOLDER_STREAM_ID.to_string(),
                        PLACEHOLDER_MODEL_NAME.to_string(),
                        vec![choice],
                        None,
                    )))
                }
                anthropic::ContentBlockDelta::InputJsonDelta { partial_json } => {
                    let tool_call_chunk = build_tool_call_chunk(
                        u32::try_from(index).unwrap_or(0),
                        None,
                        None,
                        Some(partial_json),
                    );
                    let choice =
                        build_tool_choice(u32::try_from(index).unwrap_or(0), tool_call_chunk);
                    Ok(Some(build_stream_response(
                        PLACEHOLDER_STREAM_ID.to_string(),
                        PLACEHOLDER_MODEL_NAME.to_string(),
                        vec![choice],
                        None,
                    )))
                }
                anthropic::ContentBlockDelta::ThinkingDelta { thinking } => {
                    let choice =
                        build_reasoning_choice(u32::try_from(index).unwrap_or(0), thinking);
                    Ok(Some(build_stream_response(
                        PLACEHOLDER_STREAM_ID.to_string(),
                        PLACEHOLDER_MODEL_NAME.to_string(),
                        vec![choice],
                        None,
                    )))
                }
                anthropic::ContentBlockDelta::SignatureDelta { .. } => Ok(None),
            },
            anthropic::StreamEvent::ContentBlockStop { index: _ }
            | anthropic::StreamEvent::Ping => Ok(None), /* Usually no */
            // separate OpenAI
            // chunk for this
            anthropic::StreamEvent::MessageStop => {
                let (id, model, usage_opt) = match anthropic_openai_usage {
                    Some(cell) => {
                        let g = cell.lock().expect("anthropic stream usage lock");
                        let id = if g.stream_message_id.is_empty() {
                            PLACEHOLDER_STREAM_ID.to_string()
                        } else {
                            g.stream_message_id.clone()
                        };
                        let model = if g.stream_model.is_empty() {
                            PLACEHOLDER_MODEL_NAME.to_string()
                        } else {
                            g.stream_model.clone()
                        };
                        let u = g.build_openai_completion_usage();
                        (id, model, Some(u))
                    }
                    None => (
                        PLACEHOLDER_STREAM_ID.to_string(),
                        PLACEHOLDER_MODEL_NAME.to_string(),
                        None,
                    ),
                };
                Ok(Some(build_stream_response(id, model, vec![], usage_opt)))
            }
            anthropic::StreamEvent::MessageDelta { delta, usage } => {
                if let Some(cell) = anthropic_openai_usage {
                    cell.lock()
                        .expect("anthropic stream usage lock")
                        .on_message_delta(&usage);
                }
                let finish_reason = match delta.stop_reason {
                    Some(anthropic::StopReason::EndTurn | anthropic::StopReason::StopSequence) => {
                        Some(openai::FinishReason::Stop)
                    }
                    Some(anthropic::StopReason::MaxTokens) => Some(openai::FinishReason::Length),
                    Some(anthropic::StopReason::ToolUse) => Some(openai::FinishReason::ToolCalls),
                    Some(anthropic::StopReason::Refusal) => {
                        Some(openai::FinishReason::ContentFilter)
                    }
                    None => None,
                };

                let completion_usage = build_stream_usage(
                    usage.as_ref().map_or(0, |u| u.input_tokens),
                    usage.as_ref().map_or(0, |u| u.output_tokens),
                );

                let choice = build_finish_choice(
                    finish_reason,
                    completion_usage.clone(),
                    delta.stop_sequence,
                );
                Ok(Some(build_stream_response(
                    PLACEHOLDER_STREAM_ID.to_string(),
                    PLACEHOLDER_MODEL_NAME.to_string(),
                    vec![choice],
                    None,
                )))
            }
            anthropic::StreamEvent::Error { error } => {
                tracing::warn!(error = ?error, "error in stream event");
                Ok(None)
            }
        }
    }
}

impl
    TryConvert<
        anthropic_ai_sdk::types::message::CreateMessageParams,
        anthropic_ai_sdk::types::message::CreateMessageParams,
    > for AnthropicConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        mut value: anthropic_ai_sdk::types::message::CreateMessageParams,
    ) -> Result<anthropic_ai_sdk::types::message::CreateMessageParams, Self::Error> {
        let source_model = ModelId::from_str(&value.model)?;
        let target_model = self
            .model_mapper
            .map_model(&source_model, &InferenceProvider::Anthropic)?;
        tracing::trace!(source_model = ?source_model, target_model = ?target_model, "mapped model");

        value.model = target_model.to_string();

        Ok(value)
    }
}

impl
    TryConvert<
        anthropic_ai_sdk::types::message::CreateMessageResponse,
        anthropic_ai_sdk::types::message::CreateMessageResponse,
    > for AnthropicConverter
{
    type Error = MapperError;
    fn try_convert(
        &self,
        value: anthropic_ai_sdk::types::message::CreateMessageResponse,
    ) -> Result<anthropic_ai_sdk::types::message::CreateMessageResponse, Self::Error> {
        Ok(value)
    }
}

impl
    ResponseBodyConverter<
        anthropic_ai_sdk::types::message::CreateMessageResponse,
        anthropic_ai_sdk::types::message::CreateMessageResponse,
    > for AnthropicConverter
{
}

impl
    TryConvertError<
        crate::endpoints::anthropic::messages::AnthropicApiError,
        crate::endpoints::anthropic::messages::AnthropicApiError,
    > for AnthropicConverter
{
    type Error = MapperError;
    fn try_convert_error(
        &self,
        _resp_parts: &Parts,
        value: crate::endpoints::anthropic::messages::AnthropicApiError,
    ) -> Result<crate::endpoints::anthropic::messages::AnthropicApiError, Self::Error> {
        Ok(value)
    }
}

impl
    TryConvertStreamData<
        anthropic_ai_sdk::types::message::StreamEvent,
        anthropic_ai_sdk::types::message::StreamEvent,
    > for AnthropicConverter
{
    type Error = MapperError;

    fn try_convert_chunk(
        &self,
        value: anthropic_ai_sdk::types::message::StreamEvent,
        _anthropic_openai_usage: Option<&crate::types::extensions::AnthropicOpenAiUsageCell>,
    ) -> Result<Option<anthropic_ai_sdk::types::message::StreamEvent>, Self::Error> {
        Ok(Some(value))
    }
}

impl
    TryConvertError<
        crate::endpoints::anthropic::messages::AnthropicApiError,
        async_openai::error::WrappedError,
    > for AnthropicConverter
{
    type Error = MapperError;
    fn try_convert_error(
        &self,
        resp_parts: &Parts,
        value: crate::endpoints::anthropic::messages::AnthropicApiError,
    ) -> Result<async_openai::error::WrappedError, Self::Error> {
        let message = value.error.message;
        let error = super::openai_error_from_status(resp_parts.status, Some(message));
        Ok(error)
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::CreateChatCompletionRequest;
    use serde_json::json;

    use crate::{
        middleware::mapper::{
            TryConvertStreamData,
            capabilities::ProviderCapabilities,
            model::ModelMapper,
            non_stream_profile_data::default_non_stream_profile,
            rule_data::default_provider_rules,
            rules::{MultimodalMode, ReasoningMode, ToolChoiceMode},
        },
        types::provider::InferenceProvider,
    };

    fn sample_request() -> CreateChatCompletionRequest {
        serde_json::from_value(json!({
            "model": "anthropic/claude-sonnet-4",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ]
        }))
        .expect("request should deserialize")
    }

    fn sample_multimodal_request() -> CreateChatCompletionRequest {
        serde_json::from_value(json!({
            "model": "anthropic/claude-sonnet-4",
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
        .expect("multimodal request should deserialize")
    }

    async fn sample_model_mapper() -> ModelMapper {
        let config = crate::config::Config::default();
        let app = crate::app::build_test_app(config).await.expect("build app");
        ModelMapper::new(app.state.clone())
    }

    #[tokio::test]
    async fn anthropic_converter_does_not_reapply_non_stream_profile_to_tool_fields() {
        let mut non_stream_profile = default_non_stream_profile(&InferenceProvider::Anthropic);
        non_stream_profile.request.tool_choice_mode = ToolChoiceMode::Unsupported;
        let converter = super::AnthropicConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "anthropic/claude-sonnet-4",
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
                        "name": "lookup_weather",
                        "description": "lookup weather"
                    }
                }
            ],
            "tool_choice": "auto",
            "parallel_tool_calls": true
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert!(converted.tools.is_some());
        assert!(converted.tool_choice.is_some());
    }

    #[tokio::test]
    async fn anthropic_converter_does_not_reapply_non_stream_profile_to_reasoning_effort() {
        let mut non_stream_profile = default_non_stream_profile(&InferenceProvider::Anthropic);
        non_stream_profile.request.reasoning_mode = ReasoningMode::Unsupported;
        let converter = super::AnthropicConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let mut request = sample_request();
        request.reasoning_effort = Some(async_openai::types::ReasoningEffort::High);

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert!(converted.thinking.is_some());
    }

    #[tokio::test]
    async fn anthropic_converter_does_not_reject_multimodal_without_request_engine() {
        let mut non_stream_profile = default_non_stream_profile(&InferenceProvider::Anthropic);
        non_stream_profile.request.multimodal_mode = MultimodalMode::Unsupported;
        let converter = super::AnthropicConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_multimodal_request(),
        )
        .expect("conversion should succeed");

        assert_eq!(converted.messages.len(), 1);
    }

    #[tokio::test]
    async fn anthropic_converter_can_be_built_from_metadata_rules_without_reapplying_them() {
        let capabilities = ProviderCapabilities::for_provider(&InferenceProvider::Anthropic);
        let mut rules = default_provider_rules(&InferenceProvider::Anthropic);
        rules.request.tool_choice_mode = ToolChoiceMode::Unsupported;
        let converter = super::AnthropicConverter::new_with_metadata(
            capabilities,
            rules,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "anthropic/claude-sonnet-4",
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
                        "name": "lookup_weather",
                        "description": "lookup weather"
                    }
                }
            ],
            "tool_choice": "auto"
        }))
        .expect("request should deserialize");

        let converted = crate::middleware::mapper::TryConvert::try_convert(&converter, request)
            .expect("conversion should succeed");

        assert!(converted.tools.is_some());
        assert!(converted.tool_choice.is_some());
    }

    fn convert_stream_chunk(
        converter: &super::AnthropicConverter,
        event: anthropic_ai_sdk::types::message::StreamEvent,
    ) -> Option<async_openai::types::CreateChatCompletionStreamResponse> {
        <super::AnthropicConverter as TryConvertStreamData<
            anthropic_ai_sdk::types::message::StreamEvent,
            async_openai::types::CreateChatCompletionStreamResponse,
        >>::try_convert_chunk(converter, event, None)
        .expect("conversion should succeed")
    }

    #[tokio::test]
    async fn anthropic_stream_thinking_delta_maps_to_reasoning_content() {
        use anthropic_ai_sdk::types::message as anthropic;

        let converter = super::AnthropicConverter::new(sample_model_mapper().await);
        let event = anthropic::StreamEvent::ContentBlockDelta {
            index: 0,
            delta: anthropic::ContentBlockDelta::ThinkingDelta {
                thinking: "Let me reason step by step...".to_string(),
            },
        };

        let response = convert_stream_chunk(&converter, event)
            .expect("should produce a stream response, not None");
        assert_eq!(response.choices.len(), 1);

        let delta = &response.choices[0].delta;
        assert_eq!(
            delta.reasoning_content.as_deref(),
            Some("Let me reason step by step...")
        );
        assert!(delta.content.is_none());
    }

    #[tokio::test]
    async fn anthropic_stream_signature_delta_is_dropped() {
        use anthropic_ai_sdk::types::message as anthropic;

        let converter = super::AnthropicConverter::new(sample_model_mapper().await);
        let event = anthropic::StreamEvent::ContentBlockDelta {
            index: 0,
            delta: anthropic::ContentBlockDelta::SignatureDelta {
                signature: "sig-abc123".to_string(),
            },
        };

        let result = convert_stream_chunk(&converter, event);
        assert!(result.is_none(), "SignatureDelta should be dropped");
    }
}
