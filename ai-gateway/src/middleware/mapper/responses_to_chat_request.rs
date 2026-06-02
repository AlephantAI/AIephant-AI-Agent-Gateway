//! Convert OpenAI Responses API `CreateResponse` into a Chat Completions
//! `CreateChatCompletionRequest` so that existing provider converters
//! (Anthropic, Bedrock, Google, Ollama) can be reused.

use std::collections::HashMap;

use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionNamedToolChoice,
    ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestDeveloperMessage,
    ChatCompletionRequestDeveloperMessageContent, ChatCompletionRequestMessage,
    ChatCompletionRequestMessageContentPartImage,
    ChatCompletionRequestMessageContentPartText,
    ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent,
    ChatCompletionRequestToolMessage, ChatCompletionRequestToolMessageContent,
    ChatCompletionRequestUserMessage, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ChatCompletionTool,
    ChatCompletionToolChoiceOption, ChatCompletionToolType,
    CreateChatCompletionRequest, FunctionCall, FunctionName, FunctionObject,
    ImageUrl, ResponseFormat, ServiceTier as ChatServiceTier,
    responses::{
        ContentType, CreateResponse, Input, InputContent, InputItem,
        InputMessage, Role, ServiceTier as ResponsesServiceTier,
        TextResponseFormat, ToolChoice, ToolChoiceMode, ToolDefinition,
    },
};

use crate::error::mapper::MapperError;

pub fn convert(
    request: CreateResponse,
) -> Result<CreateChatCompletionRequest, MapperError> {
    let messages = convert_input(&request)?;
    let tools = convert_tools(&request);
    let tool_choice = convert_tool_choice(&request);
    let reasoning_effort =
        request.reasoning.as_ref().and_then(|r| r.effort.clone());
    let response_format = convert_response_format(&request);
    let metadata = request
        .metadata
        .as_ref()
        .and_then(|m| serde_json::to_value(m).ok());
    let service_tier = request.service_tier.map(convert_service_tier);

    #[allow(deprecated)]
    Ok(CreateChatCompletionRequest {
        messages,
        model: request.model,
        store: request.store,
        reasoning_effort,
        metadata,
        max_completion_tokens: request.max_output_tokens,
        stream: request.stream,
        stream_options: None,
        temperature: request.temperature,
        top_p: request.top_p,
        tools,
        tool_choice,
        parallel_tool_calls: request.parallel_tool_calls,
        user: request.user,
        service_tier,
        response_format,
        frequency_penalty: None,
        logit_bias: None,
        logprobs: None,
        top_logprobs: request.top_logprobs.map(|v| v as u8),
        max_tokens: None,
        n: None,
        modalities: None,
        prediction: None,
        audio: None,
        presence_penalty: None,
        seed: None,
        stop: None,
        function_call: None,
        functions: None,
        web_search_options: None,
        extra: HashMap::new(),
    })
}

fn convert_input(
    request: &CreateResponse,
) -> Result<Vec<ChatCompletionRequestMessage>, MapperError> {
    let mut messages = Vec::new();

    if let Some(ref instructions) = request.instructions {
        messages.push(ChatCompletionRequestMessage::Developer(
            ChatCompletionRequestDeveloperMessage {
                content: ChatCompletionRequestDeveloperMessageContent::Text(
                    instructions.clone(),
                ),
                name: None,
            },
        ));
    }

    match &request.input {
        Input::Text(text) => {
            messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    content: ChatCompletionRequestUserMessageContent::Text(
                        text.clone(),
                    ),
                    name: None,
                },
            ));
        }
        Input::Items(items) => {
            for item in items {
                match item {
                    InputItem::Message(msg) => {
                        convert_message(msg, &mut messages)?;
                    }
                    InputItem::Custom(v) => {
                        convert_custom_item(v, &mut messages)?;
                    }
                }
            }
        }
    }

    Ok(messages)
}

fn convert_message(
    msg: &InputMessage,
    messages: &mut Vec<ChatCompletionRequestMessage>,
) -> Result<(), MapperError> {
    match msg.role {
        Role::User => {
            let content = convert_user_content(&msg.content)?;
            messages.push(ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    content,
                    name: None,
                },
            ));
        }
        Role::Assistant => {
            let content = convert_assistant_content(&msg.content);
            #[allow(deprecated)]
            messages.push(ChatCompletionRequestMessage::Assistant(
                ChatCompletionRequestAssistantMessage {
                    content,
                    tool_calls: None,
                    refusal: None,
                    name: None,
                    audio: None,
                    function_call: None,
                },
            ));
        }
        Role::System => {
            let text = extract_plain_text(&msg.content);
            messages.push(ChatCompletionRequestMessage::System(
                ChatCompletionRequestSystemMessage {
                    content: ChatCompletionRequestSystemMessageContent::Text(
                        text,
                    ),
                    name: None,
                },
            ));
        }
        Role::Developer => {
            let text = extract_plain_text(&msg.content);
            messages.push(ChatCompletionRequestMessage::Developer(
                ChatCompletionRequestDeveloperMessage {
                    content: ChatCompletionRequestDeveloperMessageContent::Text(
                        text,
                    ),
                    name: None,
                },
            ));
        }
    }
    Ok(())
}

fn convert_user_content(
    content: &InputContent,
) -> Result<ChatCompletionRequestUserMessageContent, MapperError> {
    match content {
        InputContent::TextInput(text) => {
            Ok(ChatCompletionRequestUserMessageContent::Text(text.clone()))
        }
        InputContent::InputItemContentList(parts) => {
            let chat_parts: Vec<_> = parts
                .iter()
                .filter_map(convert_content_type_to_user_part)
                .collect();
            Ok(ChatCompletionRequestUserMessageContent::Array(chat_parts))
        }
    }
}

/// Inner struct fields on `InputText` / `InputImage` are private;
/// serialize to JSON to extract values.
fn convert_content_type_to_user_part(
    part: &ContentType,
) -> Option<ChatCompletionRequestUserMessageContentPart> {
    match part {
        ContentType::InputText(t) => {
            let v = serde_json::to_value(t).ok()?;
            let text = v.get("text")?.as_str()?.to_string();
            Some(ChatCompletionRequestUserMessageContentPart::Text(
                ChatCompletionRequestMessageContentPartText { text },
            ))
        }
        ContentType::InputImage(img) => {
            let v = serde_json::to_value(img).ok()?;
            let url = v.get("image_url")?.as_str()?.to_string();
            let detail = v
                .get("detail")
                .and_then(|d| serde_json::from_value(d.clone()).ok());
            Some(ChatCompletionRequestUserMessageContentPart::ImageUrl(
                ChatCompletionRequestMessageContentPartImage {
                    image_url: ImageUrl { url, detail },
                },
            ))
        }
        ContentType::InputFile(_) => None,
    }
}

fn convert_assistant_content(
    content: &InputContent,
) -> Option<ChatCompletionRequestAssistantMessageContent> {
    let text = extract_plain_text(content);
    if text.is_empty() {
        None
    } else {
        Some(ChatCompletionRequestAssistantMessageContent::Text(text))
    }
}

fn extract_plain_text(content: &InputContent) -> String {
    match content {
        InputContent::TextInput(text) => text.clone(),
        InputContent::InputItemContentList(parts) => {
            let mut buf = String::new();
            for part in parts {
                if let ContentType::InputText(t) = part
                    && let Ok(v) = serde_json::to_value(t)
                    && let Some(text) = v.get("text").and_then(|s| s.as_str())
                {
                    buf.push_str(text);
                }
            }
            buf
        }
    }
}

fn convert_custom_item(
    v: &serde_json::Value,
    messages: &mut Vec<ChatCompletionRequestMessage>,
) -> Result<(), MapperError> {
    let item_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match item_type {
        "function_call" => {
            let call_id = v
                .get("call_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let name = v
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = v
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("{}")
                .to_string();

            let tool_call = ChatCompletionMessageToolCall {
                id: call_id,
                r#type: ChatCompletionToolType::Function,
                function: FunctionCall { name, arguments },
            };

            ensure_last_is_assistant(messages);
            if let Some(ChatCompletionRequestMessage::Assistant(asst)) =
                messages.last_mut()
            {
                asst.tool_calls.get_or_insert_with(Vec::new).push(tool_call);
            }
        }
        "function_call_output" => {
            let call_id = v
                .get("call_id")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let output = v
                .get("output")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string();

            messages.push(ChatCompletionRequestMessage::Tool(
                ChatCompletionRequestToolMessage {
                    content: ChatCompletionRequestToolMessageContent::Text(
                        output,
                    ),
                    tool_call_id: call_id,
                },
            ));
        }
        _ => {}
    }

    Ok(())
}

fn ensure_last_is_assistant(messages: &mut Vec<ChatCompletionRequestMessage>) {
    if !matches!(
        messages.last(),
        Some(ChatCompletionRequestMessage::Assistant(_))
    ) {
        #[allow(deprecated)]
        messages.push(ChatCompletionRequestMessage::Assistant(
            ChatCompletionRequestAssistantMessage {
                content: None,
                tool_calls: None,
                refusal: None,
                name: None,
                audio: None,
                function_call: None,
            },
        ));
    }
}

fn convert_tools(request: &CreateResponse) -> Option<Vec<ChatCompletionTool>> {
    let tools = request.tools.as_ref()?;
    let chat_tools: Vec<_> = tools
        .iter()
        .filter_map(|tool| {
            if let ToolDefinition::Function(f) = tool {
                Some(ChatCompletionTool {
                    r#type: ChatCompletionToolType::Function,
                    function: Some(FunctionObject {
                        name: f.name.clone(),
                        description: f.description.clone(),
                        parameters: Some(f.parameters.clone()),
                        strict: Some(f.strict),
                    }),
                    extra: HashMap::new(),
                })
            } else {
                None
            }
        })
        .collect();
    if chat_tools.is_empty() {
        None
    } else {
        Some(chat_tools)
    }
}

fn convert_tool_choice(
    request: &CreateResponse,
) -> Option<ChatCompletionToolChoiceOption> {
    request.tool_choice.as_ref().map(|tc| match tc {
        ToolChoice::Mode(ToolChoiceMode::None) => {
            ChatCompletionToolChoiceOption::None
        }
        ToolChoice::Mode(ToolChoiceMode::Auto) => {
            ChatCompletionToolChoiceOption::Auto
        }
        ToolChoice::Mode(ToolChoiceMode::Required) => {
            ChatCompletionToolChoiceOption::Required
        }
        ToolChoice::Function { name } => ChatCompletionToolChoiceOption::Named(
            ChatCompletionNamedToolChoice {
                r#type: ChatCompletionToolType::Function,
                function: FunctionName { name: name.clone() },
            },
        ),
        ToolChoice::Hosted { .. } => ChatCompletionToolChoiceOption::Auto,
    })
}

fn convert_response_format(request: &CreateResponse) -> Option<ResponseFormat> {
    let text_config = request.text.as_ref()?;
    Some(match &text_config.format {
        TextResponseFormat::Text => ResponseFormat::Text,
        TextResponseFormat::JsonObject => ResponseFormat::JsonObject,
        TextResponseFormat::JsonSchema(schema) => ResponseFormat::JsonSchema {
            json_schema: schema.clone(),
        },
    })
}

fn convert_service_tier(tier: ResponsesServiceTier) -> ChatServiceTier {
    match tier {
        ResponsesServiceTier::Auto => ChatServiceTier::Auto,
        ResponsesServiceTier::Default => ChatServiceTier::Default,
        ResponsesServiceTier::Flex => ChatServiceTier::Flex,
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::ReasoningEffort;
    use serde_json::json;

    use super::*;

    fn make_request(v: serde_json::Value) -> CreateResponse {
        serde_json::from_value(v).expect("test request should deserialize")
    }

    #[test]
    fn convert_simple_text_input() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": "hello"
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(
            &result.messages[0],
            ChatCompletionRequestMessage::User(u)
                if matches!(
                    &u.content,
                    ChatCompletionRequestUserMessageContent::Text(t)
                        if t == "hello"
                )
        ));
        assert_eq!(result.model, "gpt-4o");
    }

    #[test]
    fn convert_items_with_user_message() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": [
                {"role": "user", "content": "what is 1+1?"}
            ]
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(
            &result.messages[0],
            ChatCompletionRequestMessage::User(_)
        ));
    }

    #[test]
    fn convert_instructions_become_developer_message() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": "hi",
            "instructions": "You are a helpful assistant."
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.messages.len(), 2);
        assert!(matches!(
            &result.messages[0],
            ChatCompletionRequestMessage::Developer(d)
                if matches!(
                    &d.content,
                    ChatCompletionRequestDeveloperMessageContent::Text(t)
                        if t == "You are a helpful assistant."
                )
        ));
        assert!(matches!(
            &result.messages[1],
            ChatCompletionRequestMessage::User(_)
        ));
    }

    #[test]
    fn convert_function_call_aggregates_to_assistant() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": [
                {"role": "user", "content": "read file"},
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"foo.rs\"}"
                },
                {
                    "type": "function_call",
                    "call_id": "call_2",
                    "name": "write_file",
                    "arguments": "{\"path\":\"bar.rs\"}"
                }
            ]
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.messages.len(), 2);
        if let ChatCompletionRequestMessage::Assistant(asst) =
            &result.messages[1]
        {
            let tc = asst.tool_calls.as_ref().unwrap();
            assert_eq!(tc.len(), 2);
            assert_eq!(tc[0].id, "call_1");
            assert_eq!(tc[0].function.name, "read_file");
            assert_eq!(tc[1].id, "call_2");
            assert_eq!(tc[1].function.name, "write_file");
        } else {
            panic!("expected assistant message with tool calls");
        }
    }

    #[test]
    fn convert_function_call_output_becomes_tool_message() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": [
                {"role": "user", "content": "do it"},
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "file contents here"
                }
            ]
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.messages.len(), 3);
        if let ChatCompletionRequestMessage::Tool(tool) = &result.messages[2] {
            assert_eq!(tool.tool_call_id, "call_1");
            assert!(matches!(
                &tool.content,
                ChatCompletionRequestToolMessageContent::Text(t)
                    if t == "file contents here"
            ));
        } else {
            panic!("expected tool message");
        }
    }

    #[test]
    fn convert_tools_filters_non_function() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": "hello",
            "tools": [
                {
                    "type": "function",
                    "name": "get_weather",
                    "parameters": {"type": "object"},
                    "strict": false,
                    "description": "Get weather"
                },
                {
                    "type": "web_search_preview"
                }
            ]
        }));
        let result = convert(req).unwrap();
        let tools = result.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.as_ref().unwrap().name, "get_weather");
    }

    #[test]
    fn convert_tool_choice_modes() {
        for (mode, expected) in [
            ("none", ChatCompletionToolChoiceOption::None),
            ("auto", ChatCompletionToolChoiceOption::Auto),
            ("required", ChatCompletionToolChoiceOption::Required),
        ] {
            let req = make_request(json!({
                "model": "gpt-4o",
                "input": "hello",
                "tools": [{
                    "type": "function",
                    "name": "f",
                    "parameters": {},
                    "strict": false
                }],
                "tool_choice": mode
            }));
            let result = convert(req).unwrap();
            assert_eq!(result.tool_choice.unwrap(), expected, "mode={mode}");
        }
    }

    #[test]
    fn convert_response_format_json_schema() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": "hello",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "my_schema",
                    "schema": {"type": "object"}
                }
            }
        }));
        let result = convert(req).unwrap();
        assert!(matches!(
            result.response_format,
            Some(ResponseFormat::JsonSchema { .. })
        ));
    }

    #[test]
    fn convert_multi_turn_conversation() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.messages.len(), 4);
        assert!(matches!(
            &result.messages[0],
            ChatCompletionRequestMessage::System(_)
        ));
        assert!(matches!(
            &result.messages[1],
            ChatCompletionRequestMessage::User(_)
        ));
        assert!(matches!(
            &result.messages[2],
            ChatCompletionRequestMessage::Assistant(_)
        ));
        assert!(matches!(
            &result.messages[3],
            ChatCompletionRequestMessage::User(_)
        ));
    }

    #[test]
    fn convert_tool_choice_function_becomes_named() {
        let request: CreateResponse = serde_json::from_value(json!({
            "model": "test-model",
            "input": "hello",
            "tool_choice": {"type": "function", "name": "read_file"},
            "tools": [{"type": "function", "name": "read_file", "parameters": {}}]
        }))
        .unwrap();
        let result = convert(request).unwrap();
        assert!(matches!(
            result.tool_choice,
            Some(ChatCompletionToolChoiceOption::Named(_))
        ));
    }

    #[test]
    fn convert_reasoning_effort_mapped() {
        let request: CreateResponse = serde_json::from_value(json!({
            "model": "test-model",
            "input": "hello",
            "reasoning": {"effort": "high"}
        }))
        .unwrap();
        let result = convert(request).unwrap();
        assert_eq!(result.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn convert_preserves_optional_fields() {
        let req = make_request(json!({
            "model": "gpt-4o",
            "input": "hello",
            "temperature": 0.7,
            "top_p": 0.9,
            "max_output_tokens": 1024,
            "store": true,
            "user": "user-123",
            "parallel_tool_calls": true
        }));
        let result = convert(req).unwrap();
        assert_eq!(result.temperature, Some(0.7));
        assert_eq!(result.top_p, Some(0.9));
        assert_eq!(result.max_completion_tokens, Some(1024));
        assert_eq!(result.store, Some(true));
        assert_eq!(result.user.as_deref(), Some("user-123"));
        assert_eq!(result.parallel_tool_calls, Some(true));
    }

    /// Cursor-style Responses bodies must survive serde round-trips on the
    /// passthrough path (identity mapping) without dropping fields.
    #[test]
    fn create_response_serde_preserves_cursor_forwarding_fields() {
        let json = json!({
            "model": "openai/gpt-5.5",
            "input": [{"type": "message", "role": "user", "content": "test"}],
            "include": ["reasoning.encrypted_content"],
            "reasoning": {"effort": "medium", "summary": "detailed"},
            "text": {"format": {"type": "text"}, "verbosity": "medium"},
            "max_output_tokens": 32000,
            "max_tool_calls": null,
            "stream": true,
            "store": false,
            "tool_choice": "auto",
            "tools": [],
            "prompt_cache_key": "pcache-123",
            "prompt_cache_retention": "24h"
        });
        let request: CreateResponse = serde_json::from_value(json.clone())
            .expect("deserialize CreateResponse");
        let round =
            serde_json::to_value(&request).expect("serialize CreateResponse");

        assert_eq!(round["include"], json["include"]);
        assert_eq!(round["reasoning"]["effort"], "medium");
        assert_eq!(round["reasoning"]["summary"], "detailed");
        assert_eq!(round["text"]["format"]["type"], "text");
        assert_eq!(round["text"]["verbosity"], "medium");
        assert_eq!(round["max_output_tokens"], 32000);
        assert!(round["max_tool_calls"].is_null());
        assert_eq!(round["stream"], true);
        assert_eq!(round["store"], false);
        assert_eq!(round["prompt_cache_key"], "pcache-123");
        assert_eq!(round["prompt_cache_retention"], "24h");
    }
}
