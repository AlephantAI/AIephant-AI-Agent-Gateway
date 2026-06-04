//! Cursor + OpenAI-compatible upstream: translate `/v1/responses` requests to
//! Chat Completions for providers such as OpenRouter, then map Chat SSE / JSON
//! back to the Responses API wire shape expected by Cursor.

use std::collections::HashMap;

use async_openai::types::{
    ChatCompletionNamedToolChoice, ChatCompletionRequestAssistantMessage,
    ChatCompletionRequestDeveloperMessage, ChatCompletionRequestDeveloperMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionToolChoiceOption,
    ChatCompletionToolType, CreateChatCompletionRequest, CreateChatCompletionResponse,
    CreateChatCompletionStreamResponse, FinishReason, FunctionObject,
    responses::{
        ContentType, CreateResponse, Input, InputContent, InputItem, InputMessage,
        Role as RespRole, ToolChoice, ToolDefinition,
    },
};
use bytes::{BufMut, Bytes, BytesMut};
use futures::{StreamExt, TryStreamExt, stream};
use http_body_util::BodyExt;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    endpoints::{ApiEndpoint, openai::OpenAI},
    error::{
        api::ApiError, internal::InternalError, invalid_req::InvalidRequestError,
        mapper::MapperError, stream::StreamError,
    },
    ide_adapation::responses_ingress_normalize,
    types::{
        extensions::{ClientResponseSemantic, LoggerResponseWireSemantic, MapperContext},
        model_id::ModelId,
        response::Response,
    },
};

fn put_sse_data_record(buf: &mut BytesMut, payload: &[u8]) {
    buf.put_slice(b"data: ");
    buf.put_slice(payload);
    buf.put_slice(b"\n\n");
}

fn put_sse_data_json<T: Serialize>(buf: &mut BytesMut, val: &T) -> Result<(), ApiError> {
    let json = serde_json::to_vec(val).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<T>(),
            error,
        })
    })?;
    put_sse_data_record(buf, &json);
    Ok(())
}

fn text_from_input_content(content: &InputContent) -> Result<String, MapperError> {
    match content {
        InputContent::TextInput(s) => Ok(s.clone()),
        InputContent::InputItemContentList(parts) => {
            let mut acc = String::new();
            for p in parts {
                if let ContentType::InputText(it) = p {
                    let v = serde_json::to_value(it).map_err(MapperError::SerdeError)?;
                    if let Some(s) = v.get("text").and_then(|x| x.as_str()) {
                        acc.push_str(s);
                    }
                }
            }
            Ok(acc)
        }
    }
}

fn input_message_to_chat(m: &InputMessage) -> Result<ChatCompletionRequestMessage, MapperError> {
    let text = text_from_input_content(&m.content)?;
    Ok(match m.role {
        RespRole::User => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text(text),
            name: None,
        }),
        RespRole::Assistant => {
            ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
                content: Some(
                    async_openai::types::ChatCompletionRequestAssistantMessageContent::Text(text),
                ),
                refusal: None,
                name: None,
                audio: None,
                tool_calls: None,
                #[allow(deprecated)]
                function_call: None,
            })
        }
        RespRole::System => {
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(text),
                name: None,
            })
        }
        RespRole::Developer => {
            ChatCompletionRequestMessage::Developer(ChatCompletionRequestDeveloperMessage {
                content: ChatCompletionRequestDeveloperMessageContent::Text(text),
                name: None,
            })
        }
    })
}

fn input_item_to_chat_message(
    item: &InputItem,
) -> Result<Option<ChatCompletionRequestMessage>, MapperError> {
    match item {
        InputItem::Message(m) => Ok(Some(input_message_to_chat(m)?)),
        InputItem::Custom(v) => {
            let m: InputMessage =
                serde_json::from_value(v.clone()).map_err(MapperError::SerdeError)?;
            Ok(Some(input_message_to_chat(&m)?))
        }
    }
}

fn input_to_messages(
    input: &Input,
    instructions: Option<&str>,
) -> Result<Vec<ChatCompletionRequestMessage>, MapperError> {
    let mut out = Vec::new();
    if let Some(inst) = instructions.map(str::trim).filter(|s| !s.is_empty()) {
        out.push(ChatCompletionRequestMessage::System(
            ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(inst.to_string()),
                name: None,
            },
        ));
    }
    match input {
        Input::Text(s) => {
            if !s.trim().is_empty() {
                out.push(ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage {
                        content: ChatCompletionRequestUserMessageContent::Text(s.clone()),
                        name: None,
                    },
                ));
            }
        }
        Input::Items(items) => {
            for it in items {
                if let Some(m) = input_item_to_chat_message(it)? {
                    out.push(m);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(MapperError::InvalidRequest);
    }
    Ok(out)
}

fn map_tool_definition(def: ToolDefinition) -> Option<ChatCompletionTool> {
    match def {
        ToolDefinition::Function(f) => Some(ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: Some(FunctionObject {
                name: f.name,
                description: f.description,
                parameters: Some(f.parameters),
                strict: Some(f.strict),
            }),
            extra: HashMap::new(),
        }),
        ToolDefinition::Custom(v) => serde_json::from_value(v).ok(),
        _ => None,
    }
}

fn map_tool_choice(tc: &ToolChoice) -> Option<ChatCompletionToolChoiceOption> {
    match tc {
        ToolChoice::Mode(m) => Some(match m {
            async_openai::types::responses::ToolChoiceMode::None => {
                ChatCompletionToolChoiceOption::None
            }
            async_openai::types::responses::ToolChoiceMode::Auto => {
                ChatCompletionToolChoiceOption::Auto
            }
            async_openai::types::responses::ToolChoiceMode::Required => {
                ChatCompletionToolChoiceOption::Required
            }
        }),
        ToolChoice::Function { name } => Some(ChatCompletionToolChoiceOption::Named(
            ChatCompletionNamedToolChoice {
                r#type: ChatCompletionToolType::Function,
                function: async_openai::types::FunctionName { name: name.clone() },
            },
        )),
        ToolChoice::Hosted { .. } => None,
    }
}

/// Build a [`CreateChatCompletionRequest`] from a Responses [`CreateResponse`].
pub(crate) fn create_response_to_chat_request(
    mut r: CreateResponse,
) -> Result<CreateChatCompletionRequest, MapperError> {
    let messages = input_to_messages(&r.input, r.instructions.as_deref())?;
    let tools: Option<Vec<ChatCompletionTool>> = r
        .tools
        .as_ref()
        .map(|defs| {
            defs.iter()
                .filter_map(|d| map_tool_definition(d.clone()))
                .collect()
        })
        .filter(|v: &Vec<ChatCompletionTool>| !v.is_empty());
    let tool_choice = r.tool_choice.as_ref().and_then(map_tool_choice);
    let response_format = r.text.as_ref().map(|t| match t.format {
        async_openai::types::responses::TextResponseFormat::Text => {
            async_openai::types::ResponseFormat::Text
        }
        async_openai::types::responses::TextResponseFormat::JsonObject => {
            async_openai::types::ResponseFormat::JsonObject
        }
        async_openai::types::responses::TextResponseFormat::JsonSchema(ref js) => {
            async_openai::types::ResponseFormat::JsonSchema {
                json_schema: async_openai::types::ResponseFormatJsonSchema {
                    description: js.description.clone(),
                    name: js.name.clone(),
                    schema: js.schema.clone(),
                    strict: js.strict,
                },
            }
        }
    });
    let reasoning_effort = r.reasoning.as_ref().and_then(|x| x.effort.clone());
    Ok(CreateChatCompletionRequest {
        model: std::mem::take(&mut r.model),
        messages,
        store: r.store,
        reasoning_effort,
        metadata: None,
        frequency_penalty: None,
        logit_bias: None,
        logprobs: None,
        top_logprobs: None,
        #[allow(deprecated)]
        max_tokens: None,
        max_completion_tokens: r.max_output_tokens,
        n: None,
        modalities: None,
        prediction: None,
        audio: None,
        presence_penalty: None,
        response_format,
        seed: None,
        service_tier: None,
        stop: None,
        stream: r.stream,
        stream_options: None,
        temperature: r.temperature,
        tool_choice,
        tools,
        top_p: r.top_p,
        web_search_options: None,
        user: r.user.clone(),
        parallel_tool_calls: r.parallel_tool_calls,
        #[allow(deprecated)]
        functions: None,
        #[allow(deprecated)]
        function_call: None,
        extra: HashMap::new(),
    })
}

#[derive(Debug, Default)]
struct ToolStreamTrack {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
    done: bool,
}

#[derive(Debug)]
pub(crate) struct CursorChatToResponsesStreamState {
    seq: u64,
    started: bool,
    message_item_id: String,
    /// Codex / Responses clients require `output_item.added` +
    /// `content_part.added` before `output_text.delta` references
    /// `message_item_id`.
    message_streaming_shell: bool,
    reasoning_item_id: String,
    reasoning_streaming_shell: bool,
    resp_id: Option<String>,
    model: Option<String>,
    created: Option<u64>,
    tools: HashMap<u32, ToolStreamTrack>,
    last_usage: Option<async_openai::types::CompletionUsage>,
    last_finish: Option<FinishReason>,
    emit_tool_done_events: bool,
}

fn delta_reasoning_text(raw: &Value) -> Option<String> {
    let delta = raw
        .get("choices")
        .and_then(|c| c.as_array().and_then(|a| a.first()))
        .and_then(|c| c.get("delta"))?;
    for key in ["reasoning_content", "reasoning"] {
        if let Some(s) = delta.get(key).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn nonstream_choice_reasoning_text(raw: &Value, index: usize) -> Option<String> {
    let message = raw
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|choices| choices.get(index))
        .and_then(|choice| choice.get("message"))?;
    for key in ["reasoning_content", "reasoning"] {
        if let Some(s) = message.get(key).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn chat_reasoning_tokens(raw: &Value) -> Option<u64> {
    raw.get("usage")
        .and_then(|u| u.get("completion_tokens_details"))
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
}

impl CursorChatToResponsesStreamState {
    fn with_tool_done_events(emit_tool_done_events: bool) -> Self {
        Self {
            emit_tool_done_events,
            ..Self::default()
        }
    }

    fn next_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    fn ensure_header(
        &mut self,
        chunk: &CreateChatCompletionStreamResponse,
    ) -> Result<BytesMut, ApiError> {
        let mut buf = BytesMut::new();
        if self.started {
            return Ok(buf);
        }
        self.started = true;
        if self.resp_id.is_none() {
            let id = if chunk.id.starts_with("resp_") {
                chunk.id.clone()
            } else {
                format!(
                    "resp_{}",
                    chunk.id.strip_prefix("chatcmpl-").unwrap_or(&chunk.id)
                )
            };
            self.resp_id = Some(id);
        }
        if self.model.is_none() {
            self.model = Some(chunk.model.clone());
        }
        if self.created.is_none() {
            self.created = Some(u64::from(chunk.created));
        }
        let rid = self.resp_id.clone().unwrap_or_default();
        let model = self.model.clone().unwrap_or_default();
        let created = self.created.unwrap_or(0);
        put_sse_data_json(
            &mut buf,
            &json!({
                "type": "response.created",
                "response": {
                    "id": rid,
                    "object": "response",
                    "created_at": created,
                    "model": model,
                    "status": "in_progress"
                }
            }),
        )?;
        put_sse_data_json(
            &mut buf,
            &json!({
                "type": "response.in_progress",
                "response": {
                    "id": rid,
                    "object": "response",
                    "created_at": created,
                    "model": model,
                    "status": "in_progress"
                }
            }),
        )?;
        Ok(buf)
    }

    fn ensure_message_streaming_shell(&mut self, buf: &mut BytesMut) -> Result<(), ApiError> {
        if self.message_streaming_shell {
            return Ok(());
        }
        self.message_streaming_shell = true;
        put_sse_data_json(
            buf,
            &json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": self.message_item_id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": []
                }
            }),
        )?;
        put_sse_data_json(
            buf,
            &json!({
                "type": "response.content_part.added",
                "item_id": self.message_item_id,
                "output_index": 0,
                "content_index": 0,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": []
                }
            }),
        )?;
        Ok(())
    }

    fn ensure_reasoning_streaming_shell(&mut self, buf: &mut BytesMut) -> Result<(), ApiError> {
        if self.reasoning_streaming_shell {
            return Ok(());
        }
        self.reasoning_streaming_shell = true;
        put_sse_data_json(
            buf,
            &json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": {
                    "id": self.reasoning_item_id,
                    "type": "reasoning",
                    "status": "in_progress",
                    "summary": []
                }
            }),
        )?;
        Ok(())
    }

    pub(crate) fn process_upstream_chat_chunk(
        &mut self,
        chunk: &CreateChatCompletionStreamResponse,
        raw: &Value,
    ) -> Result<Bytes, ApiError> {
        let mut buf = self.ensure_header(chunk)?;

        if let Some(u) = &chunk.usage {
            self.last_usage = Some(u.clone());
        }
        let choice0 = chunk.choices.first();
        if let Some(ch) = choice0 {
            if let Some(fr) = ch.finish_reason {
                self.last_finish = Some(fr);
            }
            let d = &ch.delta;
            if let Some(r) = &d.refusal {
                if !r.is_empty() {
                    put_sse_data_json(
                        &mut buf,
                        &json!({
                            "type": "response.refusal.delta",
                            "sequence_number": self.next_seq(),
                            "delta": r
                        }),
                    )?;
                }
            }
            if let Some(tc) = &d.tool_calls {
                for t in tc {
                    let tr = self
                        .tools
                        .entry(t.index)
                        .or_insert_with(|| ToolStreamTrack {
                            item_id: format!("item_{}", Uuid::new_v4().simple()),
                            call_id: t
                                .id
                                .clone()
                                .unwrap_or_else(|| format!("call_{}", Uuid::new_v4().simple())),
                            name: String::new(),
                            arguments: String::new(),
                            added: false,
                            done: false,
                        });
                    if let Some(id) = &t.id {
                        tr.call_id.clone_from(id);
                    }
                    if let Some(f) = &t.function {
                        if let Some(n) = &f.name {
                            tr.name.clone_from(n);
                        }
                    }
                    if !tr.added && (!tr.name.is_empty() || t.function.is_some()) {
                        let name = tr.name.clone();
                        if !name.is_empty() {
                            put_sse_data_json(
                                &mut buf,
                                &json!({
                                    "type": "response.output_item.added",
                                    "output_index": 0,
                                    "item": {
                                        "id": tr.item_id,
                                        "type": "function_call",
                                        "call_id": tr.call_id,
                                        "name": name,
                                        "arguments": ""
                                    }
                                }),
                            )?;
                            tr.added = true;
                        }
                    }
                    if let Some(f) = &t.function {
                        if let Some(arg) = &f.arguments {
                            if !arg.is_empty() {
                                tr.arguments.push_str(arg);
                                put_sse_data_json(
                                    &mut buf,
                                    &json!({
                                        "type": "response.function_call_arguments.delta",
                                        "item_id": tr.item_id,
                                        "output_index": 0,
                                        "delta": arg
                                    }),
                                )?;
                            }
                        }
                    }
                }
            }
            if let Some(c) = &d.content {
                if !c.is_empty() {
                    self.ensure_message_streaming_shell(&mut buf)?;
                    put_sse_data_json(
                        &mut buf,
                        &json!({
                            "type": "response.output_text.delta",
                            "item_id": self.message_item_id,
                            "output_index": 0,
                            "content_index": 0,
                            "delta": c,
                            "sequence_number": self.next_seq()
                        }),
                    )?;
                }
            }
            if let Some(reasoning) = delta_reasoning_text(raw) {
                self.ensure_reasoning_streaming_shell(&mut buf)?;
                put_sse_data_json(
                    &mut buf,
                    &json!({
                        "type": "response.reasoning_text.delta",
                        "item_id": self.reasoning_item_id,
                        "output_index": 0,
                        "content_index": 0,
                        "delta": reasoning,
                        "sequence_number": self.next_seq()
                    }),
                )?;
            }
        }
        if matches!(self.last_finish, Some(FinishReason::ToolCalls)) {
            self.emit_tool_done_events(&mut buf)?;
        }

        Ok(buf.freeze())
    }

    fn emit_tool_done_events(&mut self, buf: &mut BytesMut) -> Result<(), ApiError> {
        if !self.emit_tool_done_events {
            return Ok(());
        }
        for tr in self.tools.values_mut() {
            if !tr.added || tr.done {
                continue;
            }
            put_sse_data_json(
                buf,
                &json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": tr.item_id,
                    "output_index": 0,
                    "arguments": tr.arguments
                }),
            )?;
            put_sse_data_json(
                buf,
                &json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "id": tr.item_id,
                        "type": "function_call",
                        "call_id": tr.call_id,
                        "name": tr.name,
                        "arguments": tr.arguments,
                        "status": "completed"
                    }
                }),
            )?;
            tr.done = true;
        }
        Ok(())
    }

    pub(crate) fn finalize_stream(&mut self, _origin: &CreateResponse) -> Result<Bytes, ApiError> {
        let mut buf = BytesMut::new();
        let rid = self
            .resp_id
            .clone()
            .unwrap_or_else(|| format!("resp_{}", Uuid::new_v4().simple()));
        let model = self.model.clone().unwrap_or_else(|| "unknown".to_string());
        let usage_v = self.last_usage.as_ref().map(|u| {
            json!({
                "input_tokens": u.prompt_tokens,
                "output_tokens": u.completion_tokens,
                "total_tokens": u.total_tokens
            })
        });
        let incomplete = self
            .last_finish
            .map(|fr| {
                if matches!(fr, FinishReason::Length) {
                    Some(json!({"reason": "max_output_tokens"}))
                } else {
                    None
                }
            })
            .flatten();
        let status = if incomplete.is_some() {
            "incomplete"
        } else {
            "completed"
        };
        put_sse_data_json(
            &mut buf,
            &json!({
                "type": "response.completed",
                "response": {
                    "id": rid,
                    "object": "response",
                    "created_at": self.created.unwrap_or(0),
                    "model": model,
                    "status": status,
                    "usage": usage_v,
                    "incomplete_details": incomplete
                }
            }),
        )?;
        put_sse_data_record(&mut buf, b"[DONE]");
        buf.put_slice(b"\n\n");
        Ok(buf.freeze())
    }
}

impl Default for CursorChatToResponsesStreamState {
    fn default() -> Self {
        Self {
            seq: 0,
            started: false,
            message_item_id: format!("msg_{}", Uuid::new_v4().simple()),
            message_streaming_shell: false,
            reasoning_item_id: format!("rs_{}", Uuid::new_v4().simple()),
            reasoning_streaming_shell: false,
            resp_id: None,
            model: None,
            created: None,
            tools: HashMap::new(),
            last_usage: None,
            last_finish: None,
            emit_tool_done_events: false,
        }
    }
}

fn trim_sse_payload(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    let tail = &bytes[start..];
    let rel_end = tail.iter().rposition(|b| !b.is_ascii_whitespace());
    let end = start + rel_end.map(|i| i + 1).unwrap_or(0);
    &bytes[start..end]
}

pub(crate) async fn map_stream_response_chat_to_responses(
    parts: http::response::Parts,
    body: crate::types::body::Body,
    origin: CreateResponse,
    emit_tool_done_events: bool,
) -> Result<Response, ApiError> {
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(
        CursorChatToResponsesStreamState::with_tool_done_events(emit_tool_done_events),
    ));
    let origin = Arc::new(origin);
    let st_main = Arc::clone(&state);
    let mapped = futures::TryStreamExt::map_err(body.into_data_stream(), |e| {
        ApiError::StreamError(StreamError::BodyError(e))
    })
    .try_filter_map(move |bytes: Bytes| {
        let st = Arc::clone(&st_main);
        async move {
            let payload = trim_sse_payload(&bytes);
            if payload.is_empty() || payload == b"[DONE]" {
                return Ok(None);
            }
            let chunk: CreateChatCompletionStreamResponse = serde_json::from_slice(payload)
                .map_err(|e| {
                    ApiError::Internal(InternalError::Deserialize {
                        ty: "CreateChatCompletionStreamResponse",
                        error: e,
                    })
                })?;
            let raw: Value = serde_json::to_value(&chunk).map_err(|e| {
                ApiError::Internal(InternalError::MapperError(MapperError::SerdeError(e)))
            })?;
            let mut guard = st.lock().expect("cursor bridge mutex poisoned");
            let out = guard.process_upstream_chat_chunk(&chunk, &raw)?;
            if out.is_empty() {
                return Ok(None);
            }
            Ok(Some(out))
        }
    });
    let st_tail = Arc::clone(&state);
    let origin_tail = Arc::clone(&origin);
    let tail = stream::once(async move {
        let mut g = st_tail.lock().expect("cursor bridge mutex poisoned");
        let fin = g.finalize_stream(origin_tail.as_ref())?;
        Ok::<Bytes, ApiError>(fin)
    });
    let merged = mapped.chain(tail);
    let final_body = axum_core::body::Body::new(reqwest::Body::wrap_stream(merged));
    Ok(Response::from_parts(parts, final_body))
}

pub(crate) async fn map_json_response_chat_to_responses(
    parts: http::response::Parts,
    body: crate::types::body::Body,
    origin: &CreateResponse,
) -> Result<Response, ApiError> {
    let bytes = body
        .collect()
        .await
        .map_err(InternalError::CollectBodyError)?
        .to_bytes();
    if !parts.status.is_success() {
        return Ok(Response::from_parts(
            parts,
            axum_core::body::Body::from(bytes),
        ));
    }
    let raw: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ApiError::Internal(InternalError::Deserialize {
            ty: std::any::type_name::<Value>(),
            error,
        })
    })?;
    let chat: CreateChatCompletionResponse =
        serde_json::from_value(raw.clone()).map_err(|error| {
            ApiError::Internal(InternalError::Deserialize {
                ty: std::any::type_name::<CreateChatCompletionResponse>(),
                error,
            })
        })?;
    let mut output: Vec<Value> = Vec::new();
    for (idx, ch) in chat.choices.iter().enumerate() {
        if let Some(reasoning) = nonstream_choice_reasoning_text(&raw, idx) {
            output.push(json!({
                "id": format!("rs_{}", Uuid::new_v4().simple()),
                "type": "reasoning",
                "status": "completed",
                "summary": [{
                    "text": reasoning
                }]
            }));
        }
        if let Some(text) = ch.message.content.as_ref().filter(|s| !s.is_empty()) {
            output.push(json!({
                "id": format!("msg_{}", Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": text,
                    "annotations": []
                }]
            }));
        }
        if let Some(tools) = &ch.message.tool_calls {
            for tc in tools {
                output.push(json!({
                    "id": format!("fc_{}", Uuid::new_v4().simple()),
                    "type": "function_call",
                    "name": tc.function.name,
                    "call_id": tc.id,
                    "arguments": tc.function.arguments,
                    "status": "completed"
                }));
            }
        }
    }
    let mut status = "completed";
    let mut incomplete: Option<Value> = None;
    for ch in &chat.choices {
        if let Some(fr) = ch.finish_reason {
            if matches!(fr, FinishReason::Length) {
                status = "incomplete";
                incomplete = Some(json!({"reason": "max_output_tokens"}));
            }
        }
    }
    let rid = if chat.id.starts_with("resp_") {
        chat.id.clone()
    } else {
        format!(
            "resp_{}",
            chat.id.strip_prefix("chatcmpl-").unwrap_or(&chat.id)
        )
    };
    let usage = chat.usage.as_ref().map(|u| {
        let mut usage = json!({
            "input_tokens": u.prompt_tokens,
            "output_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens
        });
        if let Some(reasoning_tokens) = chat_reasoning_tokens(&raw) {
            usage["output_tokens_details"] = json!({
                "reasoning_tokens": reasoning_tokens
            });
        }
        usage
    });
    let body_out = json!({
        "id": rid,
        "object": "response",
        "created_at": u64::from(chat.created),
        "model": chat.model,
        "output": output,
        "status": status,
        "usage": usage,
        "incomplete_details": incomplete,
        "instructions": origin.instructions,
        "parallel_tool_calls": origin.parallel_tool_calls,
        "temperature": origin.temperature,
        "top_p": origin.top_p,
        "tool_choice": origin.tool_choice,
        "tools": origin.tools,
    });
    let out_bytes = serde_json::to_vec(&body_out).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: "response",
            error,
        })
    })?;
    Ok(Response::from_parts(
        parts,
        axum_core::body::Body::from(out_bytes),
    ))
}

/// Returns `(body, mapper_ctx, upstream_api_endpoint)` when the Cursor ↔
/// OpenAI-compatible **`POST /v1/responses`** → `chat/completions` bridge
/// applies.
///
/// The same translation runs for Cursor when the body is Responses-shaped on
/// **`/v1/chat/completions`** (unified redirect). Response mapping then uses
/// `MapperContext::unified_responses_bridge_chat_completions_sse` to return
/// Chat wire format to the client instead of rewriting upstream Chat into
/// Responses SSE.
pub(crate) fn try_map_responses_to_compatible_chat(
    converter_registry: &crate::middleware::mapper::registry::EndpointConverterRegistry,
    profile: crate::ide_adapation::client_profile::ClientProfile,
    source_endpoint: &ApiEndpoint,
    target_endpoint: &ApiEndpoint,
    body: &Bytes,
    unified_model_body_passthrough: bool,
) -> Result<Option<(Bytes, MapperContext, ApiEndpoint)>, ApiError> {
    use crate::ide_adapation::client_profile::ClientProfile as P;
    if !matches!(profile, P::CursorIde | P::CodexCli) {
        return Ok(None);
    }
    let should_bridge = matches!(source_endpoint, ApiEndpoint::OpenAI(OpenAI::Responses(_)))
        && matches!(
            target_endpoint,
            ApiEndpoint::OpenAICompatible {
                openai_endpoint: OpenAI::Responses(_),
                ..
            }
        );
    if !should_bridge {
        return Ok(None);
    }

    let ApiEndpoint::OpenAICompatible { provider, .. } = target_endpoint else {
        return Ok(None);
    };
    let upstream = ApiEndpoint::OpenAICompatible {
        provider: provider.clone(),
        openai_endpoint: OpenAI::chat_completions(),
    };
    let mut body_json: Value =
        serde_json::from_slice(body.as_ref()).map_err(InvalidRequestError::InvalidRequestBody)?;
    responses_ingress_normalize::rewrite_responses_input_items_for_create_response(&mut body_json)
        .map_err(|e| ApiError::Internal(InternalError::MapperError(e)))?;
    let cr: CreateResponse =
        serde_json::from_value(body_json).map_err(InvalidRequestError::InvalidRequestBody)?;
    let origin = cr.clone();
    let chat_req = create_response_to_chat_request(cr)
        .map_err(|e| ApiError::Internal(InternalError::MapperError(e)))?;
    let is_stream = chat_req.stream.unwrap_or(false);
    let model = if unified_model_body_passthrough {
        ModelId::Unknown(chat_req.model.clone())
    } else {
        ModelId::from_str_and_provider(provider.clone(), &chat_req.model)
            .map_err(InternalError::MapperError)?
    };
    let chat_bytes = Bytes::from(serde_json::to_vec(&chat_req).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<CreateChatCompletionRequest>(),
            error,
        })
    })?);
    let converter = converter_registry
        .get_converter(&ApiEndpoint::OpenAI(OpenAI::chat_completions()), &upstream)
        .ok_or_else(|| {
            ApiError::Internal(InternalError::InvalidConverter(
                ApiEndpoint::OpenAI(OpenAI::chat_completions()),
                upstream.clone(),
            ))
        })?;
    let (body_out, mut mc) = if unified_model_body_passthrough {
        converter.convert_req_body_model_passthrough(chat_bytes)?
    } else {
        converter.convert_req_body(chat_bytes)?
    };
    mc.cursor_responses_via_chat_completions = true;
    mc.cursor_responses_origin = Some(origin);
    mc.model = Some(model);
    mc.is_stream = is_stream;
    mc.client_expects_responses_wire = profile == P::CodexCli;
    mc.client_response_semantic = ClientResponseSemantic::Responses;
    mc.logger_response_wire_semantic = if is_stream {
        LoggerResponseWireSemantic::ChatCompletionsSse
    } else {
        LoggerResponseWireSemantic::ChatCompletionsJson
    };
    Ok(Some((body_out, mc, upstream)))
}

#[cfg(test)]
mod bridge_mapping_tests {
    use bytes::Bytes;
    use rustc_hash::FxHashMap;
    use serde_json::{Value, json};

    use super::try_map_responses_to_compatible_chat;
    use crate::{
        app::build_test_app,
        config::Config,
        endpoints::{ApiEndpoint, openai::OpenAI},
        ide_adapation::client_profile::ClientProfile,
        middleware::mapper::{model::ModelMapper, registry::EndpointConverterRegistry},
        types::{
            extensions::{ClientResponseSemantic, LoggerResponseWireSemantic},
            model_id::ModelId,
            provider::InferenceProvider,
        },
    };

    async fn registry() -> EndpointConverterRegistry {
        let app = build_test_app(Config::default()).await.expect("build app");
        let mut flags = FxHashMap::default();
        flags.insert("openrouter".to_string(), false);
        app.state.set_provider_is_router_flags(flags);
        let model_mapper = ModelMapper::new(app.state.clone());
        EndpointConverterRegistry::new(&model_mapper)
    }

    fn endpoints() -> (ApiEndpoint, ApiEndpoint) {
        let provider = InferenceProvider::Named("openrouter".into());
        (
            ApiEndpoint::OpenAI(OpenAI::responses()),
            ApiEndpoint::OpenAICompatible {
                provider,
                openai_endpoint: OpenAI::responses(),
            },
        )
    }

    fn responses_body(model: &str) -> Bytes {
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": model,
                "input": "hello",
                "stream": false
            }))
            .expect("body serializes"),
        )
    }

    fn responses_stream_body(model: &str) -> Bytes {
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": model,
                "input": "hello",
                "stream": true
            }))
            .expect("body serializes"),
        )
    }

    fn json_body(body: &Bytes) -> Value {
        serde_json::from_slice(body).expect("body is json")
    }

    #[tokio::test]
    async fn cursor_responses_bridge_preserves_unknown_model_when_unified_body_passthrough() {
        let registry = registry().await;
        let (source, target) = endpoints();

        let mapped = try_map_responses_to_compatible_chat(
            &registry,
            ClientProfile::CursorIde,
            &source,
            &target,
            &responses_stream_body("future-openrouter-responses-model"),
            true,
        )
        .expect("bridge should not reject unknown model")
        .expect("bridge should match cursor responses");

        let (body, ctx, upstream) = mapped;
        let body = json_body(&body);
        assert_eq!(body["model"], "future-openrouter-responses-model");
        assert!(body["messages"].is_array());
        assert_eq!(
            upstream,
            ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named("openrouter".into()),
                openai_endpoint: OpenAI::chat_completions(),
            }
        );
        assert!(ctx.cursor_responses_via_chat_completions);
        assert!(!ctx.client_expects_responses_wire);
        assert_eq!(
            ctx.client_response_semantic,
            ClientResponseSemantic::Responses
        );
        assert_eq!(
            ctx.logger_response_wire_semantic,
            LoggerResponseWireSemantic::ChatCompletionsSse
        );
        assert_eq!(
            ctx.model,
            Some(ModelId::Unknown(
                "future-openrouter-responses-model".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn codex_responses_bridge_preserves_unknown_model_and_responses_wire_flag_when_unified_body_passthrough()
     {
        let registry = registry().await;
        let (source, target) = endpoints();

        let mapped = try_map_responses_to_compatible_chat(
            &registry,
            ClientProfile::CodexCli,
            &source,
            &target,
            &responses_stream_body("future-openrouter-responses-model"),
            true,
        )
        .expect("bridge should not reject unknown model")
        .expect("bridge should match codex responses");

        let (body, ctx, _upstream) = mapped;
        let body = json_body(&body);
        assert_eq!(body["model"], "future-openrouter-responses-model");
        assert!(ctx.cursor_responses_via_chat_completions);
        assert!(ctx.client_expects_responses_wire);
        assert_eq!(
            ctx.client_response_semantic,
            ClientResponseSemantic::Responses
        );
        assert_eq!(
            ctx.logger_response_wire_semantic,
            LoggerResponseWireSemantic::ChatCompletionsSse
        );
        assert_eq!(
            ctx.model,
            Some(ModelId::Unknown(
                "future-openrouter-responses-model".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn cursor_responses_bridge_without_unified_body_passthrough_keeps_catalog_mapping() {
        let registry = registry().await;
        let (source, target) = endpoints();

        let mapped = try_map_responses_to_compatible_chat(
            &registry,
            ClientProfile::CursorIde,
            &source,
            &target,
            &responses_body("future-openrouter-responses-model"),
            false,
        );

        assert!(
            mapped.is_err(),
            "non-passthrough bridge should keep the existing converter \
             mapping path"
        );
    }

    #[tokio::test]
    async fn unknown_profile_does_not_trigger_responses_bridge_even_with_unified_body_passthrough()
    {
        let registry = registry().await;
        let (source, target) = endpoints();

        let mapped = try_map_responses_to_compatible_chat(
            &registry,
            ClientProfile::Unknown,
            &source,
            &target,
            &responses_body("future-openrouter-responses-model"),
            true,
        )
        .expect("profile miss should not error");

        assert!(mapped.is_none());
    }

    #[tokio::test]
    async fn openclaw_profile_does_not_trigger_responses_bridge_even_with_unified_body_passthrough()
    {
        let registry = registry().await;
        let (source, target) = endpoints();

        let mapped = try_map_responses_to_compatible_chat(
            &registry,
            ClientProfile::OpenClaw,
            &source,
            &target,
            &responses_body("future-openrouter-responses-model"),
            true,
        )
        .expect("profile miss should not error");

        assert!(mapped.is_none());
    }
}

#[cfg(test)]
mod stream_bridge_tests {
    use async_openai::types::{
        ChatChoiceStream, ChatCompletionMessageToolCallChunk, ChatCompletionStreamResponseDelta,
        CreateChatCompletionStreamResponse, FinishReason, FunctionCallStream, Role,
    };
    use http_body_util::BodyExt;
    use serde_json::json;

    use super::{CursorChatToResponsesStreamState, map_json_response_chat_to_responses};

    #[test]
    fn text_delta_emits_output_item_and_content_part_before_delta() {
        let mut state = CursorChatToResponsesStreamState::default();
        let chunk = CreateChatCompletionStreamResponse {
            id: "chatcmpl-test".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: Some("hi".to_string()),
                    role: Some(Role::Assistant),
                    function_call: None,
                    tool_calls: None,
                    refusal: None,
                    reasoning_content: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            created: 1,
            model: "gpt-test".to_string(),
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
            service_tier: None,
        };
        let raw = json!({
            "choices": [{ "delta": { "content": "hi", "role": "assistant" } }]
        });
        let out = state
            .process_upstream_chat_chunk(&chunk, &raw)
            .expect("chunk maps");
        let text = String::from_utf8(out.to_vec()).unwrap();
        let item_added = text.find("response.output_item.added").unwrap();
        let part_added = text.find("response.content_part.added").unwrap();
        let text_delta = text.find("response.output_text.delta").unwrap();
        assert!(item_added < part_added);
        assert!(part_added < text_delta);
    }

    #[test]
    fn reasoning_delta_uses_stable_reasoning_item_id() {
        let mut state = CursorChatToResponsesStreamState::with_tool_done_events(true);
        let reasoning_id = state.reasoning_item_id.clone();
        let chunk = CreateChatCompletionStreamResponse {
            id: "chatcmpl-r".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: None,
                    role: None,
                    refusal: None,
                    reasoning_content: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            created: 1,
            model: "gpt-test".to_string(),
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
            service_tier: None,
        };
        let raw = json!({
            "choices": [{ "delta": { "reasoning_content": "think" } }]
        });
        let out = state
            .process_upstream_chat_chunk(&chunk, &raw)
            .expect("reasoning maps");
        let text = String::from_utf8(out.to_vec()).unwrap();
        assert!(text.contains("response.output_item.added"));
        assert!(text.contains(&reasoning_id));
        assert!(text.contains("response.reasoning_text.delta"));
    }

    #[test]
    fn tool_call_finish_emits_responses_done_events() {
        let mut state = CursorChatToResponsesStreamState::with_tool_done_events(true);
        let tool_chunk = CreateChatCompletionStreamResponse {
            id: "chatcmpl-tool".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: Some(vec![ChatCompletionMessageToolCallChunk {
                        index: 0,
                        id: Some("call_read".to_string()),
                        r#type: Some(async_openai::types::ChatCompletionToolType::Function),
                        function: Some(FunctionCallStream {
                            name: Some("read_file".to_string()),
                            arguments: Some("{\"path\":\"Cargo.toml\"}".to_string()),
                        }),
                    }]),
                    role: None,
                    refusal: None,
                    reasoning_content: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            created: 1,
            model: "gpt-test".to_string(),
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
            service_tier: None,
        };
        let finish_chunk = CreateChatCompletionStreamResponse {
            id: "chatcmpl-tool".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: None,
                    role: None,
                    refusal: None,
                    reasoning_content: None,
                },
                finish_reason: Some(FinishReason::ToolCalls),
                logprobs: None,
            }],
            created: 1,
            model: "gpt-test".to_string(),
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
            service_tier: None,
        };

        let raw = json!({ "choices": [{ "delta": {} }] });
        let mut text = String::from_utf8(
            state
                .process_upstream_chat_chunk(&tool_chunk, &raw)
                .expect("tool chunk maps")
                .to_vec(),
        )
        .unwrap();
        text.push_str(
            &String::from_utf8(
                state
                    .process_upstream_chat_chunk(&finish_chunk, &raw)
                    .expect("finish chunk maps")
                    .to_vec(),
            )
            .unwrap(),
        );

        assert!(text.contains("response.output_item.added"));
        assert!(text.contains("response.function_call_arguments.delta"));
        assert!(text.contains("response.function_call_arguments.done"));
        assert!(text.contains("response.output_item.done"));
    }

    #[test]
    fn default_cursor_tool_call_finish_does_not_emit_extra_done_events() {
        let mut state = CursorChatToResponsesStreamState::default();
        let tool_chunk = CreateChatCompletionStreamResponse {
            id: "chatcmpl-tool".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: Some(vec![ChatCompletionMessageToolCallChunk {
                        index: 0,
                        id: Some("call_read".to_string()),
                        r#type: Some(async_openai::types::ChatCompletionToolType::Function),
                        function: Some(FunctionCallStream {
                            name: Some("read_file".to_string()),
                            arguments: Some("{\"path\":\"Cargo.toml\"}".to_string()),
                        }),
                    }]),
                    role: None,
                    refusal: None,
                    reasoning_content: None,
                },
                finish_reason: None,
                logprobs: None,
            }],
            created: 1,
            model: "gpt-test".to_string(),
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
            service_tier: None,
        };
        let finish_chunk = CreateChatCompletionStreamResponse {
            id: "chatcmpl-tool".to_string(),
            choices: vec![ChatChoiceStream {
                index: 0,
                delta: ChatCompletionStreamResponseDelta {
                    content: None,
                    function_call: None,
                    tool_calls: None,
                    role: None,
                    refusal: None,
                    reasoning_content: None,
                },
                finish_reason: Some(FinishReason::ToolCalls),
                logprobs: None,
            }],
            created: 1,
            model: "gpt-test".to_string(),
            system_fingerprint: None,
            object: "chat.completion.chunk".to_string(),
            usage: None,
            service_tier: None,
        };

        let raw = json!({ "choices": [{ "delta": {} }] });
        let mut text = String::from_utf8(
            state
                .process_upstream_chat_chunk(&tool_chunk, &raw)
                .expect("tool chunk maps")
                .to_vec(),
        )
        .unwrap();
        text.push_str(
            &String::from_utf8(
                state
                    .process_upstream_chat_chunk(&finish_chunk, &raw)
                    .expect("finish chunk maps")
                    .to_vec(),
            )
            .unwrap(),
        );

        assert!(text.contains("response.output_item.added"));
        assert!(text.contains("response.function_call_arguments.delta"));
        assert!(!text.contains("response.function_call_arguments.done"));
        assert!(!text.contains("response.output_item.done"));
    }

    #[tokio::test]
    async fn nonstream_chat_reasoning_maps_to_responses_output_and_usage() {
        let parts = http::Response::builder()
            .status(200)
            .body(())
            .expect("response builds")
            .into_parts()
            .0;
        let origin = async_openai::types::responses::CreateResponse {
            model: "openrouter/qwen".to_string(),
            input: async_openai::types::responses::Input::Text("hi".to_string()),
            background: None,
            include: None,
            instructions: Some("be precise".to_string()),
            max_output_tokens: None,
            max_tool_calls: None,
            metadata: None,
            parallel_tool_calls: Some(true),
            previous_response_id: None,
            prompt: None,
            reasoning: None,
            service_tier: None,
            store: None,
            stream: None,
            temperature: Some(0.2),
            text: None,
            tool_choice: None,
            tools: None,
            top_logprobs: None,
            top_p: None,
            truncation: None,
            user: None,
            extra: Default::default(),
        };
        let body = axum_core::body::Body::from(
            json!({
                "id": "chatcmpl-r",
                "object": "chat.completion",
                "created": 1,
                "model": "qwen",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "reasoning_content": "hidden chain summary",
                        "content": "final answer",
                        "refusal": null,
                        "tool_calls": null,
                        "function_call": null,
                        "audio": null
                    },
                    "finish_reason": "stop",
                    "logprobs": null
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 6,
                    "total_tokens": 16,
                    "completion_tokens_details": {
                        "reasoning_tokens": 4
                    }
                }
            })
            .to_string(),
        );

        let response = map_json_response_chat_to_responses(parts, body, &origin)
            .await
            .expect("response maps");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        let mapped: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");

        assert_eq!(mapped["output"][0]["type"], "reasoning");
        assert_eq!(mapped["output"][0]["status"], "completed");
        assert_eq!(
            mapped["output"][0]["summary"][0]["text"],
            "hidden chain summary"
        );
        assert_eq!(mapped["output"][1]["type"], "message");
        assert_eq!(mapped["output"][1]["content"][0]["text"], "final answer");
        assert_eq!(mapped["usage"]["input_tokens"], 10);
        assert_eq!(mapped["usage"]["output_tokens"], 6);
        assert_eq!(mapped["usage"]["total_tokens"], 16);
        assert_eq!(
            mapped["usage"]["output_tokens_details"]["reasoning_tokens"],
            4
        );
    }

    #[tokio::test]
    async fn nonstream_without_reasoning_keeps_existing_usage_shape() {
        let parts = http::Response::builder()
            .status(200)
            .body(())
            .expect("response builds")
            .into_parts()
            .0;
        let origin = async_openai::types::responses::CreateResponse {
            model: "openrouter/qwen".to_string(),
            input: async_openai::types::responses::Input::Text("hi".to_string()),
            background: None,
            include: None,
            instructions: None,
            max_output_tokens: None,
            max_tool_calls: None,
            metadata: None,
            parallel_tool_calls: None,
            previous_response_id: None,
            prompt: None,
            reasoning: None,
            service_tier: None,
            store: None,
            stream: None,
            temperature: None,
            text: None,
            tool_choice: None,
            tools: None,
            top_logprobs: None,
            top_p: None,
            truncation: None,
            user: None,
            extra: Default::default(),
        };
        let body = axum_core::body::Body::from(
            json!({
                "id": "chatcmpl-nr",
                "object": "chat.completion",
                "created": 1,
                "model": "qwen",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "final answer",
                        "refusal": null,
                        "tool_calls": null,
                        "function_call": null,
                        "audio": null
                    },
                    "finish_reason": "stop",
                    "logprobs": null
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 6,
                    "total_tokens": 16
                }
            })
            .to_string(),
        );

        let response = map_json_response_chat_to_responses(parts, body, &origin)
            .await
            .expect("response maps");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        let mapped: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json");

        assert_eq!(mapped["output"].as_array().expect("output").len(), 1);
        assert_eq!(mapped["output"][0]["type"], "message");
        assert!(mapped["usage"].get("output_tokens_details").is_none());
    }
}
