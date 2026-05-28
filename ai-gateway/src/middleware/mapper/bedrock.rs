use std::{collections::HashMap, str::FromStr};

use async_openai::types::{CreateChatCompletionResponse, CreateChatCompletionStreamResponse};
use base64::Engine;
use http::response::Parts;
use serde::de::DeserializeOwned;

use super::{
    MapperError, ResponseBodyConverter, TryConvert, TryConvertStreamData,
    capabilities::ProviderCapabilities, families::ProviderProtocolFamily, model::ModelMapper,
    non_stream_profile::NonStreamFormatProfile,
    non_stream_profile_data::default_non_stream_profile, rules::ProviderRuleSet,
};
use crate::{
    endpoints::bedrock::converse::ConverseResponse,
    middleware::mapper::{
        DEFAULT_MAX_TOKENS, TryConvertError, mime_from_data_uri,
        stream_normalizer::{
            build_reasoning_choice, build_role_choice, build_stream_response, build_stream_usage,
            build_text_choice, build_tool_call_chunk, build_tool_choice,
        },
    },
    types::{model_id::ModelId, provider::InferenceProvider},
};

pub struct BedrockConverter {
    non_stream_profile: NonStreamFormatProfile,
    model_mapper: ModelMapper,
}

impl BedrockConverter {
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
        let capabilities = ProviderCapabilities::for_provider(&InferenceProvider::Bedrock);
        let rules = super::rule_data::default_provider_rules(&InferenceProvider::Bedrock);
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
            .expect("bedrock converter metadata must be valid")
    }

    pub fn try_new_with_metadata(
        capabilities: ProviderCapabilities,
        rules: ProviderRuleSet,
        model_mapper: ModelMapper,
    ) -> Result<Self, MapperError> {
        if capabilities.provider != InferenceProvider::Bedrock
            || rules.provider != InferenceProvider::Bedrock
            || rules.family != ProviderProtocolFamily::BedrockConverse
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

fn json_value_to_document<T>(value: serde_json::Value) -> Result<T, MapperError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(value).map_err(|_| MapperError::InvalidRequest)
}

fn bedrock_image_format_from_data_uri(
    uri: &str,
) -> Option<aws_sdk_bedrockruntime::types::ImageFormat> {
    let mime = mime_from_data_uri(uri)?;
    match mime.mime_type() {
        "image/png" => Some(aws_sdk_bedrockruntime::types::ImageFormat::Png),
        "image/jpeg" => Some(aws_sdk_bedrockruntime::types::ImageFormat::Jpeg),
        "image/gif" => Some(aws_sdk_bedrockruntime::types::ImageFormat::Gif),
        "image/webp" => Some(aws_sdk_bedrockruntime::types::ImageFormat::Webp),
        _ => None,
    }
}

fn map_reasoning_effort_to_bedrock_fields(
    target_model: &ModelId,
    reasoning_effort: Option<&async_openai::types::ReasoningEffort>,
    max_tokens: u32,
) -> Option<u64> {
    use async_openai::types::ReasoningEffort;

    let ModelId::Bedrock(model) = target_model else {
        return None;
    };

    if model.provider != "anthropic" || max_tokens < 1024 {
        return None;
    }

    let budget_tokens = match reasoning_effort? {
        ReasoningEffort::None => return None,
        ReasoningEffort::Low => 1024,
        ReasoningEffort::Medium => usize::max(1024, (max_tokens as usize * 2) / 3),
        ReasoningEffort::High | ReasoningEffort::XHigh => max_tokens as usize,
    };

    Some(u64::try_from(budget_tokens).unwrap_or(1024))
}

impl
    TryConvert<
        async_openai::types::CreateChatCompletionRequest,
        aws_sdk_bedrockruntime::operation::converse::ConverseInput,
    > for BedrockConverter
{
    type Error = MapperError;
    #[allow(clippy::too_many_lines)]
    fn try_convert(
        &self,
        value: async_openai::types::CreateChatCompletionRequest,
    ) -> Result<aws_sdk_bedrockruntime::operation::converse::ConverseInput, Self::Error> {
        use async_openai::types as openai;
        use aws_sdk_bedrockruntime::types as bedrock;
        let source_model = ModelId::from_str(&value.model)?;

        let target_model = self
            .model_mapper
            .map_model(&source_model, &InferenceProvider::Bedrock)?;

        tracing::trace!(source_model = ?source_model, target_model = ?target_model, "mapped model");

        let max_tokens = value.max_completion_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
        let stop_sequences = match value.stop {
            Some(openai::Stop::String(stop)) => Some(vec![stop]),
            Some(openai::Stop::StringArray(stops)) => Some(stops),
            None => None,
        };
        let temperature = value.temperature;
        let top_p = value.top_p;

        let metadata = value
            .user
            .map(|user| HashMap::from([("user_id".to_string(), user)]));
        let additional_model_request_fields = map_reasoning_effort_to_bedrock_fields(
            &target_model,
            value.reasoning_effort.as_ref(),
            max_tokens,
        );

        let tool_choice = match value.tool_choice {
            Some(openai::ChatCompletionToolChoiceOption::Named(tool)) => {
                let t = bedrock::SpecificToolChoice::builder()
                    .name(tool.function.name)
                    .build();
                if let Ok(t) = t {
                    Some(bedrock::ToolChoice::Tool(t))
                } else {
                    None
                }
            }
            Some(openai::ChatCompletionToolChoiceOption::Auto) => Some(bedrock::ToolChoice::Auto(
                bedrock::AutoToolChoice::builder().build(),
            )),
            Some(openai::ChatCompletionToolChoiceOption::Required) => Some(
                bedrock::ToolChoice::Any(bedrock::AnyToolChoice::builder().build()),
            ),
            Some(openai::ChatCompletionToolChoiceOption::None) | None => None,
        };

        let tools = if let Some(tools) = value.tools {
            let mapped_tools: Result<Vec<_>, MapperError> = tools
                .into_iter()
                .filter(|tool| tool.function.is_some())
                .map(|tool| {
                    let func = tool.function.expect("filtered above");
                    let mut tool_spec = bedrock::ToolSpecification::builder()
                        .name(func.name.clone())
                        .set_description(func.description.clone());
                    if let Some(parameters) = func.parameters
                        && let Ok(json_value) = json_value_to_document(parameters)
                    {
                        tool_spec =
                            tool_spec.input_schema(bedrock::ToolInputSchema::Json(json_value));
                    }
                    let tool_spec = tool_spec
                        .build()
                        .map_err(|e| MapperError::FailedToMapBedrockMessage(e.into()))?;

                    Ok(bedrock::Tool::ToolSpec(tool_spec))
                })
                .collect();
            let tools = mapped_tools?;
            if tools.is_empty() { None } else { Some(tools) }
        } else {
            None
        };

        let mut mapped_messages = Vec::with_capacity(value.messages.len());
        for message in value.messages {
            match message {
                openai::ChatCompletionRequestMessage::Developer(_)
                | openai::ChatCompletionRequestMessage::System(_) => {}
                openai::ChatCompletionRequestMessage::User(message) => {
                    let mapped_content: Vec<bedrock::ContentBlock> = match message.content {
                        openai::ChatCompletionRequestUserMessageContent::Text(content) => {
                            vec![bedrock::ContentBlock::Text(content)]
                        }
                        openai::ChatCompletionRequestUserMessageContent::Array(content) => content
                            .into_iter()
                            .filter_map(|part| match part {
                                openai::ChatCompletionRequestUserMessageContentPart::Text(text) => {
                                    Some(bedrock::ContentBlock::Text(text.text))
                                }
                                openai::ChatCompletionRequestUserMessageContentPart::ImageUrl(
                                    image,
                                ) => {
                                    if image.image_url.url.starts_with("http") {
                                        None
                                    } else {
                                        let format = bedrock_image_format_from_data_uri(
                                            &image.image_url.url,
                                        )?;
                                        let (_, encoded) = image.image_url.url.split_once(',')?;
                                        let bytes = base64::engine::general_purpose::STANDARD
                                            .decode(encoded)
                                            .ok()?;
                                        let mapped_image = bedrock::ImageBlock::builder()
                                            .format(format)
                                            .source(bedrock::ImageSource::Bytes(
                                                aws_sdk_bedrockruntime::primitives::Blob::new(
                                                    bytes,
                                                ),
                                            ))
                                            .build()
                                            .ok()?;
                                        Some(bedrock::ContentBlock::Image(mapped_image))
                                    }
                                }
                                openai::ChatCompletionRequestUserMessageContentPart::InputAudio(
                                    _audio,
                                ) => None,
                            })
                            .collect(),
                    };
                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::User)
                        .set_content(Some(mapped_content))
                        .build();

                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Assistant(message) => {
                    let mapped_content = match message.content {
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Text(content)) => {
                            vec![bedrock::ContentBlock::Text(content)]
                        }
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Array(content)) => {
                            content.into_iter().map(|part| {
                                match part {
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Text(text) => {
                                        bedrock::ContentBlock::Text(text.text)
                                    }
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Refusal(text) => {
                                        bedrock::ContentBlock::Text(text.refusal.clone())
                                    }
                                }
                            }).collect()
                        }
                        None => continue,
                    };
                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::Assistant)
                        .set_content(Some(mapped_content))
                        .build();
                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Tool(message) => {
                    let mapped_content = match message.content {
                        openai::ChatCompletionRequestToolMessageContent::Text(text) => {
                            let x = bedrock::ToolResultBlock::builder()
                                .tool_use_id(message.tool_call_id)
                                .content(bedrock::ToolResultContentBlock::Text(text))
                                .build();
                            if let Ok(tool_result_block) = x {
                                vec![bedrock::ContentBlock::ToolResult(tool_result_block)]
                            } else {
                                vec![]
                            }
                        }
                        openai::ChatCompletionRequestToolMessageContent::Array(content) => content
                            .into_iter()
                            .filter_map(|part| match part {
                                openai::ChatCompletionRequestToolMessageContentPart::Text(text) => {
                                    let tool_result_block = bedrock::ToolResultBlock::builder()
                                        .tool_use_id(message.tool_call_id.clone())
                                        .content(bedrock::ToolResultContentBlock::Text(text.text))
                                        .build()
                                        .ok()?;
                                    Some(bedrock::ContentBlock::ToolResult(tool_result_block))
                                }
                            })
                            .collect(),
                    };

                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::Assistant)
                        .set_content(Some(mapped_content))
                        .build();
                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Function(message) => {
                    let tools_ref = tools.as_ref();
                    let Some(tool) = tools_ref.and_then(|tools| {
                        tools.iter().find_map(|tool| {
                            if let bedrock::Tool::ToolSpec(spec) = tool {
                                if spec.name == message.name {
                                    Some(tool.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                    }) else {
                        continue;
                    };

                    let tool_spec = tool
                        .as_tool_spec()
                        .map_err(|_| MapperError::ToolMappingInvalid(message.name.clone()))?;

                    let input = tool_spec
                        .input_schema
                        .as_ref()
                        .and_then(|schema| schema.as_json().ok())
                        .cloned();

                    let tool_use = bedrock::ToolUseBlock::builder()
                        .name(message.name.clone())
                        .tool_use_id(message.name.clone())
                        .set_input(input)
                        .build();
                    let mapped_content = if let Ok(tool_use) = tool_use {
                        vec![bedrock::ContentBlock::ToolUse(tool_use)]
                    } else {
                        vec![]
                    };

                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::Assistant)
                        .set_content(Some(mapped_content))
                        .build();
                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
            }
        }

        let mut builder = aws_sdk_bedrockruntime::operation::converse::ConverseInput::builder()
            .model_id(target_model.to_string())
            .set_messages(Some(mapped_messages))
            .set_request_metadata(metadata);

        if let Some(budget_tokens) = additional_model_request_fields {
            let thinking = HashMap::<String, _>::from([
                ("type".to_string(), "enabled".into()),
                ("budget_tokens".to_string(), budget_tokens.into()),
            ]);
            let additional_fields =
                HashMap::<String, _>::from([("thinking".to_string(), thinking.into())]);
            builder = builder.additional_model_request_fields(additional_fields.into());
        }

        if let Some(tools) = tools {
            let tool_config = bedrock::ToolConfiguration::builder()
                .set_tool_choice(tool_choice)
                .set_tools(Some(tools))
                .build();
            if let Ok(tool_config) = tool_config {
                builder = builder.tool_config(tool_config);
            }
        }
        #[allow(clippy::cast_possible_wrap)]
        let inference_config = Some(
            bedrock::InferenceConfiguration::builder()
                .top_p(top_p.unwrap_or_default())
                .temperature(temperature.unwrap_or_default())
                .max_tokens(i32::try_from(max_tokens).unwrap_or(DEFAULT_MAX_TOKENS as i32))
                .set_stop_sequences(stop_sequences)
                .build(),
        );
        let converse_input = builder
            .set_inference_config(inference_config)
            .build()
            .map_err(|e| MapperError::FailedToMapBedrockMessage(e.into()))?;

        Ok(converse_input)
    }

    #[allow(clippy::too_many_lines)]
    fn try_convert_model_passthrough(
        &self,
        value: async_openai::types::CreateChatCompletionRequest,
    ) -> Result<aws_sdk_bedrockruntime::operation::converse::ConverseInput, Self::Error> {
        use async_openai::types as openai;
        use aws_sdk_bedrockruntime::types as bedrock;

        let target_model = value.model.clone();
        let max_tokens = value.max_completion_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
        let stop_sequences = match value.stop {
            Some(openai::Stop::String(stop)) => Some(vec![stop]),
            Some(openai::Stop::StringArray(stops)) => Some(stops),
            None => None,
        };
        let temperature = value.temperature;
        let top_p = value.top_p;

        let metadata = value
            .user
            .map(|user| HashMap::from([("user_id".to_string(), user)]));
        let parsed_target_model =
            ModelId::from_str_and_provider(InferenceProvider::Bedrock, &target_model).ok();
        let additional_model_request_fields =
            parsed_target_model.as_ref().and_then(|target_model| {
                map_reasoning_effort_to_bedrock_fields(
                    target_model,
                    value.reasoning_effort.as_ref(),
                    max_tokens,
                )
            });

        let tool_choice = match value.tool_choice {
            Some(openai::ChatCompletionToolChoiceOption::Named(tool)) => {
                let t = bedrock::SpecificToolChoice::builder()
                    .name(tool.function.name)
                    .build();
                if let Ok(t) = t {
                    Some(bedrock::ToolChoice::Tool(t))
                } else {
                    None
                }
            }
            Some(openai::ChatCompletionToolChoiceOption::Auto) => Some(bedrock::ToolChoice::Auto(
                bedrock::AutoToolChoice::builder().build(),
            )),
            Some(openai::ChatCompletionToolChoiceOption::Required) => Some(
                bedrock::ToolChoice::Any(bedrock::AnyToolChoice::builder().build()),
            ),
            Some(openai::ChatCompletionToolChoiceOption::None) | None => None,
        };

        let tools = if let Some(tools) = value.tools {
            let mapped_tools: Result<Vec<_>, MapperError> = tools
                .into_iter()
                .filter(|tool| tool.function.is_some())
                .map(|tool| {
                    let func = tool.function.expect("filtered above");
                    let mut tool_spec = bedrock::ToolSpecification::builder()
                        .name(func.name.clone())
                        .set_description(func.description.clone());
                    if let Some(parameters) = func.parameters {
                        if let Ok(json_value) = json_value_to_document(parameters) {
                            tool_spec =
                                tool_spec.input_schema(bedrock::ToolInputSchema::Json(json_value));
                        }
                    }
                    let tool_spec = tool_spec
                        .build()
                        .map_err(|e| MapperError::FailedToMapBedrockMessage(e.into()))?;

                    Ok(bedrock::Tool::ToolSpec(tool_spec))
                })
                .collect();
            let tools = mapped_tools?;
            if tools.is_empty() { None } else { Some(tools) }
        } else {
            None
        };

        let mut mapped_messages = Vec::with_capacity(value.messages.len());
        for message in value.messages {
            match message {
                openai::ChatCompletionRequestMessage::Developer(_)
                | openai::ChatCompletionRequestMessage::System(_) => {}
                openai::ChatCompletionRequestMessage::User(message) => {
                    let mapped_content: Vec<bedrock::ContentBlock> = match message.content {
                        openai::ChatCompletionRequestUserMessageContent::Text(content) => {
                            vec![bedrock::ContentBlock::Text(content)]
                        }
                        openai::ChatCompletionRequestUserMessageContent::Array(content) => content
                            .into_iter()
                            .filter_map(|part| match part {
                                openai::ChatCompletionRequestUserMessageContentPart::Text(text) => {
                                    Some(bedrock::ContentBlock::Text(text.text))
                                }
                                openai::ChatCompletionRequestUserMessageContentPart::ImageUrl(
                                    image,
                                ) => {
                                    if image.image_url.url.starts_with("http") {
                                        None
                                    } else {
                                        let format = bedrock_image_format_from_data_uri(
                                            &image.image_url.url,
                                        )?;
                                        let (_, encoded) = image.image_url.url.split_once(',')?;
                                        let bytes = base64::engine::general_purpose::STANDARD
                                            .decode(encoded)
                                            .ok()?;
                                        let mapped_image = bedrock::ImageBlock::builder()
                                            .format(format)
                                            .source(bedrock::ImageSource::Bytes(
                                                aws_sdk_bedrockruntime::primitives::Blob::new(
                                                    bytes,
                                                ),
                                            ))
                                            .build()
                                            .ok()?;
                                        Some(bedrock::ContentBlock::Image(mapped_image))
                                    }
                                }
                                openai::ChatCompletionRequestUserMessageContentPart::InputAudio(
                                    _,
                                ) => None,
                            })
                            .collect(),
                    };
                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::User)
                        .set_content(Some(mapped_content))
                        .build();

                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Assistant(message) => {
                    let mapped_content = match message.content {
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Text(content)) => {
                            vec![bedrock::ContentBlock::Text(content)]
                        }
                        Some(openai::ChatCompletionRequestAssistantMessageContent::Array(content)) => {
                            content.into_iter().map(|part| {
                                match part {
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Text(text) => {
                                        bedrock::ContentBlock::Text(text.text)
                                    }
                                    openai::ChatCompletionRequestAssistantMessageContentPart::Refusal(text) => {
                                        bedrock::ContentBlock::Text(text.refusal.clone())
                                    }
                                }
                            }).collect()
                        }
                        None => continue,
                    };
                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::Assistant)
                        .set_content(Some(mapped_content))
                        .build();
                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Tool(message) => {
                    let mapped_content = match message.content {
                        openai::ChatCompletionRequestToolMessageContent::Text(text) => {
                            let x = bedrock::ToolResultBlock::builder()
                                .tool_use_id(message.tool_call_id)
                                .content(bedrock::ToolResultContentBlock::Text(text))
                                .build();
                            if let Ok(tool_result_block) = x {
                                vec![bedrock::ContentBlock::ToolResult(tool_result_block)]
                            } else {
                                vec![]
                            }
                        }
                        openai::ChatCompletionRequestToolMessageContent::Array(content) => content
                            .into_iter()
                            .filter_map(|part| match part {
                                openai::ChatCompletionRequestToolMessageContentPart::Text(text) => {
                                    let tool_result_block = bedrock::ToolResultBlock::builder()
                                        .tool_use_id(message.tool_call_id.clone())
                                        .content(bedrock::ToolResultContentBlock::Text(text.text))
                                        .build()
                                        .ok()?;
                                    Some(bedrock::ContentBlock::ToolResult(tool_result_block))
                                }
                            })
                            .collect(),
                    };

                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::Assistant)
                        .set_content(Some(mapped_content))
                        .build();
                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
                openai::ChatCompletionRequestMessage::Function(message) => {
                    let tools_ref = tools.as_ref();
                    let Some(tool) = tools_ref.and_then(|tools| {
                        tools.iter().find_map(|tool| {
                            if let bedrock::Tool::ToolSpec(spec) = tool {
                                if spec.name == message.name {
                                    Some(tool.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                    }) else {
                        continue;
                    };

                    let tool_spec = tool
                        .as_tool_spec()
                        .map_err(|_| MapperError::ToolMappingInvalid(message.name.clone()))?;

                    let input = tool_spec
                        .input_schema
                        .as_ref()
                        .and_then(|schema| schema.as_json().ok())
                        .cloned();

                    let tool_use = bedrock::ToolUseBlock::builder()
                        .name(message.name.clone())
                        .tool_use_id(message.name.clone())
                        .set_input(input)
                        .build();
                    let mapped_content = if let Ok(tool_use) = tool_use {
                        vec![bedrock::ContentBlock::ToolUse(tool_use)]
                    } else {
                        vec![]
                    };

                    let mapped_message = bedrock::Message::builder()
                        .role(bedrock::ConversationRole::Assistant)
                        .set_content(Some(mapped_content))
                        .build();
                    if let Ok(mapped_message) = mapped_message {
                        mapped_messages.push(mapped_message);
                    }
                }
            }
        }

        let mut builder = aws_sdk_bedrockruntime::operation::converse::ConverseInput::builder()
            .model_id(target_model)
            .set_messages(Some(mapped_messages))
            .set_request_metadata(metadata);

        if let Some(budget_tokens) = additional_model_request_fields {
            let thinking = HashMap::<String, _>::from([
                ("type".to_string(), "enabled".into()),
                ("budget_tokens".to_string(), budget_tokens.into()),
            ]);
            let additional_fields =
                HashMap::<String, _>::from([("thinking".to_string(), thinking.into())]);
            builder = builder.additional_model_request_fields(additional_fields.into());
        }

        if let Some(tools) = tools {
            let tool_config = bedrock::ToolConfiguration::builder()
                .set_tool_choice(tool_choice)
                .set_tools(Some(tools))
                .build();
            if let Ok(tool_config) = tool_config {
                builder = builder.tool_config(tool_config);
            }
        }
        #[allow(clippy::cast_possible_wrap)]
        let inference_config = Some(
            bedrock::InferenceConfiguration::builder()
                .top_p(top_p.unwrap_or_default())
                .temperature(temperature.unwrap_or_default())
                .max_tokens(i32::try_from(max_tokens).unwrap_or(DEFAULT_MAX_TOKENS as i32))
                .set_stop_sequences(stop_sequences)
                .build(),
        );
        let converse_input = builder
            .set_inference_config(inference_config)
            .build()
            .map_err(|e| MapperError::FailedToMapBedrockMessage(e.into()))?;

        Ok(converse_input)
    }

    fn passthrough_model_for_mapper_context(
        &self,
        target: &aws_sdk_bedrockruntime::operation::converse::ConverseInput,
    ) -> Option<String> {
        target.model_id.clone()
    }
}

impl TryConvert<ConverseResponse, CreateChatCompletionResponse> for BedrockConverter {
    type Error = MapperError;

    #[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
    fn try_convert(
        &self,
        value: ConverseResponse,
    ) -> std::result::Result<CreateChatCompletionResponse, Self::Error> {
        super::non_stream_response_interpreter::convert_bedrock_response(
            &self.non_stream_profile,
            value,
        )
    }
}

impl ResponseBodyConverter<ConverseResponse, CreateChatCompletionResponse> for BedrockConverter {
    fn try_convert_response(
        &self,
        resp_parts: &Parts,
        value: ConverseResponse,
    ) -> std::result::Result<CreateChatCompletionResponse, Self::Error> {
        super::non_stream_response_interpreter::convert_bedrock_response_from_parts(
            resp_parts,
            &self.non_stream_profile,
            value,
        )
    }
}

impl
    TryConvertStreamData<
        aws_sdk_bedrockruntime::types::ConverseStreamOutput,
        CreateChatCompletionStreamResponse,
    > for BedrockConverter
{
    type Error = MapperError;

    #[allow(clippy::too_many_lines)]
    fn try_convert_chunk(
        &self,
        value: aws_sdk_bedrockruntime::types::ConverseStreamOutput,
        _anthropic_openai_usage: Option<&crate::types::extensions::AnthropicOpenAiUsageCell>,
    ) -> Result<std::option::Option<CreateChatCompletionStreamResponse>, Self::Error> {
        convert_bedrock_stream_chunk(value)
    }
}

impl
    TryConvertError<
        crate::endpoints::bedrock::converse::ConverseError,
        async_openai::error::WrappedError,
    > for BedrockConverter
{
    type Error = MapperError;

    fn try_convert_error(
        &self,
        resp_parts: &Parts,
        _value: crate::endpoints::bedrock::converse::ConverseError,
    ) -> Result<async_openai::error::WrappedError, Self::Error> {
        Ok(super::openai_error_from_status(resp_parts.status, None))
    }
}

fn convert_bedrock_stream_chunk(
    value: aws_sdk_bedrockruntime::types::ConverseStreamOutput,
) -> Result<Option<CreateChatCompletionStreamResponse>, MapperError> {
    use async_openai::types as openai;
    use aws_sdk_bedrockruntime::types as bedrock;

    const PLACEHOLDER_STREAM_ID: &str = "bedrock-stream-id";
    const PLACEHOLDER_MODEL_NAME: &str = "bedrock-model";

    #[allow(deprecated)]
    let mut choices = Vec::new();
    let mut completion_usage = build_stream_usage(0, 0);
    match value {
        bedrock::ConverseStreamOutput::MessageStart(message) => {
            choices.push(build_role_choice(
                0,
                match message.role {
                    bedrock::ConversationRole::Assistant => openai::Role::Assistant,
                    bedrock::ConversationRole::User => openai::Role::User,
                    _ => openai::Role::System,
                },
            ));
        }
        bedrock::ConverseStreamOutput::ContentBlockStart(content_block_start) => {
            if let Some(bedrock::ContentBlockStart::ToolUse(tool_use)) = content_block_start.start {
                let tool_call_chunk = build_tool_call_chunk(
                    content_block_start
                        .content_block_index
                        .try_into()
                        .unwrap_or(0),
                    Some(tool_use.tool_use_id),
                    Some(tool_use.name),
                    Some(String::new()),
                );
                choices.push(build_tool_choice(0, tool_call_chunk));
            }
        }
        bedrock::ConverseStreamOutput::ContentBlockDelta(content_block_delta_event) => {
            match content_block_delta_event.delta {
                Some(bedrock::ContentBlockDelta::Text(text)) => {
                    choices.push(build_text_choice(
                        u32::try_from(content_block_delta_event.content_block_index).unwrap_or(0),
                        text,
                    ));
                }
                Some(bedrock::ContentBlockDelta::ToolUse(tool_use)) => {
                    let tool_call_chunk = build_tool_call_chunk(
                        u32::try_from(content_block_delta_event.content_block_index).unwrap_or(0),
                        None,
                        None,
                        Some(tool_use.input),
                    );
                    choices.push(build_tool_choice(0, tool_call_chunk));
                }
                Some(bedrock::ContentBlockDelta::ReasoningContent(reasoning)) => {
                    if let Ok(text) = reasoning.as_text() {
                        choices.push(build_reasoning_choice(0, text.clone()));
                    }
                }
                _ => {}
            }
        }
        bedrock::ConverseStreamOutput::Metadata(metadata) => {
            if let Some(usage) = metadata.usage {
                completion_usage = build_stream_usage(
                    u32::try_from(usage.input_tokens).unwrap_or(0),
                    u32::try_from(usage.output_tokens).unwrap_or(0),
                );
            }
        }
        bedrock::ConverseStreamOutput::ContentBlockStop(_)
        | bedrock::ConverseStreamOutput::MessageStop(_)
        | _ => {}
    }

    Ok(Some(build_stream_response(
        PLACEHOLDER_STREAM_ID.to_string(),
        PLACEHOLDER_MODEL_NAME.to_string(),
        choices,
        Some(completion_usage),
    )))
}

#[cfg(test)]
mod tests {
    use async_openai::types::CreateChatCompletionRequest;
    use aws_sdk_bedrockruntime::types::{
        ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart, ContentBlockStartEvent,
        ConversationRole, ConverseStreamMetadataEvent, ConverseStreamOutput, MessageStartEvent,
        TokenUsage, ToolUseBlockDelta, ToolUseBlockStart,
    };
    use serde_json::json;

    use super::convert_bedrock_stream_chunk;
    use crate::{
        middleware::mapper::{
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
            "model": "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
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
            "model": "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
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
    async fn bedrock_converter_does_not_reapply_non_stream_profile_to_tool_fields() {
        let mut non_stream_profile = default_non_stream_profile(&InferenceProvider::Bedrock);
        non_stream_profile.request.tool_choice_mode = ToolChoiceMode::Unsupported;
        let converter = super::BedrockConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
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

        let converted: aws_sdk_bedrockruntime::operation::converse::ConverseInput =
            crate::middleware::mapper::TryConvert::try_convert(&converter, request)
                .expect("conversion should succeed");

        assert!(converted.tool_config().is_some());
    }

    #[tokio::test]
    async fn bedrock_converter_does_not_reapply_non_stream_profile_to_reasoning_effort() {
        let mut non_stream_profile = default_non_stream_profile(&InferenceProvider::Bedrock);
        non_stream_profile.request.reasoning_mode = ReasoningMode::Unsupported;
        let converter = super::BedrockConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );
        let mut request = sample_request();
        request.reasoning_effort = Some(async_openai::types::ReasoningEffort::High);

        let converted: aws_sdk_bedrockruntime::operation::converse::ConverseInput =
            crate::middleware::mapper::TryConvert::try_convert(&converter, request)
                .expect("conversion should succeed");

        assert!(converted.additional_model_request_fields().is_some());
    }

    #[tokio::test]
    async fn bedrock_converter_does_not_reject_multimodal_without_request_engine() {
        let mut non_stream_profile = default_non_stream_profile(&InferenceProvider::Bedrock);
        non_stream_profile.request.multimodal_mode = MultimodalMode::Unsupported;
        let converter = super::BedrockConverter::new_with_profile(
            non_stream_profile,
            sample_model_mapper().await,
        );

        let converted = crate::middleware::mapper::TryConvert::try_convert(
            &converter,
            sample_multimodal_request(),
        )
        .expect("conversion should succeed");

        assert!(!converted.messages().is_empty());
    }

    #[tokio::test]
    async fn bedrock_converter_can_be_built_from_metadata_rules_without_reapplying_them() {
        let capabilities = ProviderCapabilities::for_provider(&InferenceProvider::Bedrock);
        let mut rules = default_provider_rules(&InferenceProvider::Bedrock);
        rules.request.tool_choice_mode = ToolChoiceMode::Unsupported;
        let converter = super::BedrockConverter::new_with_metadata(
            capabilities,
            rules,
            sample_model_mapper().await,
        );
        let request: CreateChatCompletionRequest = serde_json::from_value(json!({
            "model": "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
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
                        "name": "lookup_weather"
                    }
                }
            ],
            "tool_choice": "auto"
        }))
        .expect("request should deserialize");

        let converted: aws_sdk_bedrockruntime::operation::converse::ConverseInput =
            crate::middleware::mapper::TryConvert::try_convert(&converter, request)
                .expect("conversion should succeed");

        assert!(converted.tool_config().is_some());
    }

    #[test]
    fn bedrock_stream_message_start_maps_role_delta() {
        let event = ConverseStreamOutput::MessageStart(
            MessageStartEvent::builder()
                .role(ConversationRole::Assistant)
                .build()
                .expect("message start"),
        );

        let response = convert_bedrock_stream_chunk(event)
            .expect("convert ok")
            .expect("response");

        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0]
                .delta
                .role
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("assistant")
        );
    }

    #[test]
    fn bedrock_stream_tool_start_maps_tool_call_chunk() {
        let event = ConverseStreamOutput::ContentBlockStart(
            ContentBlockStartEvent::builder()
                .content_block_index(2)
                .start(ContentBlockStart::ToolUse(
                    ToolUseBlockStart::builder()
                        .tool_use_id("call_1")
                        .name("lookup_weather")
                        .build()
                        .expect("tool use start"),
                ))
                .build()
                .expect("content block start"),
        );

        let response = convert_bedrock_stream_chunk(event)
            .expect("convert ok")
            .expect("response");

        let tool_call = &response.choices[0]
            .delta
            .tool_calls
            .as_ref()
            .expect("tool calls")[0];
        assert_eq!(tool_call.index, 2);
        assert_eq!(tool_call.id.as_deref(), Some("call_1"));
        assert_eq!(
            tool_call
                .function
                .as_ref()
                .and_then(|function| function.name.as_deref()),
            Some("lookup_weather")
        );
    }

    #[test]
    fn bedrock_stream_text_delta_maps_openai_text_delta() {
        let event = ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(1)
                .delta(ContentBlockDelta::Text("hello".to_string()))
                .build()
                .expect("content block delta"),
        );

        let response = convert_bedrock_stream_chunk(event)
            .expect("convert ok")
            .expect("response");

        assert_eq!(response.choices[0].index, 1);
        assert_eq!(response.choices[0].delta.content.as_deref(), Some("hello"));
    }

    #[test]
    fn bedrock_stream_tool_delta_maps_openai_tool_arguments() {
        let event = ConverseStreamOutput::ContentBlockDelta(
            ContentBlockDeltaEvent::builder()
                .content_block_index(0)
                .delta(ContentBlockDelta::ToolUse(
                    ToolUseBlockDelta::builder()
                        .input("{\"city\":\"Paris\"}")
                        .build()
                        .expect("tool use delta"),
                ))
                .build()
                .expect("content block delta"),
        );

        let response = convert_bedrock_stream_chunk(event)
            .expect("convert ok")
            .expect("response");

        let tool_call = &response.choices[0]
            .delta
            .tool_calls
            .as_ref()
            .expect("tool calls")[0];
        assert_eq!(
            tool_call
                .function
                .as_ref()
                .and_then(|function| function.arguments.as_deref()),
            Some("{\"city\":\"Paris\"}")
        );
    }

    #[test]
    fn bedrock_stream_metadata_maps_usage() {
        let event = ConverseStreamOutput::Metadata(
            ConverseStreamMetadataEvent::builder()
                .usage(
                    TokenUsage::builder()
                        .input_tokens(9)
                        .output_tokens(4)
                        .total_tokens(13)
                        .build()
                        .expect("usage"),
                )
                .build(),
        );

        let response = convert_bedrock_stream_chunk(event)
            .expect("convert ok")
            .expect("response");

        let usage = response.usage.expect("usage");
        assert_eq!(usage.prompt_tokens, 9);
        assert_eq!(usage.completion_tokens, 4);
        assert_eq!(usage.total_tokens, 13);
    }
}
