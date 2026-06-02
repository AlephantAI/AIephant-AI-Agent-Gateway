use async_openai::types::{
    ChatChoice, ChatCompletionMessageToolCall, ChatCompletionResponseMessage, CompletionUsage,
    CreateChatCompletionResponse, FinishReason, Role,
};

pub const OPENAI_CHAT_COMPLETION_OBJECT: &str = "chat.completion";

#[must_use]
pub fn build_usage(
    prompt_tokens: u32,
    completion_tokens: u32,
    cached_tokens: Option<u32>,
) -> CompletionUsage {
    CompletionUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
        prompt_tokens_details: Some(async_openai::types::PromptTokensDetails {
            audio_tokens: None,
            cached_tokens,
            cache_write_tokens: None,
            cache_write_details: None,
        }),
        completion_tokens_details: None,
    }
}

#[must_use]
pub fn normalize_tool_calls(
    tool_calls: Vec<ChatCompletionMessageToolCall>,
) -> Option<Vec<ChatCompletionMessageToolCall>> {
    if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    }
}

#[allow(deprecated)]
#[must_use]
pub fn build_assistant_message(
    content: Option<String>,
    tool_calls: Option<Vec<ChatCompletionMessageToolCall>>,
) -> ChatCompletionResponseMessage {
    ChatCompletionResponseMessage {
        content,
        refusal: None,
        tool_calls,
        role: Role::Assistant,
        function_call: None,
        audio: None,
        reasoning_content: None,
    }
}

#[must_use]
pub fn build_chat_response(
    id: String,
    model: String,
    content: Option<String>,
    tool_calls: Option<Vec<ChatCompletionMessageToolCall>>,
    finish_reason: Option<FinishReason>,
    usage: CompletionUsage,
) -> CreateChatCompletionResponse {
    let message = build_assistant_message(content, tool_calls);
    let choice = ChatChoice {
        index: 0,
        message,
        finish_reason,
        logprobs: None,
    };

    CreateChatCompletionResponse {
        choices: vec![choice],
        id,
        created: 0,
        model,
        object: OPENAI_CHAT_COMPLETION_OBJECT.to_string(),
        usage: Some(usage),
        service_tier: None,
        system_fingerprint: None,
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::{
        ChatCompletionMessageToolCall, ChatCompletionToolType, FinishReason, FunctionCall,
    };

    use super::{build_chat_response, build_usage, normalize_tool_calls};

    #[test]
    fn normalize_tool_calls_returns_none_for_empty_vec() {
        assert!(normalize_tool_calls(Vec::new()).is_none());
    }

    #[test]
    fn build_chat_response_sets_finish_reason_and_usage() {
        let tool_calls = vec![ChatCompletionMessageToolCall {
            id: "call_1".to_string(),
            r#type: ChatCompletionToolType::Function,
            function: FunctionCall {
                name: "lookup_weather".to_string(),
                arguments: "{\"city\":\"Paris\"}".to_string(),
            },
        }];
        let response = build_chat_response(
            "resp_1".to_string(),
            "model_1".to_string(),
            None,
            normalize_tool_calls(tool_calls),
            Some(FinishReason::ToolCalls),
            build_usage(12, 6, None),
        );

        assert_eq!(response.object, "chat.completion");
        assert_eq!(
            response.choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(response.usage.as_ref().expect("usage").total_tokens, 18);
        assert_eq!(
            response.choices[0]
                .message
                .tool_calls
                .as_ref()
                .expect("tool calls")[0]
                .function
                .name,
            "lookup_weather"
        );
    }
}
