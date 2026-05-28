use async_openai::types::{
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    CreateChatCompletionRequest, CreateChatCompletionRequestArgs, ReasoningEffort,
};

#[tokio::test]
async fn chat_types_serde() {
    let request: CreateChatCompletionRequest = CreateChatCompletionRequestArgs::default()
        .messages([
            ChatCompletionRequestSystemMessageArgs::default()
                .content("your are a calculator")
                .build()
                .unwrap()
                .into(),
            ChatCompletionRequestUserMessageArgs::default()
                .content("what is the result of 1+1")
                .build()
                .unwrap()
                .into(),
        ])
        .build()
        .unwrap();
    // serialize the request
    let serialized = serde_json::to_string(&request).unwrap();
    // deserialize the request
    let deserialized: CreateChatCompletionRequest = serde_json::from_str(&serialized).unwrap();
    assert_eq!(request, deserialized);
}

#[test]
fn reasoning_effort_deserializes_openai_none_and_xhigh() {
    let cases = [
        ("\"none\"", ReasoningEffort::None),
        ("\"low\"", ReasoningEffort::Low),
        ("\"medium\"", ReasoningEffort::Medium),
        ("\"high\"", ReasoningEffort::High),
        ("\"xhigh\"", ReasoningEffort::XHigh),
    ];
    for (json, expected) in cases {
        let effort: ReasoningEffort = serde_json::from_str(json).unwrap();
        assert_eq!(effort, expected);
        let round_trip = serde_json::to_string(&effort).unwrap();
        assert_eq!(round_trip, json);
    }
}

#[test]
fn responses_reasoning_config_deserializes_effort_none() {
    use async_openai::types::responses::ReasoningConfig;

    let config: ReasoningConfig =
        serde_json::from_str(r#"{"effort":"none","summary":null}"#).unwrap();
    assert_eq!(config.effort, Some(ReasoningEffort::None));
    assert!(config.summary.is_none());
}
