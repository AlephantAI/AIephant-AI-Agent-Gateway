//! Bridge OpenAI Responses API payloads to Chat Completions shape for unified
//! `chat/completions` clients (e.g. Cursor) that POST Responses-shaped JSON but
//! expect Chat Completions SSE / JSON.

use std::collections::{HashMap, HashSet};

use async_openai::types::{
    ChatChoice, ChatChoiceStream, ChatCompletionMessageToolCall,
    ChatCompletionResponseMessage, ChatCompletionStreamResponseDelta,
    ChatCompletionToolType, CompletionTokensDetails, CompletionUsage,
    CreateChatCompletionResponse, CreateChatCompletionStreamResponse,
    FinishReason, PromptTokensDetails, Role,
    responses::{Annotation, Content, OutputContent, Response, Status},
};
use bytes::{BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde_json::Value;

use crate::{
    error::{api::ApiError, internal::InternalError},
    middleware::mapper::stream_normalizer::{
        build_finish_choice, build_reasoning_choice, build_role_choice,
        build_stream_response, build_text_choice, build_tool_call_chunk,
        build_tool_choice,
    },
};

#[derive(Debug, Default)]
pub(super) struct BridgeStreamState {
    completion_id: Option<String>,
    model: Option<String>,
    role_sent: bool,
    has_tool_calls: bool,
    hidden_text_item_ids: HashSet<String>,
    /// Maps Responses-API item_id → sequential tool call index for streaming.
    tool_call_indices: HashMap<String, u32>,
    next_tool_index: u32,
}

fn parse_prompt_tokens_details(v: &Value) -> Option<PromptTokensDetails> {
    let d = v.get("input_tokens_details")?;
    Some(PromptTokensDetails {
        cached_tokens: d
            .get("cached_tokens")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as u32),
        audio_tokens: None,
        cache_write_tokens: None,
        cache_write_details: None,
    })
}

fn parse_completion_tokens_details(
    v: &Value,
) -> Option<CompletionTokensDetails> {
    let d = v.get("output_tokens_details")?;
    Some(CompletionTokensDetails {
        reasoning_tokens: d
            .get("reasoning_tokens")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as u32),
        audio_tokens: None,
        accepted_prediction_tokens: None,
        rejected_prediction_tokens: None,
    })
}

fn completion_usage_from_responses_value(u: &Value) -> Option<CompletionUsage> {
    let input = u.get("input_tokens")?.as_u64()? as u32;
    let output = u.get("output_tokens")?.as_u64()? as u32;
    let total = u
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64)
        .map_or_else(|| input.saturating_add(output), |t| t as u32);
    Some(CompletionUsage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: total,
        prompt_tokens_details: parse_prompt_tokens_details(u),
        completion_tokens_details: parse_completion_tokens_details(u),
    })
}

fn put_sse_record(buf: &mut BytesMut, payload: &[u8]) {
    buf.put("data: ".as_bytes());
    buf.put(payload);
    buf.put("\n\n".as_bytes());
}

fn put_sse_json<T: Serialize>(
    buf: &mut BytesMut,
    val: &T,
) -> Result<(), ApiError> {
    let json = serde_json::to_vec(val).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<T>(),
            error,
        })
    })?;
    put_sse_record(buf, &json);
    Ok(())
}

impl BridgeStreamState {
    fn ingest_response_snapshot(&mut self, resp: Option<&Value>) {
        let Some(resp) = resp else {
            return;
        };
        if let Some(id) = resp.get("id").and_then(|i| i.as_str()) {
            self.completion_id = Some(id.to_string());
        }
        if let Some(m) = resp.get("model").and_then(|m| m.as_str()) {
            self.model = Some(m.to_string());
        }
    }

    fn completion_id(&self) -> String {
        self.completion_id
            .clone()
            .unwrap_or_else(|| "chatcmpl-bridge".to_string())
    }

    fn model_name(&self) -> String {
        self.model.clone().unwrap_or_else(|| "unknown".to_string())
    }

    fn push_role_if_needed(
        &mut self,
        buf: &mut BytesMut,
    ) -> Result<(), ApiError> {
        if self.role_sent {
            return Ok(());
        }
        let chunk = build_stream_response(
            self.completion_id(),
            self.model_name(),
            vec![build_role_choice(0, Role::Assistant)],
            None,
        );
        put_sse_json(buf, &chunk)?;
        self.role_sent = true;
        Ok(())
    }

    fn finish_chunk(
        &self,
        usage: Option<CompletionUsage>,
    ) -> Result<CreateChatCompletionStreamResponse, ApiError> {
        let reason = if self.has_tool_calls {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };
        Ok(build_stream_response(
            self.completion_id(),
            self.model_name(),
            vec![build_finish_choice(
                Some(reason),
                CompletionUsage::default(),
                None,
            )],
            usage,
        ))
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn process_upstream_sse_json(
        &mut self,
        raw: &[u8],
    ) -> Result<Option<Bytes>, ApiError> {
        let v: Value = serde_json::from_slice(raw).map_err(|error| {
            ApiError::Internal(InternalError::Deserialize {
                ty: "responses_sse_event",
                error,
            })
        })?;

        let mut buf = BytesMut::new();
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match ty {
            "response.created" | "response.in_progress" => {
                self.ingest_response_snapshot(v.get("response"));
            }
            "response.reasoning_summary_text.delta" => return Ok(None),
            "response.output_text.delta" => {
                if v.get("item_id").and_then(|i| i.as_str()).is_some_and(
                    |item_id| self.hidden_text_item_ids.contains(item_id),
                ) {
                    return Ok(None);
                }
                let delta =
                    v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Ok(None);
                }
                self.push_role_if_needed(&mut buf)?;
                let chunk = build_stream_response(
                    self.completion_id(),
                    self.model_name(),
                    vec![build_text_choice(0, delta.to_string())],
                    None,
                );
                put_sse_json(&mut buf, &chunk)?;
            }
            "response.reasoning_text.delta" => {
                let delta =
                    v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Ok(None);
                }
                self.push_role_if_needed(&mut buf)?;
                let chunk = build_stream_response(
                    self.completion_id(),
                    self.model_name(),
                    vec![build_reasoning_choice(0, delta.to_string())],
                    None,
                );
                put_sse_json(&mut buf, &chunk)?;
            }
            "response.refusal.delta" => {
                let delta =
                    v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Ok(None);
                }
                self.push_role_if_needed(&mut buf)?;
                let choice = ChatChoiceStream {
                    index: 0,
                    delta: ChatCompletionStreamResponseDelta {
                        content: None,
                        #[allow(deprecated)]
                        function_call: None,
                        tool_calls: None,
                        role: None,
                        refusal: Some(delta.to_string()),
                        reasoning_content: None,
                    },
                    finish_reason: None,
                    logprobs: None,
                };
                let chunk = build_stream_response(
                    self.completion_id(),
                    self.model_name(),
                    vec![choice],
                    None,
                );
                put_sse_json(&mut buf, &chunk)?;
            }
            "response.output_item.added" => {
                if let Some(item) = v.get("item") {
                    let item_type =
                        item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if item_type == "message"
                        && item
                            .get("phase")
                            .and_then(|p| p.as_str())
                            .is_some_and(|phase| phase == "commentary")
                        && let Some(item_id) =
                            item.get("id").and_then(|i| i.as_str())
                    {
                        self.hidden_text_item_ids.insert(item_id.to_string());
                    }
                    if item_type == "function_call" {
                        self.has_tool_calls = true;
                        let item_id = item
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let call_id = item
                            .get("call_id")
                            .and_then(|c| c.as_str())
                            .unwrap_or(&item_id)
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let idx = self.next_tool_index;
                        self.tool_call_indices.insert(item_id.clone(), idx);
                        self.next_tool_index += 1;

                        self.push_role_if_needed(&mut buf)?;
                        let tc = build_tool_call_chunk(
                            idx,
                            Some(call_id),
                            Some(name),
                            Some(String::new()),
                        );
                        let choice = build_tool_choice(0, tc);
                        let chunk = build_stream_response(
                            self.completion_id(),
                            self.model_name(),
                            vec![choice],
                            None,
                        );
                        put_sse_json(&mut buf, &chunk)?;
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let delta =
                    v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Ok(None);
                }
                let item_id =
                    v.get("item_id").and_then(|i| i.as_str()).unwrap_or("");
                let idx =
                    self.tool_call_indices.get(item_id).copied().unwrap_or(0);

                let tc = build_tool_call_chunk(
                    idx,
                    None,
                    None,
                    Some(delta.to_string()),
                );
                let choice = build_tool_choice(0, tc);
                let chunk = build_stream_response(
                    self.completion_id(),
                    self.model_name(),
                    vec![choice],
                    None,
                );
                put_sse_json(&mut buf, &chunk)?;
            }
            "response.function_call_arguments.done" => {
                // Arguments fully delivered via delta events; nothing extra
                // needed. We still mark tool calls in case the initial
                // output_item.added was somehow missed.
                self.has_tool_calls = true;
            }
            "response.completed" => {
                self.ingest_response_snapshot(v.get("response"));
                self.push_role_if_needed(&mut buf)?;
                let usage = v
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .and_then(completion_usage_from_responses_value);
                let finish = self.finish_chunk(usage)?;
                put_sse_json(&mut buf, &finish)?;
                put_sse_record(&mut buf, b"[DONE]");
            }
            "error" => {
                let err_obj = serde_json::json!({
                    "error": {
                        "message": v.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error"),
                        "type": "invalid_request_error",
                        "code": v.get("code").and_then(|c| c.as_str()),
                        "param": v.get("param").and_then(|p| p.as_str()),
                    }
                });
                put_sse_json(&mut buf, &err_obj)?;
            }
            "response.failed" => {
                self.ingest_response_snapshot(v.get("response"));
                self.push_role_if_needed(&mut buf)?;
                let finish = self.finish_chunk(None)?;
                put_sse_json(&mut buf, &finish)?;
                put_sse_record(&mut buf, b"[DONE]");
            }
            _ => {
                return Ok(None);
            }
        }

        if buf.is_empty() {
            Ok(None)
        } else {
            Ok(Some(buf.freeze()))
        }
    }
}

struct AggregatedOutput {
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Vec<ChatCompletionMessageToolCall>,
}

fn aggregate_output(resp: &Response) -> AggregatedOutput {
    let mut text_buf = String::new();
    let mut refusal_out: Option<String> = None;
    let mut tool_calls = Vec::new();
    let mut annotations: Vec<&Annotation> = Vec::new();

    if let Some(ref t) = resp.output_text {
        text_buf.push_str(t);
    }

    for item in &resp.output {
        match item {
            OutputContent::Message(m) => {
                for c in &m.content {
                    match c {
                        Content::OutputText(ot) => {
                            text_buf.push_str(&ot.text);
                            annotations.extend(ot.annotations.iter());
                        }
                        Content::Refusal(r) => {
                            refusal_out = Some(r.refusal.clone());
                        }
                    }
                }
            }
            OutputContent::FunctionCall(fc) => {
                tool_calls.push(ChatCompletionMessageToolCall {
                    id: fc.call_id.clone(),
                    r#type: ChatCompletionToolType::Function,
                    function: async_openai::types::FunctionCall {
                        name: fc.name.clone(),
                        arguments: fc.arguments.clone(),
                    },
                });
            }
            _ => {}
        }
    }

    if !annotations.is_empty() {
        text_buf.push_str("\n\n---\nReferences:\n");
        for (i, ann) in annotations.iter().enumerate() {
            match ann {
                Annotation::UrlCitation(uc) => {
                    let title = if uc.title.is_empty() {
                        &uc.url
                    } else {
                        &uc.title
                    };
                    text_buf.push_str(&format!(
                        "[{}] {}: {}\n",
                        i + 1,
                        title,
                        uc.url
                    ));
                }
                Annotation::FileCitation(fc) => {
                    text_buf.push_str(&format!(
                        "[{}] file: {}\n",
                        i + 1,
                        fc.file_id
                    ));
                }
                Annotation::FilePath(fp) => {
                    text_buf.push_str(&format!(
                        "[{}] file: {}\n",
                        i + 1,
                        fp.file_id
                    ));
                }
            }
        }
    }

    let content = (!text_buf.is_empty()).then_some(text_buf);
    AggregatedOutput {
        content,
        refusal: refusal_out,
        tool_calls,
    }
}

fn finish_reason_for_status(
    status: &Status,
    has_tool_calls: bool,
) -> Option<FinishReason> {
    match status {
        Status::Completed => {
            if has_tool_calls {
                Some(FinishReason::ToolCalls)
            } else {
                Some(FinishReason::Stop)
            }
        }
        Status::Incomplete => Some(FinishReason::Length),
        Status::Failed => Some(FinishReason::Stop),
        Status::InProgress => None,
    }
}

pub(super) fn non_stream_responses_body_to_chat_completion(
    body: &[u8],
) -> Result<Bytes, ApiError> {
    let resp: Response = serde_json::from_slice(body).map_err(|error| {
        ApiError::Internal(InternalError::Deserialize {
            ty: std::any::type_name::<Response>(),
            error,
        })
    })?;

    let agg = aggregate_output(&resp);
    let has_tool_calls = !agg.tool_calls.is_empty();
    let finish_reason = finish_reason_for_status(&resp.status, has_tool_calls);
    let tool_calls = if agg.tool_calls.is_empty() {
        None
    } else {
        Some(agg.tool_calls)
    };

    let message = ChatCompletionResponseMessage {
        content: agg.content,
        refusal: agg.refusal,
        tool_calls,
        role: Role::Assistant,
        #[allow(deprecated)]
        function_call: None,
        audio: None,
        reasoning_content: None,
    };

    let choice = ChatChoice {
        index: 0,
        message,
        finish_reason,
        logprobs: None,
    };

    let usage = resp.usage.as_ref().map(|u| CompletionUsage {
        prompt_tokens: u.input_tokens,
        completion_tokens: u.output_tokens,
        total_tokens: u.total_tokens,
        prompt_tokens_details: Some(u.input_tokens_details.clone()),
        completion_tokens_details: Some(u.output_tokens_details.clone()),
    });

    let created_u32 = u32::try_from(resp.created_at).unwrap_or(u32::MAX);

    let out = CreateChatCompletionResponse {
        id: resp.id.clone(),
        choices: vec![choice],
        created: created_u32,
        model: resp.model.clone(),
        service_tier: None,
        system_fingerprint: None,
        object: "chat.completion".to_string(),
        usage,
    };

    let bytes = serde_json::to_vec(&out).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<CreateChatCompletionResponse>(),
            error,
        })
    })?;
    Ok(Bytes::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_emits_role_then_text_then_done() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        let o1 = st
            .process_upstream_sse_json(
                br#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}}"#,
            )
            .unwrap();
        assert!(o1.is_none());

        let o2 = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_text.delta","delta":"hi"}"#,
            )
            .unwrap()
            .expect("chunk");
        acc.put(o2.as_ref());

        let o3 = st
            .process_upstream_sse_json(
                br#"{"type":"response.completed","response":{"id":"resp_1","model":"gpt-5","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}"#,
            )
            .unwrap()
            .expect("done");
        acc.put(o3.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(s.contains("chat.completion.chunk"));
        assert!(s.contains("\"role\":\"assistant\""));
        assert!(s.contains("\"content\":\"hi\""));
        assert!(s.contains("prompt_tokens"));
        assert!(s.contains("data: [DONE]"));
    }

    #[test]
    fn bridge_drops_reasoning_summary_text_from_chat_content() {
        let mut st = BridgeStreamState::default();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_reasoning","model":"gpt-5"}}"#,
        )
        .unwrap();

        let reasoning = st
            .process_upstream_sse_json(
                br#"{"type":"response.reasoning_summary_text.delta","delta":"Organizing explanation"}"#,
            )
            .unwrap();
        assert!(reasoning.is_none());
    }

    #[test]
    fn bridge_drops_commentary_text_but_keeps_final_answer_text() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_phase","model":"gpt-5"}}"#,
        )
        .unwrap();
        st.process_upstream_sse_json(
            br#"{"type":"response.output_item.added","output_index":0,"item":{"id":"msg_commentary","type":"message","phase":"commentary","role":"assistant","content":[]}}"#,
        )
        .unwrap();
        let commentary = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_text.delta","item_id":"msg_commentary","delta":"I am checking logs"}"#,
            )
            .unwrap();
        assert!(commentary.is_none());

        st.process_upstream_sse_json(
            br#"{"type":"response.output_item.added","output_index":1,"item":{"id":"msg_final","type":"message","phase":"final_answer","role":"assistant","content":[]}}"#,
        )
        .unwrap();
        let final_text = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_text.delta","item_id":"msg_final","delta":"final answer"}"#,
            )
            .unwrap()
            .expect("final answer text is forwarded");
        acc.put(final_text.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(!s.contains("I am checking logs"));
        assert!(s.contains("final answer"));
    }

    #[test]
    fn bridge_emits_tool_call_chunks_and_finish_reason() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_tc","model":"gpt-5"}}"#,
        )
        .unwrap();

        let o1 = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_item.added","item":{"id":"item_1","type":"function_call","call_id":"call_abc","name":"read_file","arguments":""}}"#,
            )
            .unwrap()
            .expect("tool call start chunk");
        acc.put(o1.as_ref());

        let o2 = st
            .process_upstream_sse_json(
                br#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"{\"path\":"}"#,
            )
            .unwrap()
            .expect("args delta");
        acc.put(o2.as_ref());

        let o3 = st
            .process_upstream_sse_json(
                br#"{"type":"response.function_call_arguments.delta","item_id":"item_1","delta":"\"foo.rs\"}"}"#,
            )
            .unwrap()
            .expect("args delta 2");
        acc.put(o3.as_ref());

        st.process_upstream_sse_json(
            br#"{"type":"response.function_call_arguments.done","item_id":"item_1","arguments":"{\"path\":\"foo.rs\"}"}"#,
        )
        .unwrap();

        let o4 = st
            .process_upstream_sse_json(
                br#"{"type":"response.completed","response":{"id":"resp_tc","model":"gpt-5","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
            )
            .unwrap()
            .expect("done");
        acc.put(o4.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(s.contains("\"role\":\"assistant\""));
        assert!(s.contains("call_abc"));
        assert!(s.contains("read_file"));
        assert!(s.contains("path"));
        assert!(s.contains("foo.rs"));
        assert!(s.contains("\"finish_reason\":\"tool_calls\""));
        assert!(s.contains("data: [DONE]"));
    }

    #[test]
    fn bridge_parallel_tool_calls() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_p","model":"gpt-5"}}"#,
        )
        .unwrap();

        let o1 = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_item.added","item":{"id":"item_a","type":"function_call","call_id":"call_1","name":"read_file","arguments":""}}"#,
            )
            .unwrap()
            .expect("first tool");
        acc.put(o1.as_ref());

        let o2 = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_item.added","item":{"id":"item_b","type":"function_call","call_id":"call_2","name":"write_file","arguments":""}}"#,
            )
            .unwrap()
            .expect("second tool");
        acc.put(o2.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(s.contains("call_1"));
        assert!(s.contains("call_2"));
        assert!(s.contains("read_file"));
        assert!(s.contains("write_file"));
        assert_eq!(st.next_tool_index, 2);
    }

    #[test]
    fn bridge_text_only_uses_stop_finish_reason() {
        let mut st = BridgeStreamState::default();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_s","model":"gpt-5"}}"#,
        )
        .unwrap();

        st.process_upstream_sse_json(
            br#"{"type":"response.output_text.delta","delta":"hello"}"#,
        )
        .unwrap();

        let done = st
            .process_upstream_sse_json(
                br#"{"type":"response.completed","response":{"id":"resp_s","model":"gpt-5","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
            )
            .unwrap()
            .expect("done");

        let s = String::from_utf8_lossy(&done);
        assert!(s.contains("\"finish_reason\":\"stop\""));
        assert!(!s.contains("\"finish_reason\":\"tool_calls\""));
    }

    #[test]
    fn bridge_reasoning_text_delta_maps_to_reasoning_content() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_r","model":"o3"}}"#,
        )
        .unwrap();

        let o = st
            .process_upstream_sse_json(
                br#"{"type":"response.reasoning_text.delta","delta":"thinking step"}"#,
            )
            .unwrap()
            .expect("reasoning chunk");
        acc.put(o.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(
            s.contains("\"reasoning_content\":\"thinking step\""),
            "reasoning text should map to reasoning_content, got: {s}"
        );
        assert!(
            !s.contains("\"content\":\"thinking step\""),
            "reasoning text must NOT appear as regular content"
        );
    }

    #[test]
    fn bridge_output_text_delta_still_maps_to_content() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        st.process_upstream_sse_json(
            br#"{"type":"response.created","response":{"id":"resp_oc","model":"o3"}}"#,
        )
        .unwrap();

        let o = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_text.delta","delta":"hello world"}"#,
            )
            .unwrap()
            .expect("text chunk");
        acc.put(o.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(
            s.contains("\"content\":\"hello world\""),
            "output text should map to content, got: {s}"
        );
        assert!(
            !s.contains("\"reasoning_content\":\"hello world\""),
            "output text must NOT appear as reasoning_content"
        );
    }

    #[test]
    fn non_stream_maps_basic_response() {
        let json = br#"{"id":"resp_x","created_at":1,"model":"gpt-5","object":"response","output":[],"status":"completed"}"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["id"], "resp_x");
    }

    #[test]
    fn non_stream_maps_function_call_output() {
        let json = br#"{
            "id":"resp_fc",
            "created_at":1,
            "model":"gpt-5",
            "object":"response",
            "output":[
                {"type":"function_call","id":"item_1","call_id":"call_xyz","name":"shell","arguments":"{\"cmd\":\"ls\"}","status":"completed"}
            ],
            "status":"completed"
        }"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["object"], "chat.completion");
        let tc = &v["choices"][0]["message"]["tool_calls"];
        assert_eq!(tc[0]["id"], "call_xyz");
        assert_eq!(tc[0]["type"], "function");
        assert_eq!(tc[0]["function"]["name"], "shell");
        assert_eq!(tc[0]["function"]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn non_stream_maps_mixed_text_and_tool_calls() {
        let json = br#"{
            "id":"resp_mix",
            "created_at":1,
            "model":"gpt-5",
            "object":"response",
            "output":[
                {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"thinking...","annotations":[]}]},
                {"type":"function_call","id":"item_2","call_id":"call_a","name":"read","arguments":"{}","status":"completed"},
                {"type":"function_call","id":"item_3","call_id":"call_b","name":"write","arguments":"{\"x\":1}","status":"completed"}
            ],
            "status":"completed"
        }"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let msg = &v["choices"][0]["message"];
        assert_eq!(msg["content"], "thinking...");
        assert_eq!(msg["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn non_stream_bridge_inlines_annotations_as_footnotes() {
        let json = br#"{
            "id":"resp_ann",
            "created_at":1,
            "model":"gpt-5",
            "object":"response",
            "output":[
                {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{
                    "type":"output_text",
                    "text":"According to the source, AI is transformative.",
                    "annotations":[
                        {"type":"url_citation","url":"https://example.com/ai","title":"AI Overview","start_index":0,"end_index":20}
                    ]
                }]}
            ],
            "status":"completed"
        }"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let content = v["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.contains("According to the source"));
        assert!(content.contains("References:"));
        assert!(content.contains("[1] AI Overview: https://example.com/ai"));
    }

    #[test]
    fn non_stream_bridge_no_footnotes_when_no_annotations() {
        let json = br#"{
            "id":"resp_noann",
            "created_at":1,
            "model":"gpt-5",
            "object":"response",
            "output":[
                {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{
                    "type":"output_text",
                    "text":"Just plain text.",
                    "annotations":[]
                }]}
            ],
            "status":"completed"
        }"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let content = v["choices"][0]["message"]["content"].as_str().unwrap();
        assert_eq!(content, "Just plain text.");
        assert!(!content.contains("References:"));
    }

    #[test]
    fn non_stream_bridge_inlines_multiple_annotation_types() {
        let json = br#"{
            "id":"resp_multi",
            "created_at":1,
            "model":"gpt-5",
            "object":"response",
            "output":[
                {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{
                    "type":"output_text",
                    "text":"Mixed citations here.",
                    "annotations":[
                        {"type":"url_citation","url":"https://a.com","title":"Source A","start_index":0,"end_index":5},
                        {"type":"file_citation","file_id":"file_abc123","index":0},
                        {"type":"url_citation","url":"https://b.com","title":"","start_index":6,"end_index":10}
                    ]
                }]}
            ],
            "status":"completed"
        }"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let content = v["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.contains("[1] Source A: https://a.com"));
        assert!(content.contains("[2] file: file_abc123"));
        assert!(content.contains("[3] https://b.com: https://b.com"));
    }
}
