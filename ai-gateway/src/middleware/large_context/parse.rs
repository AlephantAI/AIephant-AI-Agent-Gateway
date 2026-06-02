use bytes::Bytes;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ChatMessageTextRef {
    pub index: usize,
    pub role: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatCompletionsPayload {
    pub raw: Value,
    pub model: Option<String>,
    pub messages: Vec<ChatMessageTextRef>,
    pub has_non_text_message_content: bool,
    pub tools: Option<Value>,
    pub requested_completion_tokens: Option<u32>,
}

fn trimmed_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn completion_budget_from_object(object: &serde_json::Map<String, Value>) -> Option<u32> {
    ["max_completion_tokens", "max_tokens", "max_output_tokens"]
        .into_iter()
        .find_map(|key| {
            object
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
        })
}

pub fn parse_chat_completions_payload(
    body: &[u8],
) -> Result<Option<ChatCompletionsPayload>, serde_json::Error> {
    let raw: Value = serde_json::from_slice(body)?;
    let Some(object) = raw.as_object() else {
        return Ok(None);
    };
    let Some(messages_value) = object.get("messages") else {
        return Ok(None);
    };
    let Some(messages_array) = messages_value.as_array() else {
        return Ok(None);
    };

    let mut messages = Vec::new();
    let mut has_non_text_message_content = false;
    for (index, message_value) in messages_array.iter().enumerate() {
        let Some(message_object) = message_value.as_object() else {
            has_non_text_message_content = true;
            continue;
        };
        let role = message_object.get("role").and_then(trimmed_string);
        match message_object.get("content") {
            Some(Value::String(content)) => messages.push(ChatMessageTextRef {
                index,
                role,
                content: content.clone(),
            }),
            Some(Value::Null) | None => {}
            Some(_) => has_non_text_message_content = true,
        }
    }

    let model = object.get("model").and_then(trimmed_string);
    let tools = object.get("tools").cloned();
    let requested_completion_tokens = completion_budget_from_object(object);

    Ok(Some(ChatCompletionsPayload {
        raw,
        model,
        messages,
        has_non_text_message_content,
        tools,
        requested_completion_tokens,
    }))
}

pub fn serialize_payload(value: &Value) -> Result<Bytes, serde_json::Error> {
    serde_json::to_vec(value).map(Bytes::from)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse_chat_completions_payload;

    #[test]
    fn parses_chat_completions_payload() {
        let body = serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "max_tokens": 512
        }))
        .unwrap();

        let payload = parse_chat_completions_payload(&body)
            .unwrap()
            .expect("payload should be recognized");
        assert_eq!(payload.model.as_deref(), Some("openai/gpt-4o-mini"));
        assert_eq!(payload.messages.len(), 1);
        assert_eq!(payload.messages[0].content, "hello");
        assert_eq!(payload.requested_completion_tokens, Some(512));
    }

    #[test]
    fn returns_none_for_non_chat_object() {
        let body = serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini"
        }))
        .unwrap();

        assert!(parse_chat_completions_payload(&body).unwrap().is_none());
    }

    #[test]
    fn marks_non_text_message_content() {
        let body = serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": "hello"
                        }
                    ]
                }
            ]
        }))
        .unwrap();

        let payload = parse_chat_completions_payload(&body)
            .unwrap()
            .expect("payload should be recognized");
        assert!(payload.has_non_text_message_content);
    }

    #[test]
    fn completion_budget_prefers_max_completion_tokens() {
        let body = serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "hello"
                }
            ],
            "max_output_tokens": 128,
            "max_tokens": 256,
            "max_completion_tokens": 512
        }))
        .unwrap();

        let payload = parse_chat_completions_payload(&body)
            .unwrap()
            .expect("payload should be recognized");
        assert_eq!(payload.requested_completion_tokens, Some(512));
    }
}
