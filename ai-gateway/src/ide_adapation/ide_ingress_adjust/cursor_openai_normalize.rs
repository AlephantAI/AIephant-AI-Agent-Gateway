//! OpenAI Chat Completions ingress normalizations aligned with 9router
//! `open-sse/translator/helpers/toolCallHelper.js` (`ensureToolCallIds`,
//! `fixMissingToolResponses`) plus stripping `index` on `tool_calls` entries
//! (see `open-sse/translator/request/openai-to-cursor.js`).
//!
//! Also normalizes **assistant** `content` so it matches
//! `async_openai::CreateChatCompletionRequest`: only `text` / `refusal` parts
//! in content arrays. Extended blocks (reasoning, thinking, Claude `tool_use`
//! inside `content`, etc.) are folded into `text`, promoted to `tool_calls`,
//! or dropped before strict serde in [`super::cursor_ingress`].
//!
//! Top-level `tools` may use Cursor / Anthropic wire (`name` + `input_schema`);
//! OpenAI chat tools require `type: function` and `function.parameters`.

use serde_json::{Map, Value, json};

use crate::error::{api::ApiError, invalid_req::InvalidRequestError};

fn tool_id_is_valid(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn sanitize_tool_id(id: &str) -> Option<String> {
    let sanitized: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized)
    }
}

fn generate_tool_call_id(
    msg_index: usize,
    tc_index: usize,
    tool_name: &str,
) -> String {
    let suffix: String = if tool_name.is_empty() {
        String::new()
    } else {
        let safe: String = tool_name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        format!("_{safe}")
    };
    format!("call_msg{msg_index}_tc{tc_index}{suffix}")
}

/// Pull plain text from extended / provider-specific assistant content blocks.
fn extract_foldable_text_from_block(
    map: &Map<String, Value>,
) -> Option<String> {
    const KEYS: &[&str] = &[
        "text",
        "thinking",
        "reasoning",
        "content",
        "summary",
        "encrypted_content",
    ];
    for k in KEYS {
        match map.get(*k)? {
            Value::String(s) => return Some(s.clone()),
            Value::Number(n) => return Some(n.to_string()),
            Value::Array(items) => {
                let mut acc = String::new();
                for it in items {
                    if let Some(s) = it.as_str() {
                        if !acc.is_empty() {
                            acc.push('\n');
                        }
                        acc.push_str(s);
                    } else if let Some(o) = it.as_object() {
                        if let Some(Value::String(t)) = o.get("text") {
                            if !acc.is_empty() {
                                acc.push('\n');
                            }
                            acc.push_str(t);
                        }
                    }
                }
                if !acc.is_empty() {
                    return Some(acc);
                }
            }
            _ => {}
        }
    }
    None
}

fn tool_use_block_to_openai_tool_call(block: &Value) -> Option<Value> {
    let o = block.as_object()?;
    let id = o.get("id").and_then(|v| v.as_str())?.to_string();
    let name = o
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let args_val = o.get("input").or_else(|| o.get("arguments"));
    let arguments = match args_val {
        None => "{}".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => {
            serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
        }
    };
    Some(json!({
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": arguments}
    }))
}

fn merge_adjacent_text_content_parts(parts: &mut Vec<Value>) {
    let mut i = 0;
    while i + 1 < parts.len() {
        let merge = match (parts[i].as_object(), parts[i + 1].as_object()) {
            (Some(a), Some(b)) => {
                a.get("type").and_then(|t| t.as_str()) == Some("text")
                    && b.get("type").and_then(|t| t.as_str()) == Some("text")
            }
            _ => false,
        };
        if merge {
            let t1 = parts[i]["text"].as_str().unwrap_or("").to_string();
            let t2 = parts[i + 1]["text"].as_str().unwrap_or("").to_string();
            let joined = if t1.is_empty() {
                t2
            } else if t2.is_empty() {
                t1
            } else {
                format!("{t1}\n{t2}")
            };
            parts[i] = json!({ "type": "text", "text": joined });
            parts.remove(i + 1);
        } else {
            i += 1;
        }
    }
}

/// Maps one JSON `messages[].content[]` block into OpenAI **user** array parts
/// (`text` | `image_url` | `input_audio`).
fn block_to_user_openai_content_parts(block: &Value) -> Vec<Value> {
    if let Some(s) = block.as_str() {
        return vec![json!({ "type": "text", "text": s })];
    }
    let Some(map) = block.as_object() else {
        return Vec::new();
    };
    let ty = map
        .get("type")
        .and_then(|t| t.as_str())
        .map(str::to_lowercase);
    match ty.as_deref() {
        Some("text" | "input_text") => {
            let text = match map.get("text") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => serde_json::to_string(v).unwrap_or_default(),
                None => String::new(),
            };
            vec![json!({ "type": "text", "text": text })]
        }
        Some("image_url") => {
            if map.get("image_url").is_some() {
                vec![json!({
                    "type": "image_url",
                    "image_url": map.get("image_url").cloned().unwrap_or(json!({}))
                })]
            } else {
                Vec::new()
            }
        }
        Some("input_audio") => {
            if map.get("input_audio").is_some() {
                vec![json!({
                    "type": "input_audio",
                    "input_audio": map.get("input_audio").cloned().unwrap_or(json!({}))
                })]
            } else {
                Vec::new()
            }
        }
        Some(
            "reasoning"
            | "thinking"
            | "redacted_thinking"
            | "encrypted_reasoning"
            | "refusal",
        ) => extract_foldable_text_from_block(map)
            .map(|t| vec![json!({ "type": "text", "text": t })])
            .unwrap_or_default(),
        Some("tool_result") => {
            let from_content = match map.get("content") {
                Some(Value::String(s)) => Some(s.clone()),
                Some(v) => serde_json::to_string(v).ok(),
                None => None,
            };
            from_content
                .or_else(|| extract_foldable_text_from_block(map))
                .map(|t| vec![json!({ "type": "text", "text": t })])
                .unwrap_or_default()
        }
        Some(_) => extract_foldable_text_from_block(map)
            .map(|t| vec![json!({ "type": "text", "text": t })])
            .unwrap_or_default(),
        None => {
            if map.get("image_url").is_some() {
                return vec![json!({
                    "type": "image_url",
                    "image_url": map.get("image_url").cloned().unwrap_or(json!({}))
                })];
            }
            if map.get("input_audio").is_some() {
                return vec![json!({
                    "type": "input_audio",
                    "input_audio": map.get("input_audio").cloned().unwrap_or(json!({}))
                })];
            }
            extract_foldable_text_from_block(map)
                .map(|t| vec![json!({ "type": "text", "text": t })])
                .unwrap_or_default()
        }
    }
}

/// Collapses an all-`text` part list to a single string; otherwise keeps an
/// array (e.g. interleaved text + `image_url`).
fn finalize_user_style_content_parts(parts: Vec<Value>) -> Value {
    if parts.is_empty() {
        return Value::String(String::new());
    }
    let has_multimodal = parts.iter().any(|p| {
        matches!(
            p.get("type").and_then(|t| t.as_str()),
            Some("image_url" | "input_audio")
        )
    });
    if has_multimodal {
        return Value::Array(parts);
    }
    let mut acc = String::new();
    for p in &parts {
        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
            let t = p.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !acc.is_empty() && !t.is_empty() {
                acc.push('\n');
            }
            acc.push_str(t);
        }
    }
    Value::String(acc)
}

/// Rewrites `user` / `system` / `tool` / `developer` `content` so strict
/// `CreateChatCompletionRequest` serde succeeds. Drops or folds blocks that
/// are not valid for [`ChatCompletionRequestUserMessageContent`] (and
/// text-only roles).
fn sanitize_user_system_tool_developer_messages_for_openai_chat_schema(
    messages: &mut [Value],
) -> Result<bool, ApiError> {
    let mut changed = false;
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        let Some(role) = obj.get("role").and_then(|r| r.as_str()) else {
            continue;
        };
        if !matches!(role, "user" | "system" | "tool" | "developer") {
            continue;
        }

        let Some(original_content) = obj.get("content").cloned() else {
            continue;
        };
        let content_eq_snapshot = original_content.clone();

        let new_content = match original_content {
            Value::String(_) | Value::Null => None,
            Value::Array(blocks) => {
                let mut parts: Vec<Value> = Vec::new();
                for b in &blocks {
                    let mut from_b = block_to_user_openai_content_parts(b);
                    if matches!(role, "system" | "developer" | "tool") {
                        from_b.retain(|p| {
                            p.get("type").and_then(|t| t.as_str())
                                == Some("text")
                        });
                    }
                    parts.extend(from_b);
                }
                merge_adjacent_text_content_parts(&mut parts);
                let out = finalize_user_style_content_parts(parts);
                if out != content_eq_snapshot {
                    changed = true;
                }
                Some(out)
            }
            Value::Object(ref map) => {
                let out = if let Some(t) = extract_foldable_text_from_block(map)
                {
                    Value::String(t)
                } else if let Some(Value::String(s)) = map.get("text") {
                    Value::String(s.clone())
                } else {
                    Value::String(String::new())
                };
                if out != content_eq_snapshot {
                    changed = true;
                }
                Some(out)
            }
            ref other => {
                let out = if let Some(s) = other.as_str() {
                    Value::String(s.to_string())
                } else {
                    Value::String(other.to_string())
                };
                if out != content_eq_snapshot {
                    changed = true;
                }
                Some(out)
            }
        };

        if let Some(c) = new_content {
            obj.insert("content".to_string(), c);
        }
    }
    Ok(changed)
}

/// Rewrites assistant `content` so `serde_json` → `CreateChatCompletionRequest`
/// succeeds: only OpenAI `text` / `refusal` array parts (or string). Claude /
/// OpenRouter reasoning blocks become `text`; `tool_use` in `content` is
/// removed and merged into top-level `tool_calls` when needed.
fn sanitize_assistant_messages_for_openai_chat_schema(
    messages: &mut [Value],
) -> Result<bool, ApiError> {
    use std::collections::HashSet;

    let mut changed = false;
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if obj.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }

        let Some(original_content) = obj.get("content").cloned() else {
            continue;
        };
        let content_eq_snapshot = original_content.clone();

        let existing_ids: HashSet<String> = obj
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|tc| {
                tc.get("id").and_then(|id| id.as_str()).map(str::to_string)
            })
            .collect();

        let mut promoted_tool_calls: Vec<Value> = Vec::new();

        let new_content = match original_content {
            Value::String(_) => None,
            Value::Null => None,
            Value::Array(blocks) => {
                let mut new_parts: Vec<Value> = Vec::new();
                for block in &blocks {
                    if let Some(s) = block.as_str() {
                        new_parts.push(
                            json!({ "type": "text", "text": s.to_string() }),
                        );
                        changed = true;
                        continue;
                    }
                    let Some(map) = block.as_object() else {
                        changed = true;
                        continue;
                    };
                    let ty = map
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(str::to_lowercase);

                    match ty.as_deref() {
                        Some("text") => {
                            let text = match map.get("text") {
                                Some(Value::String(s)) => s.clone(),
                                Some(v) => {
                                    changed = true;
                                    serde_json::to_string(v).unwrap_or_default()
                                }
                                None => String::new(),
                            };
                            new_parts
                                .push(json!({ "type": "text", "text": text }));
                        }
                        Some("refusal") => {
                            let refusal = map
                                .get("refusal")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            new_parts.push(json!({
                                "type": "refusal",
                                "refusal": refusal
                            }));
                        }
                        Some("tool_use") => {
                            changed = true;
                            if let Some(tc) =
                                tool_use_block_to_openai_tool_call(block)
                            {
                                let id = tc
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if !id.is_empty() && !existing_ids.contains(&id)
                                {
                                    promoted_tool_calls.push(tc);
                                }
                            }
                        }
                        Some(
                            "thinking"
                            | "reasoning"
                            | "redacted_thinking"
                            | "encrypted_reasoning",
                        )
                        | None => {
                            if let Some(t) =
                                extract_foldable_text_from_block(map)
                            {
                                new_parts
                                    .push(json!({ "type": "text", "text": t }));
                                changed = true;
                            } else {
                                changed = true;
                            }
                        }
                        Some(other) => {
                            if matches!(
                                other,
                                "image_url"
                                    | "input_audio"
                                    | "input_file"
                                    | "file"
                            ) {
                                changed = true;
                                continue;
                            }
                            if let Some(t) =
                                extract_foldable_text_from_block(map)
                            {
                                new_parts
                                    .push(json!({ "type": "text", "text": t }));
                                changed = true;
                            } else {
                                changed = true;
                            }
                        }
                    }
                }

                if blocks.len() != new_parts.len()
                    || !promoted_tool_calls.is_empty()
                {
                    changed = true;
                }

                merge_adjacent_text_content_parts(&mut new_parts);

                let will_have_tool_calls = obj
                    .get("tool_calls")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| !a.is_empty())
                    || !promoted_tool_calls.is_empty();

                if !promoted_tool_calls.is_empty() {
                    let tc_arr = obj
                        .entry("tool_calls".to_string())
                        .or_insert_with(|| Value::Array(Vec::new()))
                        .as_array_mut()
                        .ok_or_else(|| {
                            ApiError::InvalidRequest(InvalidRequestError::from(
                                serde_json::from_slice::<i32>(b"[]")
                                    .unwrap_err(),
                            ))
                        })?;
                    for tc in promoted_tool_calls {
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !id.is_empty()
                            && !tc_arr.iter().any(|existing| {
                                existing.get("id").and_then(|v| v.as_str())
                                    == Some(&id)
                            })
                        {
                            tc_arr.push(tc);
                        }
                    }
                }

                let out = if new_parts.is_empty() {
                    if will_have_tool_calls {
                        Value::Null
                    } else {
                        Value::String(String::new())
                    }
                } else if new_parts.len() == 1
                    && new_parts[0].get("type").and_then(|t| t.as_str())
                        == Some("text")
                {
                    let only = new_parts.into_iter().next().expect("len 1");
                    let text = only["text"].as_str().unwrap_or("").to_string();
                    Value::String(text)
                } else {
                    Value::Array(new_parts)
                };

                if out != content_eq_snapshot {
                    changed = true;
                }
                Some(out)
            }
            Value::Object(ref map) => {
                let out = if let Some(t) = extract_foldable_text_from_block(map)
                {
                    Value::String(t)
                } else if let Some(Value::String(s)) = map.get("text") {
                    Value::String(s.clone())
                } else {
                    Value::String(String::new())
                };
                if out != content_eq_snapshot {
                    changed = true;
                }
                Some(out)
            }
            ref other => {
                let out = if let Some(s) = other.as_str() {
                    Value::String(s.to_string())
                } else {
                    Value::String(other.to_string())
                };
                if out != content_eq_snapshot {
                    changed = true;
                }
                Some(out)
            }
        };

        if let Some(c) = new_content {
            obj.insert("content".to_string(), c);
        }
    }
    Ok(changed)
}

fn strip_tool_call_index_fields(messages: &mut [Value]) -> bool {
    let mut changed = false;
    for msg in messages.iter_mut() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        if obj.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(tool_calls) =
            obj.get_mut("tool_calls").and_then(|v| v.as_array_mut())
        else {
            continue;
        };
        for tc in tool_calls.iter_mut() {
            let Some(tc_obj) = tc.as_object_mut() else {
                continue;
            };
            if tc_obj.remove("index").is_some() {
                changed = true;
            }
        }
    }
    changed
}

fn stringify_tool_arguments(
    tc_obj: &mut serde_json::Map<String, Value>,
) -> bool {
    let Some(func) = tc_obj.get_mut("function").and_then(|f| f.as_object_mut())
    else {
        return false;
    };
    let Some(args) = func.get("arguments") else {
        return false;
    };
    if args.is_string() {
        return false;
    }
    let serialized =
        serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    func.insert("arguments".to_string(), Value::String(serialized));
    true
}

/// Ensures tool call ids match the Anthropic-style pattern used in 9router and
/// normalizes `type` / string `arguments` on assistant `tool_calls`.
fn ensure_tool_call_ids(messages: &mut [Value]) -> Result<bool, ApiError> {
    let mut changed = false;
    for (i, msg) in messages.iter_mut().enumerate() {
        let Some(obj) = msg.as_object_mut() else {
            continue;
        };
        let role = obj.get("role").and_then(|r| r.as_str()).map(str::to_string);
        if role.as_deref() == Some("assistant") {
            if let Some(tool_calls) =
                obj.get_mut("tool_calls").and_then(|v| v.as_array_mut())
            {
                for (j, tc) in tool_calls.iter_mut().enumerate() {
                    let Some(tc_obj) = tc.as_object_mut() else {
                        continue;
                    };
                    let name = tc_obj
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let id_val =
                        tc_obj.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let needs_new_id = !tool_id_is_valid(id_val);
                    if needs_new_id {
                        let new_id =
                            sanitize_tool_id(id_val).unwrap_or_else(|| {
                                generate_tool_call_id(i, j, &name)
                            });
                        tc_obj.insert("id".to_string(), Value::String(new_id));
                        changed = true;
                    }
                    if tc_obj.get("type").is_none() {
                        tc_obj.insert(
                            "type".to_string(),
                            Value::String("function".to_string()),
                        );
                        changed = true;
                    }
                    if stringify_tool_arguments(tc_obj) {
                        changed = true;
                    }
                }
            }
        }

        if role.as_deref() == Some("tool") {
            if let Some(id_val) =
                obj.get_mut("tool_call_id").and_then(|v| v.as_str())
            {
                let id_owned = id_val.to_string();
                if !tool_id_is_valid(&id_owned) {
                    let new_id = sanitize_tool_id(&id_owned)
                        .unwrap_or_else(|| generate_tool_call_id(i, 0, ""));
                    obj.insert(
                        "tool_call_id".to_string(),
                        Value::String(new_id),
                    );
                    changed = true;
                }
            }
        }

        if let Some(content) =
            obj.get_mut("content").and_then(|c| c.as_array_mut())
        {
            for (k, block) in content.iter_mut().enumerate() {
                let Some(block_obj) = block.as_object_mut() else {
                    continue;
                };
                let ty = block_obj
                    .get("type")
                    .and_then(|t| t.as_str())
                    .map(str::to_string);
                if ty.as_deref() == Some("tool_use") {
                    if let Some(id_val) =
                        block_obj.get("id").and_then(|v| v.as_str())
                    {
                        let id_owned = id_val.to_string();
                        if !tool_id_is_valid(&id_owned) {
                            let name = block_obj
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let new_id = sanitize_tool_id(&id_owned)
                                .unwrap_or_else(|| {
                                    generate_tool_call_id(i, k, &name)
                                });
                            block_obj.insert(
                                "id".to_string(),
                                Value::String(new_id),
                            );
                            changed = true;
                        }
                    }
                }
                if ty.as_deref() == Some("tool_result") {
                    if let Some(id_val) =
                        block_obj.get("tool_use_id").and_then(|v| v.as_str())
                    {
                        let id_owned = id_val.to_string();
                        if !tool_id_is_valid(&id_owned) {
                            let new_id = sanitize_tool_id(&id_owned)
                                .unwrap_or_else(|| {
                                    generate_tool_call_id(i, k, "")
                                });
                            block_obj.insert(
                                "tool_use_id".to_string(),
                                Value::String(new_id),
                            );
                            changed = true;
                        }
                    }
                }
            }
        }
    }
    Ok(changed)
}

fn get_tool_call_ids(msg: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let Some(obj) = msg.as_object() else {
        return ids;
    };
    if obj.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return ids;
    }
    if let Some(tool_calls) = obj.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    ids.push(id.to_string());
                }
            }
        }
    }
    if let Some(content) = obj.get("content").and_then(|c| c.as_array()) {
        for block in content {
            let ty = block.get("type").and_then(|t| t.as_str());
            if ty == Some("tool_use") {
                if let Some(id) = block.get("id").and_then(|v| v.as_str()) {
                    if !id.is_empty() {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }
    ids
}

fn has_tool_results(msg: &Value, tool_call_ids: &[String]) -> bool {
    if tool_call_ids.is_empty() {
        return false;
    }
    let Some(obj) = msg.as_object() else {
        return false;
    };
    if obj.get("role").and_then(|r| r.as_str()) == Some("tool")
        && let Some(id) = obj.get("tool_call_id").and_then(|v| v.as_str())
    {
        return tool_call_ids.iter().any(|t| t == id);
    }
    if obj.get("role").and_then(|r| r.as_str()) == Some("user")
        && let Some(content) = obj.get("content").and_then(|c| c.as_array())
    {
        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                && let Some(id) =
                    block.get("tool_use_id").and_then(|v| v.as_str())
            {
                if tool_call_ids.iter().any(|t| t == id) {
                    return true;
                }
            }
        }
    }
    false
}

fn fix_missing_tool_responses(messages: &mut Vec<Value>) -> bool {
    let mut new_messages: Vec<Value> = Vec::with_capacity(messages.len());
    let mut changed = false;
    let len = messages.len();
    for i in 0..len {
        let msg = messages[i].clone();
        new_messages.push(msg);
        let tool_call_ids = get_tool_call_ids(&messages[i]);
        if tool_call_ids.is_empty() {
            continue;
        }
        let next_msg = messages.get(i + 1);
        let need_fill =
            next_msg.is_some_and(|n| !has_tool_results(n, &tool_call_ids));
        if need_fill {
            for id in &tool_call_ids {
                new_messages.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": ""
                }));
                changed = true;
            }
        }
    }
    if changed {
        *messages = new_messages;
    }
    changed
}

/// Normalizes top-level `tool_choice` so `CreateChatCompletionRequest` serde
/// succeeds. Cursor / Anthropic stacks may send `any` or non-OpenAI objects.
fn normalize_top_level_tool_choice(body: &mut Value) -> bool {
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    if !obj.contains_key("tool_choice") {
        return false;
    }
    let tc = match obj.get("tool_choice") {
        None => return false,
        Some(v) if v.is_null() => return false,
        Some(v) => v.clone(),
    };
    let new_v = normalize_tool_choice_value(&tc);
    if new_v == tc {
        false
    } else {
        obj.insert("tool_choice".to_string(), new_v);
        true
    }
}

fn normalize_tool_choice_value(tc: &Value) -> Value {
    match tc {
        Value::String(s) => {
            let lower = s.trim().to_ascii_lowercase();
            let out = match lower.as_str() {
                "none" => "none",
                "auto" => "auto",
                "required" => "required",
                // Anthropic / OpenRouter style — OpenAI uses `required`
                "any" => "required",
                _ => "auto",
            };
            Value::String(out.to_string())
        }
        Value::Object(m) => {
            let ty = m
                .get("type")
                .and_then(|v| v.as_str())
                .map(str::to_lowercase);
            if ty.as_deref() == Some("function") {
                let name = m
                    .get("function")
                    .and_then(|f| f.as_object())
                    .and_then(|fo| fo.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
                if let Some(nm) = name {
                    return json!({
                        "type": "function",
                        "function": { "name": nm }
                    });
                }
                return Value::String("auto".into());
            }
            // Implicit named tool: `{ "function": { "name": "..." } }`
            if ty.is_none() {
                let name = m
                    .get("function")
                    .and_then(|f| f.as_object())
                    .and_then(|fo| fo.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
                if let Some(nm) = name {
                    return json!({
                        "type": "function",
                        "function": { "name": nm }
                    });
                }
            }
            if let Some(ref tn) = ty {
                match tn.as_str() {
                    "none" | "auto" | "required" => {
                        return Value::String(tn.clone());
                    }
                    "any" => return Value::String("required".into()),
                    _ => {}
                }
            }
            Value::String("auto".into())
        }
        _ => Value::String("auto".into()),
    }
}

fn normalize_single_tool_definition(tool: &Value) -> Value {
    let Some(m) = tool.as_object() else {
        return tool.clone();
    };
    let ty_str = m.get("type").and_then(|v| v.as_str());
    let looks_openai = ty_str
        .is_some_and(|t| t.eq_ignore_ascii_case("function"))
        && m.get("function").and_then(|v| v.as_object()).is_some();

    if looks_openai {
        if let Some(f) = m.get("function").and_then(|v| v.as_object()) {
            let mut inner = f.clone();
            if let Some(s) = inner.remove("input_schema") {
                inner.entry("parameters".to_string()).or_insert(s);
            }
            if inner
                .get("name")
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty())
                .is_some()
            {
                return json!({
                    "type": "function",
                    "function": Value::Object(inner),
                });
            }
        }
        return tool.clone();
    }

    if let Some(Value::String(name)) = m.get("name") {
        if !name.is_empty() {
            let mut inner = Map::new();
            inner.insert("name".into(), Value::String(name.clone()));
            if let Some(d) = m.get("description") {
                if !d.is_null() {
                    inner.insert("description".into(), d.clone());
                }
            }
            let params = m
                .get("input_schema")
                .or_else(|| m.get("parameters"))
                .cloned()
                .unwrap_or_else(
                    || json!({ "type": "object", "properties": {} }),
                );
            inner.insert("parameters".into(), params);
            return json!({
                "type": "function",
                "function": Value::Object(inner),
            });
        }
    }

    if ty_str.is_none() {
        if let Some(f) = m.get("function").and_then(|v| v.as_object()) {
            let mut inner = f.clone();
            if let Some(s) = inner.remove("input_schema") {
                inner.entry("parameters".to_string()).or_insert(s);
            }
            if inner
                .get("name")
                .and_then(|n| n.as_str())
                .filter(|s| !s.is_empty())
                .is_some()
            {
                return json!({
                    "type": "function",
                    "function": Value::Object(inner),
                });
            }
        }
    }

    tool.clone()
}

fn normalize_tools_array_for_openai(body: &mut Value) -> bool {
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    let Some(Value::Array(tools)) = obj.get_mut("tools") else {
        return false;
    };
    let mut changed = false;
    for slot in tools.iter_mut() {
        let new_t = normalize_single_tool_definition(slot);
        if new_t != *slot {
            *slot = new_t;
            changed = true;
        }
    }
    changed
}

/// Applies Cursor-targeted OpenAI ingress normalizations to `body` (top-level
/// chat completion JSON object). Returns whether any mutation occurred.
pub fn normalize_cursor_openai_request_value(
    body: &mut Value,
) -> Result<bool, ApiError> {
    let mut mutated = normalize_top_level_tool_choice(body);
    mutated |= normalize_tools_array_for_openai(body);
    let Some(messages) = body.get_mut("messages") else {
        return Ok(mutated);
    };
    let Some(arr) = messages.as_array_mut() else {
        return Err(ApiError::InvalidRequest(InvalidRequestError::from(
            serde_json::from_slice::<i32>(b"[]").unwrap_err(),
        )));
    };
    mutated |= strip_tool_call_index_fields(arr);
    mutated |=
        sanitize_user_system_tool_developer_messages_for_openai_chat_schema(
            arr,
        )?;
    mutated |= sanitize_assistant_messages_for_openai_chat_schema(arr)?;
    mutated |= ensure_tool_call_ids(arr)?;
    mutated |= fix_missing_tool_responses(arr);
    Ok(mutated)
}

#[cfg(test)]
mod tests {
    use async_openai::types::CreateChatCompletionRequest;
    use serde_json::json;

    use super::normalize_cursor_openai_request_value;

    #[test]
    fn normalizes_tool_choice_any_string_for_openai_de() {
        let mut body = json!({
            "model": "x",
            "tool_choice": "any",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["tool_choice"], "required");
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn normalizes_tool_choice_object_any_type_to_required_string() {
        let mut body = json!({
            "model": "x",
            "tool_choice": {"type": "any"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["tool_choice"], "required");
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn normalizes_tool_choice_unknown_object_to_auto() {
        let mut body = json!({
            "model": "x",
            "tool_choice": {"type": "tool", "name": "x"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["tool_choice"], "auto");
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn normalizes_implicit_named_tool_choice_object() {
        let mut body = json!({
            "model": "x",
            "tool_choice": {"function": {"name": "read_file"}},
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["function"]["name"], "read_file");
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn wraps_cursor_style_tools_name_input_schema_for_openai_de() {
        let mut body = json!({
            "model": "x",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "name": "Shell",
                "description": "runs shell",
                "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "Shell");
        assert_eq!(body["tools"][0]["function"]["description"], "runs shell");
        assert!(body["tools"][0]["function"].get("parameters").is_some());
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn rewrites_function_tool_input_schema_to_parameters() {
        let mut body = json!({
            "model": "x",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "Read",
                    "input_schema": {"type": "object", "properties": {}}
                }
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert!(body["tools"][0]["function"].get("input_schema").is_none());
        assert!(body["tools"][0]["function"].get("parameters").is_some());
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn sanitizes_user_content_reasoning_and_input_text_for_openai_de() {
        let mut body = json!({
            "model": "x",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "hi"},
                    {"type": "reasoning", "text": "r"}
                ]
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }

    #[test]
    fn strips_index_from_tool_calls() {
        let mut body = json!({
            "model": "x",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "n", "arguments": "{}"}
                }]
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        let tc = &body["messages"][0]["tool_calls"][0];
        assert!(tc.get("index").is_none());
        assert_eq!(tc["id"], "call_1");
    }

    #[test]
    fn ensures_tool_call_id_and_stringifies_arguments() {
        let mut body = json!({
            "model": "x",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "!!!",
                    "function": {"name": "fn", "arguments": {"a": 1}}
                }]
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        let tc = &body["messages"][0]["tool_calls"][0];
        assert_eq!(tc["type"], "function");
        assert!(tc["id"].as_str().unwrap().contains("call_msg"));
        assert_eq!(tc["function"]["arguments"], "{\"a\":1}");
    }

    #[test]
    fn inserts_missing_tool_messages_when_next_exists() {
        let mut body = json!({
            "model": "x",
            "messages": [
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "t1",
                        "type": "function",
                        "function": {"name": "n", "arguments": "{}"}
                    }]
                },
                {"role": "user", "content": "next"}
            ]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "t1");
        assert_eq!(body["messages"][2]["role"], "user");
    }

    #[test]
    fn does_not_insert_tool_messages_when_assistant_is_last() {
        let mut body = json!({
            "model": "x",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "t1",
                    "type": "function",
                    "function": {"name": "n", "arguments": "{}"}
                }]
            }]
        });
        assert!(!normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn folds_reasoning_blocks_so_strict_openai_de_passes() {
        let mut body = json!({
            "model": "anthropic/claude-opus-4.6",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "reasoning", "text": "think"},
                    {"type": "text", "text": "out"}
                ]
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert_eq!(body["messages"][0]["content"], "think\nout");
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .expect("strict OpenAI serde");
    }

    #[test]
    fn promotes_tool_use_content_to_tool_calls_for_openai_de() {
        let mut body = json!({
            "model": "x",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_read",
                    "name": "read",
                    "input": {"path": "/tmp"}
                }]
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        assert!(body["messages"][0]["tool_calls"].is_array());
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            "read"
        );
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .expect("strict OpenAI serde");
    }

    #[test]
    fn folds_string_fragments_in_assistant_content_array() {
        let mut body = json!({
            "model": "x",
            "messages": [{
                "role": "assistant",
                "content": ["part-a", "part-b"]
            }]
        });
        assert!(normalize_cursor_openai_request_value(&mut body).unwrap());
        let _: CreateChatCompletionRequest =
            serde_json::from_slice(&serde_json::to_vec(&body).unwrap())
                .unwrap();
    }
}
