use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_ARGUMENTS_SUMMARY_CHARS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedToolCall {
    pub tool_call_id: Option<String>,
    pub tool_name: String,
    pub tool_type: Option<String>,
    pub arguments_summary: Option<String>,
    pub choice_index: Option<u32>,
}

pub const MAX_OBSERVED_CHAT_STREAM_TOOL_CALLS: usize = 16;
pub const MAX_CHAT_STREAM_FRAMES: usize = 128;
pub const MAX_CHAT_STREAM_ARGUMENT_CHARS_PER_TOOL: usize = 16 * 1024;
pub const MAX_OBSERVER_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_OUTPUT_ITEMS: usize = 64;
pub const MAX_RESPONSE_STREAM_EVENTS: usize = 128;
pub const MAX_OBSERVED_EVENTS_PER_RESPONSE: usize = 16;
pub const MAX_REASONING_SUMMARY_CHARS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatStreamAggregationStatus {
    Complete,
    Partial,
    Aborted,
}

impl ChatStreamAggregationStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedChatCompletionStreamToolCall {
    pub choice_index: u32,
    pub tool_call_index: Option<u32>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_type: Option<String>,
    pub arguments_summary: Option<String>,
    pub arguments_hash: Option<String>,
    pub arguments_chars: Option<u64>,
    pub arguments_valid_json: Option<bool>,
    pub arguments_truncated: bool,
    pub chunk_count: u32,
    pub finish_reason: Option<String>,
    pub sse_done_seen: bool,
    pub aggregation_status: ChatStreamAggregationStatus,
    pub idempotency_key: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ChatStreamToolKey {
    Indexed {
        choice_index: u32,
        tool_call_index: u32,
    },
    ProviderId {
        choice_index: u32,
        tool_call_id: String,
    },
}

#[derive(Debug, Default)]
struct ChatStreamToolDraft {
    choice_index: u32,
    tool_call_index: Option<u32>,
    tool_call_id: Option<String>,
    tool_name: Option<String>,
    tool_type: Option<String>,
    arguments: String,
    arguments_truncated: bool,
    chunk_count: u32,
    finish_reason: Option<String>,
    sse_done_seen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedResponsesItemKind {
    FunctionCall,
    McpCall,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedResponsesItem {
    pub kind: ObservedResponsesItemKind,
    pub event_type: &'static str,
    pub response_id: Option<String>,
    pub output_index: Option<u32>,
    pub item_id: Option<String>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedResponsesStreamItemKind {
    FunctionCall,
    McpCall,
    Reasoning,
    ResponseCompleted,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedResponsesStreamItem {
    pub kind: ObservedResponsesStreamItemKind,
    pub event_type: &'static str,
    pub response_id: Option<String>,
    pub sequence: u32,
    pub output_index: Option<u32>,
    pub item_id: Option<String>,
    pub call_id: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub idempotency_key: String,
    pub metadata: serde_json::Value,
}

#[must_use]
pub fn observe_chat_completion_tool_calls(body: &[u8]) -> Vec<ObservedToolCall> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut observed = Vec::new();

    for (choice_index, choice) in choices.iter().enumerate() {
        let Some(tool_calls) = choice
            .get("message")
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for tool_call in tool_calls {
            let tool_call_id = optional_string(tool_call.get("id"));
            let tool_type = optional_string(tool_call.get("type"));
            let tool_name = optional_string(
                tool_call
                    .get("function")
                    .and_then(|function| function.get("name")),
            )
            .or_else(|| optional_string(tool_call.get("name")));
            let Some(tool_name) = tool_name else {
                continue;
            };

            let arguments_summary = optional_string(
                tool_call
                    .get("function")
                    .and_then(|function| function.get("arguments")),
            )
            .map(|arguments| summarize_arguments(&arguments));
            let choice_index_u32 = u32::try_from(choice_index).ok();
            let dedupe_key = tool_call_id.clone().unwrap_or_else(|| {
                format!(
                    "{}:{}:{}",
                    choice_index,
                    tool_name,
                    arguments_summary.as_deref().unwrap_or_default()
                )
            });
            if !seen.insert(dedupe_key) {
                continue;
            }

            observed.push(ObservedToolCall {
                tool_call_id,
                tool_name,
                tool_type,
                arguments_summary,
                choice_index: choice_index_u32,
            });
        }
    }

    observed
}

#[must_use]
pub fn observe_chat_completion_stream_tool_calls(
    request_id: uuid::Uuid,
    body: &[u8],
) -> Vec<ObservedChatCompletionStreamToolCall> {
    if body.len() > MAX_OBSERVER_BODY_BYTES {
        return Vec::new();
    }

    let mut draft_indexes = HashMap::new();
    let mut drafts = Vec::new();
    let mut sse_done_seen = false;

    for frame in parse_sse_data_frames(body)
        .into_iter()
        .take(MAX_CHAT_STREAM_FRAMES)
    {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }
        if frame == "[DONE]" {
            sse_done_seen = true;
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(frame) else {
            continue;
        };
        observe_chat_completion_stream_frame(
            &mut draft_indexes,
            &mut drafts,
            &value,
            sse_done_seen,
        );
    }

    drafts
        .into_iter()
        .take(MAX_OBSERVED_CHAT_STREAM_TOOL_CALLS)
        .map(|mut draft| {
            draft.sse_done_seen |= sse_done_seen;
            observed_chat_stream_tool_call(request_id, draft)
        })
        .collect()
}

fn observe_chat_completion_stream_frame(
    draft_indexes: &mut HashMap<ChatStreamToolKey, usize>,
    drafts: &mut Vec<ChatStreamToolDraft>,
    value: &Value,
    sse_done_seen: bool,
) {
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return;
    };

    for (fallback_choice_index, choice) in choices.iter().enumerate() {
        let Some(choice_index) =
            optional_u32(choice.get("index")).or_else(|| u32::try_from(fallback_choice_index).ok())
        else {
            continue;
        };
        let finish_reason = optional_string(choice.get("finish_reason"));
        let tool_calls = choice
            .get("delta")
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array);

        if let Some(tool_calls) = tool_calls {
            for tool_call in tool_calls {
                let tool_call_index = optional_u32(tool_call.get("index"));
                let tool_call_id = optional_string(tool_call.get("id"));
                let tool_name = tool_call
                    .get("function")
                    .and_then(|function| optional_string(function.get("name")));
                let keys =
                    chat_stream_tool_keys(choice_index, tool_call_index, tool_call_id.as_deref());
                if keys.is_empty() {
                    continue;
                }
                let draft_index = find_chat_stream_draft_index(
                    draft_indexes,
                    drafts,
                    &keys,
                    choice_index,
                    tool_call_index,
                    tool_call_id.as_deref(),
                    tool_name.as_deref(),
                );
                let is_new_draft = draft_index.is_none();
                if is_new_draft && drafts.len() >= MAX_OBSERVED_CHAT_STREAM_TOOL_CALLS {
                    continue;
                }

                let draft_index = draft_index.unwrap_or_else(|| {
                    let draft_index = drafts.len();
                    drafts.push(ChatStreamToolDraft {
                        choice_index,
                        tool_call_index,
                        tool_call_id: tool_call_id.clone(),
                        ..ChatStreamToolDraft::default()
                    });
                    draft_index
                });
                register_chat_stream_key_aliases(draft_indexes, &keys, draft_index);
                let draft = &mut drafts[draft_index];
                draft.sse_done_seen |= sse_done_seen;
                draft.chunk_count = draft.chunk_count.saturating_add(1);
                if draft.tool_call_id.is_none() {
                    draft.tool_call_id = tool_call_id;
                }
                if draft.tool_call_index.is_none() {
                    draft.tool_call_index = tool_call_index;
                }
                if draft.tool_type.is_none() {
                    draft.tool_type = optional_string(tool_call.get("type"));
                }
                if draft.finish_reason.is_none() {
                    draft.finish_reason = finish_reason.clone();
                }
                if let Some(function) = tool_call.get("function") {
                    if draft.tool_name.is_none() {
                        draft.tool_name = tool_name;
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        append_chat_stream_arguments(draft, &arguments);
                    }
                }
            }
        }

        if let Some(finish_reason) = finish_reason {
            for draft in drafts
                .iter_mut()
                .filter(|draft| draft.choice_index == choice_index && draft.finish_reason.is_none())
            {
                draft.finish_reason = Some(finish_reason.clone());
            }
        }
    }
}

fn find_chat_stream_draft_index(
    draft_indexes: &HashMap<ChatStreamToolKey, usize>,
    drafts: &[ChatStreamToolDraft],
    keys: &[ChatStreamToolKey],
    choice_index: u32,
    tool_call_index: Option<u32>,
    tool_call_id: Option<&str>,
    tool_name: Option<&str>,
) -> Option<usize> {
    if let Some(index) = keys.iter().find_map(|key| draft_indexes.get(key).copied()) {
        return Some(index);
    }

    match (tool_call_index, tool_call_id) {
        (Some(index), None) => drafts
            .iter()
            .position(|draft| {
                draft.choice_index == choice_index
                    && draft.tool_call_index == Some(index)
                    && draft.tool_call_id.is_none()
            })
            .or_else(|| {
                unique_chat_stream_draft_candidate(drafts, |draft| {
                    draft.choice_index == choice_index
                        && draft.tool_call_index.is_none()
                        && draft.tool_call_id.is_some()
                        && tool_name.is_some()
                        && draft.tool_name.as_deref() == tool_name
                })
            }),
        (None, Some(id)) => drafts.iter().position(|draft| {
            draft.choice_index == choice_index && draft.tool_call_id.as_deref() == Some(id)
        }),
        (Some(index), Some(id)) => drafts.iter().position(|draft| {
            draft.choice_index == choice_index
                && (draft.tool_call_index == Some(index)
                    || draft.tool_call_id.as_deref() == Some(id))
        }),
        (None, None) => None,
    }
}

fn unique_chat_stream_draft_candidate(
    drafts: &[ChatStreamToolDraft],
    mut predicate: impl FnMut(&ChatStreamToolDraft) -> bool,
) -> Option<usize> {
    let mut candidate = None;
    for (index, draft) in drafts.iter().enumerate() {
        if !predicate(draft) {
            continue;
        }
        if candidate.is_some() {
            return None;
        }
        candidate = Some(index);
    }
    candidate
}

fn register_chat_stream_key_aliases(
    draft_indexes: &mut HashMap<ChatStreamToolKey, usize>,
    keys: &[ChatStreamToolKey],
    draft_index: usize,
) {
    for key in keys {
        draft_indexes.insert(key.clone(), draft_index);
    }
}

fn chat_stream_tool_keys(
    choice_index: u32,
    tool_call_index: Option<u32>,
    tool_call_id: Option<&str>,
) -> Vec<ChatStreamToolKey> {
    let mut keys = Vec::with_capacity(2);
    if let Some(tool_call_index) = tool_call_index {
        keys.push(ChatStreamToolKey::Indexed {
            choice_index,
            tool_call_index,
        });
    }

    if let Some(tool_call_id) = tool_call_id {
        keys.push(ChatStreamToolKey::ProviderId {
            choice_index,
            tool_call_id: tool_call_id.to_string(),
        });
    }

    keys
}

fn append_chat_stream_arguments(draft: &mut ChatStreamToolDraft, arguments: &str) {
    let current_chars = draft.arguments.chars().count();
    if current_chars >= MAX_CHAT_STREAM_ARGUMENT_CHARS_PER_TOOL {
        draft.arguments_truncated = true;
        return;
    }

    let remaining = MAX_CHAT_STREAM_ARGUMENT_CHARS_PER_TOOL - current_chars;
    let mut argument_chars = arguments.chars();
    for ch in argument_chars.by_ref().take(remaining) {
        draft.arguments.push(ch);
    }
    if argument_chars.next().is_some() {
        draft.arguments_truncated = true;
    }
}

fn observed_chat_stream_tool_call(
    request_id: uuid::Uuid,
    draft: ChatStreamToolDraft,
) -> ObservedChatCompletionStreamToolCall {
    let arguments_summary =
        (!draft.arguments.is_empty()).then(|| summarize_arguments(&draft.arguments));
    let arguments_hash = (!draft.arguments.is_empty()).then(|| stable_hash(&draft.arguments));
    let arguments_chars = (!draft.arguments.is_empty())
        .then(|| u64::try_from(draft.arguments.chars().count()).unwrap_or(u64::MAX));
    let arguments_valid_json = (!draft.arguments.is_empty())
        .then(|| serde_json::from_str::<Value>(&draft.arguments).is_ok());
    let aggregation_status = if draft.arguments_truncated {
        ChatStreamAggregationStatus::Partial
    } else if draft.finish_reason.as_deref() == Some("tool_calls") || draft.sse_done_seen {
        ChatStreamAggregationStatus::Complete
    } else {
        ChatStreamAggregationStatus::Partial
    };
    let provider_call_id_missing = draft.tool_call_id.is_none();
    let tool_name_missing = draft.tool_name.is_none();
    let tool_call_id = draft.tool_call_id.clone();
    let tool_name = draft.tool_name.clone();
    let tool_type = draft.tool_type.clone();
    let finish_reason = draft.finish_reason.clone();
    let key_part = draft
        .tool_call_index
        .map(|index| format!("index:{index}"))
        .or_else(|| draft.tool_call_id.as_ref().map(|id| format!("id:{id}")))
        .unwrap_or_else(|| "unknown".to_string());
    let idempotency_key = format!(
        "{request_id}:chat_completions:{}:{key_part}",
        draft.choice_index
    );
    let metadata = serde_json::json!({
        "observer": "chat_completions_stream_tool_observer",
        "source_api": "chat_completions",
        "source_wire": "chat_completions_sse",
        "observed_only": true,
        "execution_owner": "model_output",
        "executed_by_gateway": false,
        "runtime_confirmed": false,
        "policy_evaluated": false,
        "provider_call_id": tool_call_id.clone(),
        "provider_call_id_missing": provider_call_id_missing,
        "choice_index": draft.choice_index,
        "tool_call_index": draft.tool_call_index,
        "tool_name_missing": tool_name_missing,
        "chunk_count": draft.chunk_count,
        "finish_reason": finish_reason.clone(),
        "sse_done_seen": draft.sse_done_seen,
        "aggregation_status": aggregation_status.as_str(),
        "arguments_summary": arguments_summary.clone(),
        "arguments_hash": arguments_hash.clone(),
        "arguments_chars": arguments_chars,
        "arguments_valid_json": arguments_valid_json,
        "arguments_truncated": draft.arguments_truncated,
        "idempotency_key": idempotency_key.clone(),
    });

    ObservedChatCompletionStreamToolCall {
        choice_index: draft.choice_index,
        tool_call_index: draft.tool_call_index,
        tool_call_id,
        tool_name,
        tool_type,
        arguments_summary,
        arguments_hash,
        arguments_chars,
        arguments_valid_json,
        arguments_truncated: draft.arguments_truncated,
        chunk_count: draft.chunk_count,
        finish_reason,
        sse_done_seen: draft.sse_done_seen,
        aggregation_status,
        idempotency_key,
        metadata,
    }
}

#[must_use]
pub fn observe_responses_nonstream_agent_items(body: &[u8]) -> Vec<ObservedResponsesItem> {
    if body.len() > MAX_OBSERVER_BODY_BYTES {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let response_id = optional_string(value.get("id"));
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut observed = Vec::new();

    for (output_index, item) in output.iter().take(MAX_RESPONSE_OUTPUT_ITEMS).enumerate() {
        if observed.len() >= MAX_OBSERVED_EVENTS_PER_RESPONSE {
            break;
        }

        let item_type = optional_string(item.get("type"));
        let output_index_u32 = u32::try_from(output_index).ok();
        let item_id = optional_string(item.get("id"));
        let draft = match item_type.as_deref() {
            Some("function_call") => observe_responses_function_call(
                response_id.clone(),
                output_index_u32,
                item_id,
                item,
            ),
            Some("mcp_call") => {
                observe_responses_mcp_call(response_id.clone(), output_index_u32, item_id, item)
            }
            Some("reasoning") => {
                observe_responses_reasoning(response_id.clone(), output_index_u32, item_id, item)
            }
            _ => None,
        };

        let Some(draft) = draft else {
            continue;
        };
        let dedupe_key = responses_dedupe_key(&draft);
        if !seen.insert(dedupe_key) {
            continue;
        }
        observed.push(draft);
    }

    observed
}

#[must_use]
pub fn observe_responses_stream_agent_items(
    request_id: uuid::Uuid,
    body: &[u8],
) -> Vec<ObservedResponsesStreamItem> {
    if body.len() > MAX_OBSERVER_BODY_BYTES {
        return Vec::new();
    }

    let mut seen = HashMap::new();
    let mut observed = Vec::new();

    for frame in parse_sse_data_frames(body)
        .into_iter()
        .take(MAX_RESPONSE_STREAM_EVENTS)
    {
        if observed.len() >= MAX_OBSERVED_EVENTS_PER_RESPONSE {
            break;
        }
        if frame.trim() == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&frame) else {
            continue;
        };
        let Some(draft) = observe_responses_stream_event(request_id, &value) else {
            continue;
        };
        if let Some(existing_index) = seen.get(&draft.idempotency_key).copied() {
            merge_responses_stream_duplicate(&mut observed[existing_index], draft);
            continue;
        }
        seen.insert(draft.idempotency_key.clone(), observed.len());
        observed.push(draft);
    }

    observed
}

fn merge_responses_stream_duplicate(
    existing: &mut ObservedResponsesStreamItem,
    draft: ObservedResponsesStreamItem,
) {
    if existing.kind != ObservedResponsesStreamItemKind::FunctionCall
        || draft.kind != ObservedResponsesStreamItemKind::FunctionCall
    {
        return;
    }

    let draft_is_richer = responses_stream_function_call_richness(&draft)
        > responses_stream_function_call_richness(existing);
    if draft_is_richer {
        existing.sequence = draft.sequence;
    }

    if draft.output_index.is_some() {
        existing.output_index = draft.output_index;
    }
    if draft.response_id.is_some() {
        existing.response_id = draft.response_id.clone();
    }
    if draft.item_id.is_some() {
        existing.item_id = draft.item_id.clone();
    }
    if draft.call_id.is_some() {
        existing.call_id = draft.call_id.clone();
    }
    if draft
        .name
        .as_deref()
        .is_some_and(|name| name != "function_call")
        || existing.name.is_none()
    {
        existing.name = draft.name.clone();
    }
    if draft
        .status
        .as_deref()
        .is_some_and(|status| status != "arguments_done")
        || existing.status.is_none()
    {
        existing.status = draft.status.clone();
    }

    merge_function_call_metadata(existing, &draft);
}

fn responses_stream_function_call_richness(item: &ObservedResponsesStreamItem) -> u8 {
    u8::from(
        item.name
            .as_deref()
            .is_some_and(|name| name != "function_call"),
    ) + u8::from(item.call_id.is_some())
        + u8::from(
            item.status
                .as_deref()
                .is_some_and(|status| status != "arguments_done"),
        )
        + u8::from(item.output_index.is_some())
}

fn merge_function_call_metadata(
    existing: &mut ObservedResponsesStreamItem,
    draft: &ObservedResponsesStreamItem,
) {
    let Some(metadata) = existing.metadata.as_object_mut() else {
        return;
    };

    insert_non_null_metadata_value(metadata, &draft.metadata, "provider_response_id");
    insert_non_null_metadata_value(metadata, &draft.metadata, "provider_item_id");
    insert_non_null_metadata_value(metadata, &draft.metadata, "provider_call_id");
    insert_non_null_metadata_value(metadata, &draft.metadata, "output_index");

    if let Some(value) = draft
        .metadata
        .get("status")
        .filter(|value| !value.is_null())
        .filter(|value| {
            value.as_str() != Some("arguments_done") || !metadata.contains_key("status")
        })
    {
        metadata.insert("status".to_string(), value.clone());
    }

    for key in ["arguments_summary", "arguments_hash", "arguments_chars"] {
        let existing_has_value = metadata.get(key).is_some_and(|value| !value.is_null());
        if !existing_has_value {
            insert_non_null_metadata_value(metadata, &draft.metadata, key);
        }
    }
}

fn insert_non_null_metadata_value(
    metadata: &mut serde_json::Map<String, Value>,
    source: &Value,
    key: &str,
) {
    if let Some(value) = source.get(key).filter(|value| !value.is_null()) {
        metadata.insert(key.to_string(), value.clone());
    }
}

fn observe_responses_stream_event(
    request_id: uuid::Uuid,
    event: &Value,
) -> Option<ObservedResponsesStreamItem> {
    let event_type = optional_string(event.get("type"))?;

    match event_type.as_str() {
        "response.output_item.done" => {
            let item = event.get("item")?;
            match optional_string(item.get("type")).as_deref() {
                Some("function_call") => {
                    observe_responses_stream_function_call_from_item(request_id, event, item, None)
                }
                Some("mcp_call") => observe_responses_stream_mcp_call_from_item(
                    request_id,
                    event,
                    item,
                    "tool.call.observed",
                    ObservedResponsesStreamItemKind::McpCall,
                    optional_string(item.get("status")),
                ),
                _ => None,
            }
        }
        "response.function_call_arguments.done" => {
            observe_responses_stream_function_call_arguments_done(request_id, event)
        }
        "response.mcp_call.completed" => observe_responses_stream_mcp_call_from_item(
            request_id,
            event,
            event.get("item").unwrap_or(event),
            "tool.call.observed",
            ObservedResponsesStreamItemKind::McpCall,
            Some("completed".to_string()),
        ),
        "response.mcp_call.failed" => observe_responses_stream_mcp_call_from_item(
            request_id,
            event,
            event.get("item").unwrap_or(event),
            "error.observed",
            ObservedResponsesStreamItemKind::Error,
            Some("failed".to_string()),
        ),
        "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
            observe_responses_stream_reasoning(request_id, event)
        }
        "response.completed" => observe_responses_stream_response_completed(request_id, event),
        "response.failed" | "response.incomplete" => {
            observe_responses_stream_response_error(request_id, event)
        }
        _ => None,
    }
}

fn observe_responses_stream_function_call_from_item(
    request_id: uuid::Uuid,
    event: &Value,
    item: &Value,
    status_override: Option<String>,
) -> Option<ObservedResponsesStreamItem> {
    let response_id = responses_stream_response_id(event);
    let sequence = responses_stream_sequence(event);
    let output_index =
        optional_u32(event.get("output_index")).or_else(|| optional_u32(item.get("output_index")));
    let item_id = optional_string(item.get("id")).or_else(|| optional_string(event.get("item_id")));
    let call_id =
        optional_string(item.get("call_id")).or_else(|| optional_string(event.get("call_id")));
    let name = optional_string(item.get("name")).or_else(|| optional_string(event.get("name")))?;
    let status = status_override
        .or_else(|| optional_string(item.get("status")))
        .or_else(|| optional_string(event.get("status")));
    let arguments =
        optional_string(event.get("arguments")).or_else(|| optional_string(item.get("arguments")));
    let arguments_summary = arguments.as_deref().map(summarize_arguments);
    let arguments_hash = arguments.as_deref().map(stable_hash);
    let arguments_chars = arguments
        .as_deref()
        .map(|arguments| arguments.chars().count());
    let idempotency_key = responses_stream_function_call_idempotency_key(
        request_id,
        response_id.as_deref(),
        sequence,
        item_id.as_deref(),
        call_id.as_deref(),
    );

    Some(ObservedResponsesStreamItem {
        kind: ObservedResponsesStreamItemKind::FunctionCall,
        event_type: "tool.call.observed",
        response_id: response_id.clone(),
        sequence,
        output_index,
        item_id: item_id.clone(),
        call_id: call_id.clone(),
        name: Some(name),
        status: status.clone(),
        idempotency_key: idempotency_key.clone(),
        metadata: serde_json::json!({
            "observer": "responses_stream_observer",
            "source_api": "responses",
            "source_wire": "responses_sse",
            "event_source_trust": "gateway_observed",
            "provider_response_id": response_id,
            "provider_item_id": item_id,
            "provider_call_id": call_id,
            "output_index": output_index,
            "tool_type": "function",
            "observed_only": true,
            "status": status,
            "arguments_summary": arguments_summary,
            "arguments_hash": arguments_hash,
            "arguments_chars": arguments_chars,
            "idempotency_key": idempotency_key,
        }),
    })
}

fn observe_responses_stream_function_call_arguments_done(
    request_id: uuid::Uuid,
    event: &Value,
) -> Option<ObservedResponsesStreamItem> {
    let item = event.get("item").unwrap_or(event);
    let response_id = responses_stream_response_id(event);
    let sequence = responses_stream_sequence(event);
    let output_index =
        optional_u32(event.get("output_index")).or_else(|| optional_u32(item.get("output_index")));
    let item_id = optional_string(item.get("id")).or_else(|| optional_string(event.get("item_id")));
    let call_id =
        optional_string(item.get("call_id")).or_else(|| optional_string(event.get("call_id")));
    let name = optional_string(event.get("name"))
        .or_else(|| optional_string(item.get("name")))
        .unwrap_or_else(|| "function_call".to_string());
    let status = Some("arguments_done".to_string());
    let arguments =
        optional_string(event.get("arguments")).or_else(|| optional_string(item.get("arguments")));
    let arguments_summary = arguments.as_deref().map(summarize_arguments);
    let arguments_hash = arguments.as_deref().map(stable_hash);
    let arguments_chars = arguments
        .as_deref()
        .map(|arguments| arguments.chars().count());
    let idempotency_key = responses_stream_function_call_idempotency_key(
        request_id,
        response_id.as_deref(),
        sequence,
        item_id.as_deref(),
        call_id.as_deref(),
    );

    Some(ObservedResponsesStreamItem {
        kind: ObservedResponsesStreamItemKind::FunctionCall,
        event_type: "tool.call.observed",
        response_id: response_id.clone(),
        sequence,
        output_index,
        item_id: item_id.clone(),
        call_id: call_id.clone(),
        name: Some(name),
        status: status.clone(),
        idempotency_key: idempotency_key.clone(),
        metadata: serde_json::json!({
            "observer": "responses_stream_observer",
            "source_api": "responses",
            "source_wire": "responses_sse",
            "event_source_trust": "gateway_observed",
            "provider_response_id": response_id,
            "provider_item_id": item_id,
            "provider_call_id": call_id,
            "output_index": output_index,
            "tool_type": "function",
            "observed_only": true,
            "status": status,
            "arguments_summary": arguments_summary,
            "arguments_hash": arguments_hash,
            "arguments_chars": arguments_chars,
            "idempotency_key": idempotency_key,
        }),
    })
}

fn observe_responses_stream_mcp_call_from_item(
    request_id: uuid::Uuid,
    event: &Value,
    item: &Value,
    event_type: &'static str,
    kind: ObservedResponsesStreamItemKind,
    status_override: Option<String>,
) -> Option<ObservedResponsesStreamItem> {
    let response_id = responses_stream_response_id(event);
    let sequence = responses_stream_sequence(event);
    let output_index =
        optional_u32(event.get("output_index")).or_else(|| optional_u32(item.get("output_index")));
    let item_id = optional_string(item.get("id")).or_else(|| optional_string(event.get("item_id")));
    let call_id = optional_string(item.get("call_id"))
        .or_else(|| optional_string(event.get("call_id")))
        .or_else(|| item_id.as_ref().map(|id| format!("openai_item:{id}")));
    let name = optional_string(item.get("name"))
        .or_else(|| optional_string(event.get("name")))
        .or_else(|| optional_string(item.get("server_label")))
        .or_else(|| optional_string(event.get("server_label")));
    let status = status_override
        .or_else(|| optional_string(item.get("status")))
        .or_else(|| optional_string(event.get("status")));
    let server_label = optional_string(item.get("server_label"))
        .or_else(|| optional_string(event.get("server_label")));
    let output = item.get("output").or_else(|| event.get("output"));
    let has_output = output.is_some();
    let output_chars = output.map(value_char_count);
    let has_error = item.get("error").is_some()
        || event.get("error").is_some()
        || status.as_deref() == Some("failed");
    let idempotency_key = responses_stream_idempotency_key(
        request_id,
        event_type,
        response_id.as_deref(),
        sequence,
        item_id.as_deref(),
        call_id.as_deref(),
        name.as_deref(),
    );

    Some(ObservedResponsesStreamItem {
        kind,
        event_type,
        response_id: response_id.clone(),
        sequence,
        output_index,
        item_id: item_id.clone(),
        call_id: call_id.clone(),
        name,
        status: status.clone(),
        idempotency_key: idempotency_key.clone(),
        metadata: serde_json::json!({
            "observer": "responses_stream_observer",
            "source_api": "responses",
            "source_wire": "responses_sse",
            "event_source_trust": "gateway_observed",
            "provider_response_id": response_id,
            "provider_item_id": item_id,
            "provider_call_id": call_id,
            "output_index": output_index,
            "tool_type": "mcp",
            "execution_owner": "provider_hosted",
            "provider_execution_status": status,
            "server_label": server_label,
            "has_output": has_output,
            "output_chars": output_chars,
            "has_error": has_error,
            "idempotency_key": idempotency_key,
        }),
    })
}

fn observe_responses_stream_reasoning(
    request_id: uuid::Uuid,
    event: &Value,
) -> Option<ObservedResponsesStreamItem> {
    let response_id = responses_stream_response_id(event);
    let sequence = responses_stream_sequence(event);
    let output_index = optional_u32(event.get("output_index"));
    let item_id =
        optional_string(event.get("item_id")).or_else(|| optional_string(event.get("id")));
    let status = optional_string(event.get("status"));
    let text = optional_string(event.get("text"));
    let summary_chars = text
        .as_deref()
        .map(|text| text.chars().count().min(MAX_REASONING_SUMMARY_CHARS));
    let has_reasoning_text = text.is_some();
    if !has_reasoning_text {
        return None;
    }
    let idempotency_key = responses_stream_idempotency_key(
        request_id,
        "llm.reasoning.observed",
        response_id.as_deref(),
        sequence,
        item_id.as_deref(),
        None,
        None,
    );

    Some(ObservedResponsesStreamItem {
        kind: ObservedResponsesStreamItemKind::Reasoning,
        event_type: "llm.reasoning.observed",
        response_id: response_id.clone(),
        sequence,
        output_index,
        item_id: item_id.clone(),
        call_id: None,
        name: Some("Reasoning text observed".to_string()),
        status: status.clone(),
        idempotency_key: idempotency_key.clone(),
        metadata: serde_json::json!({
            "observer": "responses_stream_observer",
            "source_api": "responses",
            "source_wire": "responses_sse",
            "event_source_trust": "gateway_observed",
            "provider_response_id": response_id,
            "provider_item_id": item_id,
            "output_index": output_index,
            "reasoning_visibility": "metadata_only",
            "has_reasoning_text": has_reasoning_text,
            "summary_chars": summary_chars,
            "idempotency_key": idempotency_key,
        }),
    })
}

fn observe_responses_stream_response_completed(
    request_id: uuid::Uuid,
    event: &Value,
) -> Option<ObservedResponsesStreamItem> {
    let response_id = responses_stream_response_id(event);
    let sequence = responses_stream_sequence(event);
    let status = optional_string(event.get("status")).or_else(|| Some("completed".to_string()));
    let idempotency_key = responses_stream_idempotency_key(
        request_id,
        "llm.response.completed.observed",
        response_id.as_deref(),
        sequence,
        None,
        None,
        None,
    );

    Some(ObservedResponsesStreamItem {
        kind: ObservedResponsesStreamItemKind::ResponseCompleted,
        event_type: "llm.response.completed.observed",
        response_id: response_id.clone(),
        sequence,
        output_index: None,
        item_id: None,
        call_id: None,
        name: Some("Response completed".to_string()),
        status: status.clone(),
        idempotency_key: idempotency_key.clone(),
        metadata: serde_json::json!({
            "observer": "responses_stream_observer",
            "source_api": "responses",
            "source_wire": "responses_sse",
            "event_source_trust": "gateway_observed",
            "provider_response_id": response_id,
            "status": status,
            "idempotency_key": idempotency_key,
        }),
    })
}

fn observe_responses_stream_response_error(
    request_id: uuid::Uuid,
    event: &Value,
) -> Option<ObservedResponsesStreamItem> {
    let event_kind = optional_string(event.get("type"))?;
    let response_id = responses_stream_response_id(event);
    let sequence = responses_stream_sequence(event);
    let status = if event_kind == "response.incomplete" {
        "incomplete"
    } else {
        "failed"
    }
    .to_string();
    let has_error = event.get("error").is_some();
    let has_incomplete_details = event.get("incomplete_details").is_some();
    let idempotency_key = responses_stream_idempotency_key(
        request_id,
        "error.observed",
        response_id.as_deref(),
        sequence,
        None,
        None,
        Some(&event_kind),
    );

    Some(ObservedResponsesStreamItem {
        kind: ObservedResponsesStreamItemKind::Error,
        event_type: "error.observed",
        response_id: response_id.clone(),
        sequence,
        output_index: None,
        item_id: None,
        call_id: None,
        name: Some(event_kind),
        status: Some(status.clone()),
        idempotency_key: idempotency_key.clone(),
        metadata: serde_json::json!({
            "observer": "responses_stream_observer",
            "source_api": "responses",
            "source_wire": "responses_sse",
            "event_source_trust": "gateway_observed",
            "provider_response_id": response_id,
            "provider_execution_status": status,
            "has_error": has_error,
            "has_incomplete_details": has_incomplete_details,
            "idempotency_key": idempotency_key,
        }),
    })
}

fn observe_responses_function_call(
    response_id: Option<String>,
    output_index: Option<u32>,
    item_id: Option<String>,
    item: &Value,
) -> Option<ObservedResponsesItem> {
    let name = optional_string(item.get("name"))?;
    let call_id = optional_string(item.get("call_id"));
    let status = optional_string(item.get("status"));
    let arguments_summary =
        optional_string(item.get("arguments")).map(|arguments| summarize_arguments(&arguments));

    Some(ObservedResponsesItem {
        kind: ObservedResponsesItemKind::FunctionCall,
        event_type: "tool.call.observed",
        response_id,
        output_index,
        item_id,
        call_id,
        name: Some(name),
        status: status.clone(),
        metadata: serde_json::json!({
            "observer": "responses_nonstream_agent_observer",
            "source_api": "responses",
            "tool_type": "function",
            "status": status,
            "arguments_summary": arguments_summary,
        }),
    })
}

fn observe_responses_mcp_call(
    response_id: Option<String>,
    output_index: Option<u32>,
    item_id: Option<String>,
    item: &Value,
) -> Option<ObservedResponsesItem> {
    let name =
        optional_string(item.get("name")).or_else(|| optional_string(item.get("server_label")))?;
    let status = optional_string(item.get("status"));
    let server_label = optional_string(item.get("server_label"));
    let has_output = item.get("output").is_some();
    let output_chars = item
        .get("output")
        .and_then(Value::as_str)
        .map(|value| value.chars().count());
    let has_error = item.get("error").is_some();

    Some(ObservedResponsesItem {
        kind: ObservedResponsesItemKind::McpCall,
        event_type: "tool.call.observed",
        response_id,
        output_index,
        item_id,
        call_id: optional_string(item.get("call_id")),
        name: Some(name),
        status: status.clone(),
        metadata: serde_json::json!({
            "observer": "responses_nonstream_agent_observer",
            "source_api": "responses",
            "tool_type": "mcp",
            "execution_owner": "provider_hosted",
            "server_label": server_label,
            "status": status,
            "has_output": has_output,
            "output_chars": output_chars,
            "has_error": has_error,
        }),
    })
}

fn observe_responses_reasoning(
    response_id: Option<String>,
    output_index: Option<u32>,
    item_id: Option<String>,
    item: &Value,
) -> Option<ObservedResponsesItem> {
    let summary = item
        .get("summary")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let summary_count = summary.len();
    let summary_chars = summary
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .map(str::chars)
        .map(Iterator::count)
        .sum::<usize>()
        .min(MAX_REASONING_SUMMARY_CHARS);
    let has_encrypted_content = item.get("encrypted_content").is_some();

    if summary_count == 0 && !has_encrypted_content {
        return None;
    }

    Some(ObservedResponsesItem {
        kind: ObservedResponsesItemKind::Reasoning,
        event_type: "llm.reasoning.observed",
        response_id,
        output_index,
        item_id,
        call_id: None,
        name: Some("Reasoning summary observed".to_string()),
        status: optional_string(item.get("status")),
        metadata: serde_json::json!({
            "observer": "responses_nonstream_agent_observer",
            "source_api": "responses",
            "reasoning_visibility": "metadata_only",
            "summary_count": summary_count,
            "summary_chars": summary_chars,
            "has_encrypted_content": has_encrypted_content,
        }),
    })
}

fn responses_dedupe_key(item: &ObservedResponsesItem) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        item.event_type,
        item.response_id.as_deref().unwrap_or_default(),
        item.output_index
            .map(|index| index.to_string())
            .unwrap_or_default(),
        item.item_id.as_deref().unwrap_or_default(),
        item.call_id.as_deref().unwrap_or_default(),
    )
}

fn parse_sse_data_frames(body: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(body);
    let mut frames = Vec::new();
    let mut data_lines = Vec::new();

    for line in text.lines() {
        if line.is_empty() {
            if !data_lines.is_empty() {
                frames.push(data_lines.join("\n"));
                data_lines.clear();
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        data_lines.push(data.strip_prefix(' ').unwrap_or(data).to_string());
    }

    if !data_lines.is_empty() {
        frames.push(data_lines.join("\n"));
    }

    frames
}

fn responses_stream_response_id(event: &Value) -> Option<String> {
    optional_string(event.get("response_id")).or_else(|| {
        event
            .get("response")
            .and_then(|response| optional_string(response.get("id")))
    })
}

fn responses_stream_sequence(event: &Value) -> u32 {
    optional_u32(event.get("sequence_number"))
        .or_else(|| optional_u32(event.get("sequence")))
        .unwrap_or_default()
}

fn responses_stream_idempotency_key(
    request_id: uuid::Uuid,
    event_type: &str,
    response_id: Option<&str>,
    sequence: u32,
    item_id: Option<&str>,
    call_id: Option<&str>,
    name: Option<&str>,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        request_id,
        event_type,
        response_id.unwrap_or_default(),
        sequence,
        item_id.unwrap_or_default(),
        call_id.unwrap_or_default(),
        name.unwrap_or_default(),
    )
}

fn responses_stream_function_call_idempotency_key(
    request_id: uuid::Uuid,
    response_id: Option<&str>,
    sequence: u32,
    item_id: Option<&str>,
    call_id: Option<&str>,
) -> String {
    if let Some(item_id) = item_id {
        return format!(
            "{}:function_call:{}:item:{}",
            request_id,
            response_id.unwrap_or_default(),
            item_id
        );
    }
    if let Some(call_id) = call_id {
        return format!(
            "{}:function_call:{}:call:{}",
            request_id,
            response_id.unwrap_or_default(),
            call_id
        );
    }
    format!(
        "{}:function_call:{}:sequence:{}",
        request_id,
        response_id.unwrap_or_default(),
        sequence
    )
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_u32(value: Option<&Value>) -> Option<u32> {
    value.and_then(|value| {
        value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|value| value.trim().parse::<u32>().ok())
            })
    })
}

fn summarize_arguments(arguments: &str) -> String {
    let redacted = serde_json::from_str::<Value>(arguments)
        .map(|mut value| {
            redact_sensitive_json(&mut value);
            serde_json::to_string(&value)
                .unwrap_or_else(|_| "[unserializable_json_arguments]".to_string())
        })
        .unwrap_or_else(|_| "[non_json_arguments]".to_string());
    truncate_chars(&redacted, MAX_ARGUMENTS_SUMMARY_CHARS)
}

fn redact_sensitive_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_sensitive_key(key) {
                    *value = Value::String("[redacted]".to_string());
                } else {
                    redact_sensitive_json(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_sensitive_json(value);
            }
        }
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| !matches!(ch, '_' | '-' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "password"
            | "token"
            | "secret"
            | "apikey"
            | "authorization"
            | "credential"
            | "accesstoken"
            | "refreshtoken"
            | "clientsecret"
            | "privatekey"
    ) || normalized.contains("password")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("apikey")
        || normalized.contains("authorization")
        || normalized.contains("credential")
        || normalized.contains("privatekey")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn stable_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut hash, "{byte:02x}");
    }
    hash
}

fn value_char_count(value: &Value) -> usize {
    value
        .as_str()
        .map(str::chars)
        .map(Iterator::count)
        .unwrap_or_else(|| value.to_string().chars().count())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    fn chat_sse_frame(value: serde_json::Value) -> String {
        format!("data: {value}\n\n")
    }

    fn chat_sse_done() -> String {
        "data: [DONE]\n\n".to_string()
    }

    #[test]
    fn observes_chat_completion_function_tool_call() {
        let body = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_123",
                        "type": "function",
                        "function": {
                            "name": "zendesk.get_ticket",
                            "arguments": "{\"ticket_id\":\"T-1\"}"
                        }
                    }]
                }
            }]
        });

        let observed = super::observe_chat_completion_tool_calls(body.to_string().as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(observed[0].tool_name, "zendesk.get_ticket");
        assert_eq!(observed[0].tool_type.as_deref(), Some("function"));
        assert_eq!(
            observed[0].arguments_summary.as_deref(),
            Some("{\"ticket_id\":\"T-1\"}")
        );
        assert_eq!(observed[0].choice_index, Some(0));
    }

    #[test]
    fn observes_multiple_choices_and_deduplicates_tool_call_ids() {
        let body = json!({
            "choices": [
                {
                    "message": {
                        "tool_calls": [{
                            "id": "call_dup",
                            "type": "function",
                            "function": {
                                "name": "search",
                                "arguments": "{\"q\":\"a\"}"
                            }
                        }]
                    }
                },
                {
                    "message": {
                        "tool_calls": [{
                            "id": "call_dup",
                            "type": "function",
                            "function": {
                                "name": "search",
                                "arguments": "{\"q\":\"a\"}"
                            }
                        }, {
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "summarize",
                                "arguments": "{\"id\":\"2\"}"
                            }
                        }]
                    }
                }
            ]
        });

        let observed = super::observe_chat_completion_tool_calls(body.to_string().as_bytes());

        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_dup"));
        assert_eq!(observed[1].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(observed[1].choice_index, Some(1));
    }

    #[test]
    fn skips_invalid_json_toolless_bodies_and_unnamed_tools() {
        assert!(super::observe_chat_completion_tool_calls(b"not-json").is_empty());

        let no_tools = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "No tool call"
                }
            }]
        });
        assert!(
            super::observe_chat_completion_tool_calls(no_tools.to_string().as_bytes(),).is_empty()
        );

        let unnamed = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_no_name",
                        "type": "function",
                        "function": {
                            "arguments": "{\"q\":\"a\"}"
                        }
                    }]
                }
            }]
        });
        assert!(
            super::observe_chat_completion_tool_calls(unnamed.to_string().as_bytes(),).is_empty()
        );
    }

    #[test]
    fn redacts_sensitive_argument_keys_and_truncates_long_arguments() {
        let long = "x".repeat(2_000);
        let body = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_secret",
                        "type": "function",
                        "function": {
                            "name": "external.lookup",
                            "arguments": serde_json::to_string(&json!({
                                "query": long,
                                "api_key": "sk-test",
                                "nested": {
                                    "authorization": "Bearer abc"
                                }
                            })).unwrap()
                        }
                    }]
                }
            }]
        });

        let observed = super::observe_chat_completion_tool_calls(body.to_string().as_bytes());
        let summary = observed[0].arguments_summary.as_deref().unwrap();

        assert!(summary.len() <= super::MAX_ARGUMENTS_SUMMARY_CHARS);
        assert!(!summary.contains("sk-test"));
        assert!(!summary.contains("Bearer abc"));
        assert!(summary.contains("[redacted]"));
    }

    #[test]
    fn observes_chat_completion_stream_tool_call_chunks() {
        let body = [
            chat_sse_frame(json!({
                "id": "chatcmpl_1",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_123",
                            "type": "function",
                            "function": {
                                "name": "search_docs",
                                "arguments": ""
                            }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "id": "chatcmpl_1",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "{\"" }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "id": "chatcmpl_1",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "query" }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "id": "chatcmpl_1",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "arguments": "\":\"agent gateway observer\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let request_id =
            uuid::Uuid::parse_str("01890f5a-52fd-7b9a-b51e-33a22f7b6f24").expect("uuid");
        let observed =
            super::observe_chat_completion_stream_tool_calls(request_id, body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].choice_index, 0);
        assert_eq!(observed[0].tool_call_index, Some(0));
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(observed[0].tool_name.as_deref(), Some("search_docs"));
        assert_eq!(observed[0].tool_type.as_deref(), Some("function"));
        assert_eq!(
            observed[0].arguments_summary.as_deref(),
            Some("{\"query\":\"agent gateway observer\"}")
        );
        assert_eq!(observed[0].arguments_chars, Some(34));
        assert_eq!(observed[0].arguments_valid_json, Some(true));
        assert!(!observed[0].arguments_truncated);
        assert_eq!(observed[0].chunk_count, 4);
        assert_eq!(observed[0].finish_reason.as_deref(), Some("tool_calls"));
        assert!(observed[0].sse_done_seen);
        assert_eq!(
            observed[0].aggregation_status,
            super::ChatStreamAggregationStatus::Complete
        );
        assert_eq!(
            observed[0].metadata["observer"].as_str(),
            Some("chat_completions_stream_tool_observer")
        );
        assert_eq!(
            observed[0].metadata["source_wire"].as_str(),
            Some("chat_completions_sse")
        );
        assert_eq!(observed[0].metadata["observed_only"].as_bool(), Some(true));
        assert_eq!(
            observed[0].metadata["execution_owner"].as_str(),
            Some("model_output")
        );
        assert_eq!(
            observed[0].idempotency_key,
            "01890f5a-52fd-7b9a-b51e-33a22f7b6f24:chat_completions:0:index:0"
        );
    }

    #[test]
    fn chat_completion_stream_observer_keeps_parallel_tool_calls_separate() {
        let body = [
            chat_sse_frame(json!({
                "id": "chatcmpl_parallel",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "call_a",
                                "type": "function",
                                "function": {
                                    "name": "search_docs",
                                    "arguments": "{\"q\":\"a\"}"
                                }
                            },
                            {
                                "index": 1,
                                "id": "call_b",
                                "type": "function",
                                "function": {
                                    "name": "lookup_ticket",
                                    "arguments": "{\"id\":\"T-1\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].tool_call_index, Some(0));
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(observed[0].tool_name.as_deref(), Some("search_docs"));
        assert_eq!(observed[1].tool_call_index, Some(1));
        assert_eq!(observed[1].tool_call_id.as_deref(), Some("call_b"));
        assert_eq!(observed[1].tool_name.as_deref(), Some("lookup_ticket"));
    }

    #[test]
    fn chat_completion_stream_observer_skips_unkeyed_fragments() {
        let body = [
            chat_sse_frame(json!({
                "id": "chatcmpl_missing",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "function": { "arguments": "secret=raw" }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "id": "chatcmpl_missing",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "id": "call_with_id",
                            "function": {
                                "name": "unknown_index",
                                "arguments": "{\"safe\":true}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].tool_call_index, None);
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_with_id"));
        assert_eq!(observed[0].tool_name.as_deref(), Some("unknown_index"));
        assert!(
            !observed[0].metadata.to_string().contains("secret=raw"),
            "unkeyed raw fragment must not leak into metadata"
        );
    }

    #[test]
    fn chat_completion_stream_observer_aliases_provider_id_to_index_key() {
        let body = [
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "id": "call_alias",
                            "function": {
                                "name": "lookup",
                                "arguments": "{\"q\":"
                            }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "type": "function",
                            "function": {
                                "name": "lookup",
                                "arguments": "\"abc\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_alias"));
        assert_eq!(observed[0].tool_call_index, Some(0));
        assert_eq!(observed[0].tool_name.as_deref(), Some("lookup"));
        assert_eq!(
            observed[0].arguments_summary.as_deref(),
            Some("{\"q\":\"abc\"}")
        );
        assert_eq!(
            observed[0].idempotency_key,
            "00000000-0000-0000-0000-000000000000:chat_completions:0:index:0"
        );
    }

    #[test]
    fn chat_completion_stream_observer_does_not_alias_provider_id_to_different_index_name() {
        let body = [
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "id": "call_first",
                            "function": {
                                "name": "first",
                                "arguments": "{\"a\":1}"
                            }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "name": "second",
                                "arguments": "{\"b\":2}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].tool_name.as_deref(), Some("first"));
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_first"));
        assert_eq!(observed[0].tool_call_index, None);
        assert_eq!(observed[1].tool_name.as_deref(), Some("second"));
        assert_eq!(observed[1].tool_call_id, None);
        assert_eq!(observed[1].tool_call_index, Some(0));
    }

    #[test]
    fn chat_completion_stream_observer_aliases_index_key_with_provider_id_bridge() {
        let body = [
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "type": "function",
                            "function": {
                                "name": "lookup",
                                "arguments": "{\"q\":"
                            }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_alias",
                            "function": { "arguments": "\"abc\"}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_alias"));
        assert_eq!(observed[0].tool_call_index, Some(0));
        assert_eq!(observed[0].tool_name.as_deref(), Some("lookup"));
        assert_eq!(
            observed[0].arguments_summary.as_deref(),
            Some("{\"q\":\"abc\"}")
        );
        assert_eq!(
            observed[0].idempotency_key,
            "00000000-0000-0000-0000-000000000000:chat_completions:0:index:0"
        );
    }

    #[test]
    fn chat_completion_stream_observer_does_not_merge_new_provider_id_into_index_only_draft() {
        let body = [
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "type": "function",
                            "function": {
                                "name": "first",
                                "arguments": "{\"a\":1}"
                            }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "id": "call_second",
                            "function": {
                                "name": "second",
                                "arguments": "{\"b\":2}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].tool_name.as_deref(), Some("first"));
        assert_eq!(observed[0].tool_call_id, None);
        assert_eq!(observed[0].tool_call_index, Some(0));
        assert_eq!(observed[1].tool_name.as_deref(), Some("second"));
        assert_eq!(observed[1].tool_call_id.as_deref(), Some("call_second"));
        assert_eq!(observed[1].tool_call_index, None);
    }

    #[test]
    fn chat_completion_stream_observer_redacts_and_hides_non_json_arguments() {
        let body = [
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_secret",
                            "function": {
                                "name": "external.lookup",
                                "arguments": "{\"api_key\":\"sk-test\",\"query\":\"safe\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 1,
                            "id": "call_non_json",
                            "function": {
                                "name": "raw.lookup",
                                "arguments": "token=sk-test&password=hunter2"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 2);
        let json_summary = observed[0].arguments_summary.as_deref().unwrap();
        assert!(json_summary.contains("[redacted]"));
        assert!(!json_summary.contains("sk-test"));
        let non_json_summary = observed[1].arguments_summary.as_deref().unwrap();
        assert_eq!(non_json_summary, "[non_json_arguments]");
        assert!(!observed[1].metadata.to_string().contains("hunter2"));
    }

    #[test]
    fn chat_completion_stream_observer_preserves_raw_argument_whitespace_chunks() {
        let body = [
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_spaces",
                            "function": {
                                "name": "lookup",
                                "arguments": "{\"query\":"
                            }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": " " }
                        }]
                    }
                }]
            })),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "\"agent\"}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })),
            chat_sse_done(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].arguments_summary.as_deref(),
            Some("{\"query\":\"agent\"}")
        );
        assert_eq!(observed[0].arguments_valid_json, Some(true));
    }

    #[test]
    fn chat_completion_stream_observer_redacts_common_sensitive_key_variants() {
        let body = chat_sse_frame(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_sensitive_variants",
                        "function": {
                            "name": "lookup",
                            "arguments": serde_json::json!({
                                "access_token": "access-secret",
                                "RefreshToken": "refresh-secret",
                                "clientSecret": "client-secret",
                                "private_key": "private-secret",
                                "apiKey": "api-secret",
                                "query": "safe"
                            }).to_string()
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        let summary = observed[0].arguments_summary.as_deref().unwrap();
        assert!(summary.contains("\"query\":\"safe\""));
        assert_eq!(summary.matches("[redacted]").count(), 5);
        assert!(!summary.contains("access-secret"));
        assert!(!summary.contains("refresh-secret"));
        assert!(!summary.contains("client-secret"));
        assert!(!summary.contains("private-secret"));
        assert!(!summary.contains("api-secret"));
    }

    #[test]
    fn chat_completion_stream_observer_supports_crlf_multiline_done_and_malformed() {
        let body = [
            ": keepalive\r\n".to_string(),
            "data: {not-json}\r\n\r\n".to_string(),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_multi",
                            "function": {
                                "name": "multi.tool",
                                "arguments": "{\"q\":"
                            }
                        }]
                    }
                }]
            }))
            .replace('\n', "\r\n"),
            chat_sse_frame(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "arguments": "\"abc\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }))
            .replace('\n', "\r\n"),
            "data: [DONE]\r\n\r\n".to_string(),
        ]
        .concat();

        let observed =
            super::observe_chat_completion_stream_tool_calls(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].tool_call_id.as_deref(), Some("call_multi"));
        assert_eq!(observed[0].tool_name.as_deref(), Some("multi.tool"));
        assert_eq!(
            observed[0].arguments_summary.as_deref(),
            Some("{\"q\":\"abc\"}")
        );
        assert!(observed[0].sse_done_seen);
    }

    #[test]
    fn observes_responses_function_call_as_tool_call_observed() {
        let body = serde_json::json!({
            "id": "resp_1",
            "model": "gpt-4.1",
            "output": [{
                "id": "fc_1",
                "type": "function_call",
                "call_id": "call_123",
                "name": "zendesk.get_ticket",
                "arguments": "{\"ticket_id\":\"T-1\",\"api_key\":\"sk-test\"}",
                "status": "completed"
            }]
        });

        let observed = super::observe_responses_nonstream_agent_items(body.to_string().as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesItemKind::FunctionCall
        );
        assert_eq!(observed[0].event_type, "tool.call.observed");
        assert_eq!(observed[0].response_id.as_deref(), Some("resp_1"));
        assert_eq!(observed[0].output_index, Some(0));
        assert_eq!(observed[0].item_id.as_deref(), Some("fc_1"));
        assert_eq!(observed[0].call_id.as_deref(), Some("call_123"));
        assert_eq!(observed[0].name.as_deref(), Some("zendesk.get_ticket"));
        assert_eq!(observed[0].status.as_deref(), Some("completed"));
        assert_eq!(
            observed[0].metadata["observer"].as_str(),
            Some("responses_nonstream_agent_observer")
        );
        assert_eq!(observed[0].metadata["tool_type"].as_str(), Some("function"));
        assert!(
            !observed[0].metadata["arguments_summary"]
                .as_str()
                .unwrap()
                .contains("sk-test")
        );
    }

    #[test]
    fn observes_responses_mcp_call_without_raw_output() {
        let body = serde_json::json!({
            "id": "resp_mcp",
            "output": [{
                "id": "mcp_1",
                "type": "mcp_call",
                "name": "search_docs",
                "server_label": "docs",
                "status": "completed",
                "output": "sensitive provider-hosted output"
            }]
        });

        let observed = super::observe_responses_nonstream_agent_items(body.to_string().as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, super::ObservedResponsesItemKind::McpCall);
        assert_eq!(observed[0].event_type, "tool.call.observed");
        assert_eq!(observed[0].name.as_deref(), Some("search_docs"));
        assert_eq!(observed[0].metadata["tool_type"].as_str(), Some("mcp"));
        assert_eq!(
            observed[0].metadata["execution_owner"].as_str(),
            Some("provider_hosted")
        );
        assert_eq!(observed[0].metadata["server_label"].as_str(), Some("docs"));
        assert_eq!(observed[0].metadata["has_output"].as_bool(), Some(true));
        assert_eq!(
            observed[0]
                .metadata
                .to_string()
                .contains("sensitive provider-hosted output"),
            false
        );
    }

    #[test]
    fn observes_responses_reasoning_as_reasoning_observed() {
        let body = serde_json::json!({
            "id": "resp_reasoning",
            "output": [{
                "id": "rs_1",
                "type": "reasoning",
                "summary": [{
                    "type": "summary_text",
                    "text": "I should inspect the ticket first."
                }],
                "encrypted_content": "opaque-secret"
            }]
        });

        let observed = super::observe_responses_nonstream_agent_items(body.to_string().as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesItemKind::Reasoning
        );
        assert_eq!(observed[0].event_type, "llm.reasoning.observed");
        assert_eq!(
            observed[0].name.as_deref(),
            Some("Reasoning summary observed")
        );
        assert_eq!(observed[0].metadata["summary_count"].as_u64(), Some(1));
        assert_eq!(
            observed[0].metadata["has_encrypted_content"].as_bool(),
            Some(true)
        );
        assert_eq!(
            observed[0].metadata.to_string().contains("opaque-secret"),
            false
        );
    }

    #[test]
    fn responses_observer_skips_function_call_output_and_messages() {
        let body = serde_json::json!({
            "id": "resp_skip",
            "output": [
                {
                    "type": "message",
                    "content": [{ "type": "output_text", "text": "Done" }]
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_123",
                    "output": "tool result"
                }
            ]
        });

        let observed = super::observe_responses_nonstream_agent_items(body.to_string().as_bytes());

        assert!(observed.is_empty());
    }

    #[test]
    fn responses_observer_applies_size_and_event_limits() {
        let oversized = vec![b' '; super::MAX_OBSERVER_BODY_BYTES + 1];
        assert!(super::observe_responses_nonstream_agent_items(&oversized).is_empty());

        let output = (0..32)
            .map(|i| {
                serde_json::json!({
                    "id": format!("fc_{i}"),
                    "type": "function_call",
                    "call_id": format!("call_{i}"),
                    "name": "search",
                    "arguments": "{}"
                })
            })
            .collect::<Vec<_>>();
        let body = serde_json::json!({
            "id": "resp_many",
            "output": output
        });

        let observed = super::observe_responses_nonstream_agent_items(body.to_string().as_bytes());

        assert_eq!(observed.len(), super::MAX_OBSERVED_EVENTS_PER_RESPONSE);
    }

    #[test]
    fn observes_responses_stream_function_call_from_output_item_done() {
        let body = r#"data: {"type":"response.output_item.done","response_id":"resp_1","sequence_number":7,"output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_123","name":"zendesk.get_ticket","arguments":"{\"ticket_id\":\"T-1\",\"api_key\":\"sk-test\"}","status":"completed"}}

"#;

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesStreamItemKind::FunctionCall
        );
        assert_eq!(observed[0].event_type, "tool.call.observed");
        assert_eq!(observed[0].response_id.as_deref(), Some("resp_1"));
        assert_eq!(observed[0].sequence, 7);
        assert_eq!(observed[0].output_index, Some(0));
        assert_eq!(observed[0].item_id.as_deref(), Some("fc_1"));
        assert_eq!(observed[0].call_id.as_deref(), Some("call_123"));
        assert_eq!(observed[0].name.as_deref(), Some("zendesk.get_ticket"));
        assert_eq!(
            observed[0].metadata["arguments_summary"]
                .as_str()
                .unwrap()
                .contains("sk-test"),
            false
        );
        assert!(
            observed[0].metadata["arguments_summary"]
                .as_str()
                .unwrap()
                .contains("[redacted]")
        );
        assert!(observed[0].metadata["arguments_hash"].is_string());
        assert_eq!(
            observed[0].metadata["idempotency_key"].as_str(),
            Some(observed[0].idempotency_key.as_str())
        );
    }

    #[test]
    fn observes_responses_stream_function_call_arguments_done_without_name() {
        let body = concat!(
            "data: {\"type\":\"response.function_call_arguments.done\",\"\
             response_id\":\"resp_args\",",
            "\"sequence_number\":2,\"item_id\":\"fc_args\",",
            "\"arguments\":\"{\\\"api_key\\\":\\\"sk-test\\\"}\"}\n\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].event_type, "tool.call.observed");
        assert_eq!(observed[0].name.as_deref(), Some("function_call"));
        assert_eq!(observed[0].status.as_deref(), Some("arguments_done"));
        assert_eq!(observed[0].item_id.as_deref(), Some("fc_args"));
        let arguments_summary = observed[0].metadata["arguments_summary"].as_str().unwrap();
        assert!(!arguments_summary.contains("sk-test"));
        assert!(arguments_summary.contains("[redacted]"));
    }

    #[test]
    fn responses_stream_non_json_arguments_are_not_logged_raw() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.done\",\"response_id\":\"\
             resp_non_json\",",
            "\"sequence_number\":9,\"output_index\":0,",
            "\"item\":{\"id\":\"fc_non_json\",\"type\":\"function_call\",",
            "\"call_id\":\"call_non_json\",\"name\":\"custom.tool\",",
            "\"arguments\":\"token=sk-test&password=hunter2\",",
            "\"status\":\"completed\"}}\n\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        let arguments_summary = observed[0].metadata["arguments_summary"].as_str().unwrap();
        assert_eq!(arguments_summary, "[non_json_arguments]");
        assert!(!arguments_summary.contains("sk-test"));
        assert!(!observed[0].metadata.to_string().contains("hunter2"));
        assert!(observed[0].metadata["arguments_hash"].is_string());
        assert_eq!(observed[0].metadata["arguments_chars"].as_u64(), Some(30));
    }

    #[test]
    fn responses_stream_function_call_bridge_pair_dedupes_to_richer_output_item() {
        let body = concat!(
            "data: {\"type\":\"response.function_call_arguments.done\",\"\
             response_id\":\"resp_bridge\",",
            "\"sequence_number\":2,\"item_id\":\"fc_bridge\",",
            "\"arguments\":\"{\\\"ticket_id\\\":\\\"T-1\\\"}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"response_id\":\"\
             resp_bridge\",",
            "\"sequence_number\":3,\"output_index\":0,",
            "\"item\":{\"id\":\"fc_bridge\",\"type\":\"function_call\",",
            "\"call_id\":\"call_bridge\",\"name\":\"zendesk.get_ticket\",",
            "\"status\":\"completed\"}}\n\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesStreamItemKind::FunctionCall
        );
        assert_eq!(observed[0].item_id.as_deref(), Some("fc_bridge"));
        assert_eq!(observed[0].call_id.as_deref(), Some("call_bridge"));
        assert_eq!(observed[0].name.as_deref(), Some("zendesk.get_ticket"));
        assert_eq!(observed[0].status.as_deref(), Some("completed"));
        assert_eq!(
            observed[0].metadata["arguments_summary"].as_str(),
            Some("{\"ticket_id\":\"T-1\"}")
        );
    }

    #[test]
    fn responses_stream_function_call_duplicate_frames_do_not_duplicate_events() {
        let frame = concat!(
            "data: {\"type\":\"response.output_item.done\",\"response_id\":\"\
             resp_dup\",",
            "\"sequence_number\":7,\"output_index\":0,",
            "\"item\":{\"id\":\"fc_dup\",\"type\":\"function_call\",",
            "\"call_id\":\"call_dup\",\"name\":\"zendesk.get_ticket\",",
            "\"arguments\":\"{\\\"ticket_id\\\":\\\"T-1\\\"}\",",
            "\"status\":\"completed\"}}\n\n",
        );
        let body = format!("{frame}{frame}");

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].item_id.as_deref(), Some("fc_dup"));
        assert_eq!(observed[0].call_id.as_deref(), Some("call_dup"));
        assert_eq!(observed[0].name.as_deref(), Some("zendesk.get_ticket"));
    }

    #[test]
    fn observes_responses_stream_mcp_call_without_raw_output() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.done\",\"response_id\":\"\
             resp_mcp\",\"sequence_number\":1,",
            "\"output_index\":2,\"item\":{\"id\":\"mcp_1\",\"type\":\"\
             mcp_call\",\"name\":\"search_docs\",",
            "\"server_label\":\"docs\",\"status\":\"completed\",\"output\":\"\
             sensitive provider-hosted output\"}}\n\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesStreamItemKind::McpCall
        );
        assert_eq!(observed[0].event_type, "tool.call.observed");
        assert_eq!(observed[0].call_id.as_deref(), Some("openai_item:mcp_1"));
        assert_eq!(observed[0].metadata["tool_type"].as_str(), Some("mcp"));
        assert_eq!(
            observed[0].metadata["execution_owner"].as_str(),
            Some("provider_hosted")
        );
        assert_eq!(
            observed[0].metadata["provider_execution_status"].as_str(),
            Some("completed")
        );
        assert_eq!(observed[0].metadata["server_label"].as_str(), Some("docs"));
        assert_eq!(observed[0].metadata["has_output"].as_bool(), Some(true));
        assert_eq!(
            observed[0]
                .metadata
                .to_string()
                .contains("sensitive provider-hosted output"),
            false
        );
    }

    #[test]
    fn observes_responses_stream_reasoning_as_metadata_only() {
        let body = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.done\",\"\
             response_id\":\"resp_reason\",",
            "\"sequence_number\":3,\"output_index\":0,\"item_id\":\"rs_1\",",
            "\"text\":\"raw chain of thought should stay private\"}\n\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesStreamItemKind::Reasoning
        );
        assert_eq!(observed[0].event_type, "llm.reasoning.observed");
        assert_eq!(
            observed[0].metadata["reasoning_visibility"].as_str(),
            Some("metadata_only")
        );
        assert_eq!(
            observed[0].metadata["has_reasoning_text"].as_bool(),
            Some(true)
        );
        assert_eq!(
            observed[0]
                .metadata
                .to_string()
                .contains("raw chain of thought"),
            false
        );
    }

    #[test]
    fn observes_responses_stream_failed_as_error_observed() {
        let body = concat!(
            "data: {\"type\":\"response.failed\",\"response_id\":\"\
             resp_failed\",",
            "\"sequence_number\":9,\"error\":{\"message\":\"secret failure \
             details\"}}\n\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesStreamItemKind::Error
        );
        assert_eq!(observed[0].event_type, "error.observed");
        assert_eq!(observed[0].metadata["has_error"].as_bool(), Some(true));
        assert_eq!(
            observed[0]
                .metadata
                .to_string()
                .contains("secret failure details"),
            false
        );
    }

    #[test]
    fn responses_stream_parser_ignores_done_and_malformed_json() {
        let body = concat!(
            ": keepalive\r\n",
            "data: [DONE]\r\n\r\n",
            "data: {not-json}\r\n\r\n",
            "event: response.output_item.done\r\n",
            "data: {\"type\":\"response.completed\",\"response_id\":\"\
             resp_done\",\"sequence_number\":5}\r\n\r\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].kind,
            super::ObservedResponsesStreamItemKind::ResponseCompleted
        );
        assert_eq!(observed[0].event_type, "llm.response.completed.observed");
    }

    #[test]
    fn responses_stream_parser_skips_oversized_body() {
        let oversized = vec![b' '; super::MAX_OBSERVER_BODY_BYTES + 1];

        assert!(
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), &oversized,).is_empty()
        );
    }

    #[test]
    fn responses_stream_parser_supports_crlf_multiline_data_and_comments() {
        let body = concat!(
            ": comment\r\n",
            "data: {\"type\":\"response.completed\",\r\n",
            "data: \"response_id\":\"resp_multiline\",\r\n",
            "data: \"sequence_number\":11}\r\n\r\n",
        );

        let observed =
            super::observe_responses_stream_agent_items(uuid::Uuid::nil(), body.as_bytes());

        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].response_id.as_deref(), Some("resp_multiline"));
        assert_eq!(observed[0].sequence, 11);
    }
}
