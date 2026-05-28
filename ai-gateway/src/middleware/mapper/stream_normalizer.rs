use async_openai::types::{
    ChatChoiceStream, ChatCompletionMessageToolCallChunk,
    ChatCompletionStreamResponseDelta, ChatCompletionToolType, CompletionUsage,
    CreateChatCompletionStreamResponse, FinishReason, FunctionCallStream, Role,
};

pub const OPENAI_CHAT_COMPLETION_CHUNK_OBJECT: &str = "chat.completion.chunk";
const DEFAULT_CREATED_TIMESTAMP: u32 = 0;

#[must_use]
pub fn build_stream_usage(
    prompt_tokens: u32,
    completion_tokens: u32,
) -> CompletionUsage {
    CompletionUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
        prompt_tokens_details: None,
        completion_tokens_details: None,
    }
}

#[allow(deprecated)]
fn build_delta(
    role: Option<Role>,
    content: Option<String>,
    tool_calls: Option<Vec<ChatCompletionMessageToolCallChunk>>,
    refusal: Option<String>,
) -> ChatCompletionStreamResponseDelta {
    ChatCompletionStreamResponseDelta {
        role,
        content,
        tool_calls,
        refusal,
        function_call: None,
        reasoning_content: None,
    }
}

#[must_use]
pub fn build_reasoning_choice(
    index: u32,
    reasoning_content: String,
) -> ChatChoiceStream {
    ChatChoiceStream {
        index,
        delta: ChatCompletionStreamResponseDelta {
            reasoning_content: Some(reasoning_content),
            ..build_delta(None, None, None, None)
        },
        finish_reason: None,
        logprobs: None,
    }
}

#[must_use]
pub fn build_role_choice(index: u32, role: Role) -> ChatChoiceStream {
    ChatChoiceStream {
        index,
        delta: build_delta(Some(role), None, None, None),
        finish_reason: None,
        logprobs: None,
    }
}

#[must_use]
pub fn build_text_choice(index: u32, content: String) -> ChatChoiceStream {
    ChatChoiceStream {
        index,
        delta: build_delta(None, Some(content), None, None),
        finish_reason: None,
        logprobs: None,
    }
}

#[must_use]
pub fn build_tool_call_chunk(
    index: u32,
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
) -> ChatCompletionMessageToolCallChunk {
    ChatCompletionMessageToolCallChunk {
        index,
        id,
        r#type: Some(ChatCompletionToolType::Function),
        function: Some(FunctionCallStream { name, arguments }),
    }
}

#[must_use]
pub fn build_tool_choice(
    index: u32,
    tool_call: ChatCompletionMessageToolCallChunk,
) -> ChatChoiceStream {
    ChatChoiceStream {
        index,
        delta: build_delta(None, None, Some(vec![tool_call]), None),
        finish_reason: None,
        logprobs: None,
    }
}

#[must_use]
pub fn build_finish_choice(
    finish_reason: Option<FinishReason>,
    _usage: CompletionUsage,
    refusal: Option<String>,
) -> ChatChoiceStream {
    ChatChoiceStream {
        index: 0,
        delta: build_delta(None, None, None, refusal),
        finish_reason,
        logprobs: None,
    }
}

#[must_use]
pub fn build_stream_response(
    id: String,
    model: String,
    choices: Vec<ChatChoiceStream>,
    usage: Option<CompletionUsage>,
) -> CreateChatCompletionStreamResponse {
    CreateChatCompletionStreamResponse {
        id,
        choices,
        created: DEFAULT_CREATED_TIMESTAMP,
        model,
        object: OPENAI_CHAT_COMPLETION_CHUNK_OBJECT.to_string(),
        system_fingerprint: None,
        service_tier: None,
        usage,
    }
}

#[cfg(test)]
mod tests {
    use async_openai::types::{
        ChatCompletionStreamResponseDelta, FinishReason, Role,
    };

    use super::{
        OPENAI_CHAT_COMPLETION_CHUNK_OBJECT, build_finish_choice,
        build_reasoning_choice, build_role_choice, build_stream_response,
        build_stream_usage, build_text_choice, build_tool_call_chunk,
        build_tool_choice,
    };

    #[test]
    fn build_text_choice_sets_text_delta() {
        let choice = build_text_choice(2, "hello".to_string());

        assert_eq!(choice.index, 2);
        assert_eq!(choice.delta.role, None);
        assert_eq!(choice.delta.content.as_deref(), Some("hello"));
        assert!(choice.delta.tool_calls.is_none());
    }

    #[test]
    fn build_tool_choice_sets_tool_call_delta() {
        let tool_call = build_tool_call_chunk(
            3,
            Some("call_1".to_string()),
            Some("lookup_weather".to_string()),
            Some("{\"city\":\"Paris\"}".to_string()),
        );
        let choice = build_tool_choice(0, tool_call);

        let delta_tool_call =
            &choice.delta.tool_calls.as_ref().expect("tool calls")[0];
        assert_eq!(choice.index, 0);
        assert_eq!(delta_tool_call.index, 3);
        assert_eq!(delta_tool_call.id.as_deref(), Some("call_1"));
        assert_eq!(
            delta_tool_call
                .function
                .as_ref()
                .and_then(|function| function.name.as_deref()),
            Some("lookup_weather")
        );
    }

    #[test]
    fn build_stream_response_sets_object_and_usage() {
        let response = build_stream_response(
            "stream_1".to_string(),
            "model_1".to_string(),
            vec![build_finish_choice(
                Some(FinishReason::Stop),
                build_stream_usage(10, 3),
                None,
            )],
            Some(build_stream_usage(10, 3)),
        );

        assert_eq!(response.object, OPENAI_CHAT_COMPLETION_CHUNK_OBJECT);
        assert_eq!(response.id, "stream_1");
        assert_eq!(response.model, "model_1");
        assert_eq!(response.usage.as_ref().expect("usage").total_tokens, 13);
        assert_eq!(response.choices[0].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn build_finish_choice_sets_refusal_without_content() {
        let choice = build_finish_choice(
            Some(FinishReason::ContentFilter),
            build_stream_usage(4, 0),
            Some("blocked".to_string()),
        );

        assert_eq!(choice.delta.role, None);
        assert_eq!(choice.delta.content, None);
        assert_eq!(choice.delta.refusal.as_deref(), Some("blocked"));
        assert_eq!(choice.finish_reason, Some(FinishReason::ContentFilter));
    }

    #[test]
    fn build_role_choice_sets_role_delta() {
        let response = build_stream_response(
            "stream_2".to_string(),
            "model_2".to_string(),
            vec![build_role_choice(0, Role::Assistant)],
            None,
        );

        assert_eq!(response.choices[0].delta.role, Some(Role::Assistant));
        assert_eq!(response.choices[0].delta.content, None);
    }

    #[test]
    fn build_reasoning_choice_sets_reasoning_content() {
        let choice =
            build_reasoning_choice(0, "thinking step by step...".to_string());

        assert_eq!(choice.index, 0);
        assert_eq!(choice.delta.content, None);
        assert_eq!(
            choice.delta.reasoning_content.as_deref(),
            Some("thinking step by step...")
        );
        assert!(choice.finish_reason.is_none());
    }

    #[test]
    fn reasoning_content_round_trips_in_stream_delta() {
        let delta = ChatCompletionStreamResponseDelta {
            content: None,
            #[allow(deprecated)]
            function_call: None,
            tool_calls: None,
            role: Some(Role::Assistant),
            refusal: None,
            reasoning_content: Some("thinking step by step...".to_string()),
        };
        let json = serde_json::to_string(&delta).unwrap();
        assert!(json.contains("reasoning_content"));
        let parsed: ChatCompletionStreamResponseDelta =
            serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.reasoning_content,
            Some("thinking step by step...".to_string())
        );
    }

    #[test]
    fn reasoning_content_none_is_omitted_in_json() {
        let delta = ChatCompletionStreamResponseDelta {
            content: Some("hello".to_string()),
            #[allow(deprecated)]
            function_call: None,
            tool_calls: None,
            role: None,
            refusal: None,
            reasoning_content: None,
        };
        let json = serde_json::to_string(&delta).unwrap();
        assert!(!json.contains("reasoning_content"));
    }
}
