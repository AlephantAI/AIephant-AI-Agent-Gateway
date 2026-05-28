//! Responses API inbound JSON normalization before strict `CreateResponse`
//! serde.

use bytes::Bytes;
use serde_json::{Value, json};

use crate::{
    error::{
        api::ApiError, internal::InternalError,
        invalid_req::InvalidRequestError, mapper::MapperError,
    },
    ide_adapation::client_profile::ClientProfile,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponsesNormalizeMode {
    CreateResponseCompat,
    OpenAiWire,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResponsesRequestRoutingFields {
    pub(crate) model: String,
    pub(crate) stream: bool,
}

pub(crate) fn responses_request_routing_fields(
    body: &Bytes,
) -> Result<ResponsesRequestRoutingFields, ApiError> {
    let root: Value = serde_json::from_slice(body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let model = root
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .ok_or(InvalidRequestError::MissingModelId)?
        .to_string();
    let stream = root.get("stream").and_then(Value::as_bool).unwrap_or(false);

    Ok(ResponsesRequestRoutingFields { model, stream })
}

pub fn apply_responses_wire_normalize(body: Bytes) -> Result<Bytes, ApiError> {
    apply_responses_normalize(
        body,
        ResponsesNormalizeMode::CreateResponseCompat,
        ClientProfile::Unknown,
    )
}

pub fn apply_responses_wire_normalize_for_client(
    body: Bytes,
    profile: ClientProfile,
) -> Result<Bytes, ApiError> {
    apply_responses_normalize(
        body,
        ResponsesNormalizeMode::CreateResponseCompat,
        profile,
    )
}

pub fn apply_openai_responses_wire_normalize(
    body: Bytes,
) -> Result<Bytes, ApiError> {
    apply_responses_normalize(
        body,
        ResponsesNormalizeMode::OpenAiWire,
        ClientProfile::Unknown,
    )
}

pub fn apply_openai_responses_wire_normalize_for_client(
    body: Bytes,
    profile: ClientProfile,
) -> Result<Bytes, ApiError> {
    apply_responses_normalize(body, ResponsesNormalizeMode::OpenAiWire, profile)
}

fn apply_responses_normalize(
    body: Bytes,
    mode: ResponsesNormalizeMode,
    profile: ClientProfile,
) -> Result<Bytes, ApiError> {
    let mut root: Value = serde_json::from_slice(&body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let changed = normalize_responses_request_value(&mut root, mode, profile)?;
    if !changed {
        return Ok(body);
    }
    Ok(Bytes::from(serde_json::to_vec(&root).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<Value>(),
            error,
        })
    })?))
}

fn normalize_responses_request_value(
    root: &mut Value,
    mode: ResponsesNormalizeMode,
    profile: ClientProfile,
) -> Result<bool, ApiError> {
    let mut changed = false;
    changed |= normalize_tools_array(root, profile)?;
    changed |= rewrite_responses_input_items(root, mode)
        .map_err(|e| ApiError::Internal(InternalError::MapperError(e)))?;
    Ok(changed)
}

fn normalize_tools_array(
    root: &mut Value,
    profile: ClientProfile,
) -> Result<bool, ApiError> {
    let Some(tools) = root.get_mut("tools") else {
        return Ok(false);
    };
    let Some(arr) = tools.as_array_mut() else {
        return Ok(false);
    };

    let mut changed = false;
    let mut normalized = Vec::with_capacity(arr.len());
    for tool in std::mem::take(arr) {
        let typ = tool.get("type").and_then(|t| t.as_str());
        match typ {
            Some("tool_search") if profile == ClientProfile::CodexCli => {
                let mut tool = tool;
                if let Some(obj) = tool.as_object_mut() {
                    obj.insert(
                        "type".to_string(),
                        Value::String("function".to_string()),
                    );
                    obj.entry("name".to_string()).or_insert_with(|| {
                        Value::String("tool_search".to_string())
                    });
                    obj.remove("execution");
                    changed = true;
                }
                normalized.push(tool);
            }
            Some("namespace") if profile == ClientProfile::CodexCli => {
                if flatten_codex_namespace_tool(tool, &mut normalized) {
                    changed = true;
                }
            }
            Some("web_search") => {
                let mut tool = tool;
                if let Some(obj) = tool.as_object_mut() {
                    obj.insert(
                        "type".to_string(),
                        Value::String("web_search_preview".to_string()),
                    );
                    changed = true;
                }
                normalized.push(tool);
            }
            Some("function" | "custom" | "file_search" | "mcp")
            | Some("image_generation" | "code_interpreter")
            | Some("computer_use_preview" | "local_shell") => {
                normalized.push(tool);
            }
            Some(_) if tool.get("name").and_then(|n| n.as_str()).is_some() => {
                normalized.push(tool);
            }
            Some(_) | None => {
                changed = true;
            }
        }
    }
    *arr = normalized;
    if arr.is_empty() {
        root.as_object_mut().map(|obj| obj.remove("tools"));
        changed = true;
    }
    Ok(changed)
}

fn flatten_codex_namespace_tool(
    namespace: Value,
    normalized: &mut Vec<Value>,
) -> bool {
    let Some(namespace_obj) = namespace.as_object() else {
        return true;
    };
    let Some(namespace_name) =
        namespace_obj.get("name").and_then(Value::as_str)
    else {
        return true;
    };
    let Some(namespace_tools) =
        namespace_obj.get("tools").and_then(Value::as_array)
    else {
        return true;
    };

    for child in namespace_tools {
        let Some(child_obj) = child.as_object() else {
            continue;
        };
        let Some(child_name) = child_obj.get("name").and_then(Value::as_str)
        else {
            continue;
        };

        let mut function_tool = serde_json::Map::new();
        function_tool
            .insert("type".to_string(), Value::String("function".to_string()));
        function_tool.insert(
            "name".to_string(),
            Value::String(format!("{namespace_name}__{child_name}")),
        );
        if let Some(description) = child_obj.get("description") {
            function_tool
                .insert("description".to_string(), description.clone());
        }
        let parameters = child_obj
            .get("parameters")
            .or_else(|| child_obj.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        function_tool.insert("parameters".to_string(), parameters);
        normalized.push(Value::Object(function_tool));
    }

    true
}

fn json_text_field(v: Option<&Value>) -> String {
    v.map(|t| match t {
        Value::String(s) => s.clone(),
        _ => t.to_string(),
    })
    .unwrap_or_default()
}

fn json_image_url_field(v: Option<&Value>) -> String {
    v.and_then(|u| {
        u.as_str().map(str::to_owned).or_else(|| {
            u.get("url").and_then(|x| x.as_str()).map(str::to_owned)
        })
    })
    .unwrap_or_default()
}

pub(crate) fn normalize_responses_message_content_json(
    content: &mut Value,
) -> Result<bool, MapperError> {
    normalize_input_message_content_json(content)
}

fn normalize_input_message_content_json(
    content: &mut Value,
) -> Result<bool, MapperError> {
    match content {
        Value::String(_) => Ok(false),
        Value::Array(parts) => {
            if parts.is_empty() {
                *content = Value::String(String::new());
                return Ok(true);
            }
            let mut changed = false;
            let mut new_parts = Vec::with_capacity(parts.len());
            for part in parts.iter() {
                if let Some(obj) = part.as_object() {
                    let typ =
                        obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match typ {
                        "input_text" | "input_image" | "input_file" => {
                            new_parts.push(part.clone());
                        }
                        "text" => {
                            let text = json_text_field(obj.get("text"));
                            new_parts.push(
                                json!({ "type": "input_text", "text": text }),
                            );
                            changed = true;
                        }
                        "image_url" => {
                            let url =
                                json_image_url_field(obj.get("image_url"));
                            if url.is_empty() {
                                let payload = serde_json::to_string(part)
                                    .map_err(MapperError::SerdeError)?;
                                new_parts.push(json!({
                                    "type": "input_text",
                                    "text": format!("[alephant:image_url]\n{payload}")
                                }));
                            } else {
                                new_parts.push(json!({
                                    "type": "input_image",
                                    "detail": "auto",
                                    "image_url": url
                                }));
                            }
                            changed = true;
                        }
                        "" if obj.contains_key("text") => {
                            let text = json_text_field(obj.get("text"));
                            new_parts.push(
                                json!({ "type": "input_text", "text": text }),
                            );
                            changed = true;
                        }
                        _ => {
                            let payload = serde_json::to_string(part)
                                .map_err(MapperError::SerdeError)?;
                            new_parts.push(json!({
                                "type": "input_text",
                                "text": format!("[alephant:content_part type={typ}]\n{payload}")
                            }));
                            changed = true;
                        }
                    }
                } else {
                    new_parts.push(json!({
                        "type": "input_text",
                        "text": part.to_string()
                    }));
                    changed = true;
                }
            }
            *content = Value::Array(new_parts);
            Ok(changed)
        }
        Value::Object(map) => {
            let typ = map.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match typ {
                "text" | "input_text" => {
                    let text = json_text_field(map.get("text"));
                    *content = Value::String(text);
                    Ok(true)
                }
                "image_url" => {
                    let url = json_image_url_field(map.get("image_url"));
                    if url.is_empty() {
                        let s =
                            serde_json::to_string(&Value::Object(map.clone()))
                                .map_err(MapperError::SerdeError)?;
                        *content = Value::String(format!(
                            "[alephant:message_content image_url]\n{s}"
                        ));
                    } else {
                        *content = Value::Array(vec![json!({
                            "type": "input_image",
                            "detail": "auto",
                            "image_url": url
                        })]);
                    }
                    Ok(true)
                }
                _ if typ.is_empty() && map.contains_key("text") => {
                    let text = json_text_field(map.get("text"));
                    *content = Value::String(text);
                    Ok(true)
                }
                _ => {
                    let s = serde_json::to_string(&Value::Object(map.clone()))
                        .map_err(MapperError::SerdeError)?;
                    *content = Value::String(format!(
                        "[alephant:message_content_object type={typ}]\n{s}"
                    ));
                    Ok(true)
                }
            }
        }
        _ => {
            *content = Value::String(content.to_string());
            Ok(true)
        }
    }
}

fn output_text_part(text: String) -> Value {
    json!({
        "type": "output_text",
        "text": text,
        "annotations": []
    })
}

fn refusal_part(refusal: String) -> Value {
    json!({
        "type": "refusal",
        "refusal": refusal
    })
}

fn normalize_assistant_message_content_json(
    content: &mut Value,
) -> Result<bool, MapperError> {
    match content {
        Value::String(_) => Ok(false),
        Value::Array(parts) => {
            if parts.is_empty() {
                *content = Value::String(String::new());
                return Ok(true);
            }
            let mut changed = false;
            let mut new_parts = Vec::with_capacity(parts.len());
            for part in parts.iter() {
                if let Some(obj) = part.as_object() {
                    let typ =
                        obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match typ {
                        "output_text" => {
                            let mut p = part.clone();
                            if let Some(map) = p.as_object_mut()
                                && !map.contains_key("annotations")
                            {
                                map.insert(
                                    "annotations".to_string(),
                                    json!([]),
                                );
                                changed = true;
                            }
                            new_parts.push(p);
                        }
                        "refusal" => {
                            let refusal = obj
                                .get("refusal")
                                .or_else(|| obj.get("text"))
                                .map(|v| match v {
                                    Value::String(s) => s.clone(),
                                    _ => v.to_string(),
                                })
                                .unwrap_or_default();
                            new_parts.push(refusal_part(refusal));
                            changed = true;
                        }
                        "text" | "input_text" => {
                            let text = json_text_field(obj.get("text"));
                            new_parts.push(output_text_part(text));
                            changed = true;
                        }
                        "" if obj.contains_key("text") => {
                            let text = json_text_field(obj.get("text"));
                            new_parts.push(output_text_part(text));
                            changed = true;
                        }
                        _ => {
                            let payload = serde_json::to_string(part)
                                .map_err(MapperError::SerdeError)?;
                            new_parts.push(output_text_part(format!(
                                "[alephant:assistant_content_part \
                                 type={typ}]\n{payload}"
                            )));
                            changed = true;
                        }
                    }
                } else {
                    new_parts.push(output_text_part(part.to_string()));
                    changed = true;
                }
            }
            *content = Value::Array(new_parts);
            Ok(changed)
        }
        Value::Object(map) => {
            let typ = map.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match typ {
                "output_text" | "text" | "input_text" => {
                    let text = json_text_field(map.get("text"));
                    *content = Value::Array(vec![output_text_part(text)]);
                    Ok(true)
                }
                "refusal" => {
                    let refusal = json_text_field(
                        map.get("refusal").or_else(|| map.get("text")),
                    );
                    *content = Value::Array(vec![refusal_part(refusal)]);
                    Ok(true)
                }
                _ if typ.is_empty() && map.contains_key("text") => {
                    let text = json_text_field(map.get("text"));
                    *content = Value::Array(vec![output_text_part(text)]);
                    Ok(true)
                }
                _ => {
                    let s = serde_json::to_string(&Value::Object(map.clone()))
                        .map_err(MapperError::SerdeError)?;
                    *content = Value::Array(vec![output_text_part(format!(
                        "[alephant:assistant_content_object type={typ}]\n{s}"
                    ))]);
                    Ok(true)
                }
            }
        }
        _ => {
            *content =
                Value::Array(vec![output_text_part(content.to_string())]);
            Ok(true)
        }
    }
}

pub(crate) fn rewrite_responses_input_items_for_create_response(
    root: &mut Value,
) -> Result<bool, MapperError> {
    rewrite_responses_input_items(
        root,
        ResponsesNormalizeMode::CreateResponseCompat,
    )
}

fn rewrite_responses_input_items(
    root: &mut Value,
    mode: ResponsesNormalizeMode,
) -> Result<bool, MapperError> {
    let Some(input) = root.get_mut("input") else {
        return Ok(false);
    };
    let Some(arr) = input.as_array_mut() else {
        return Ok(false);
    };
    let mut changed = false;
    for item in arr.iter_mut() {
        let is_message = match item.get("type").and_then(|t| t.as_str()) {
            Some("message") => true,
            None => item.get("role").is_some() && item.get("content").is_some(),
            Some(_) => false,
        };
        if is_message {
            let is_assistant =
                item.get("role").and_then(|r| r.as_str()) == Some("assistant");
            if let Some(content) = item.get_mut("content") {
                changed |= match (mode, is_assistant) {
                    (ResponsesNormalizeMode::OpenAiWire, true) => {
                        normalize_assistant_message_content_json(content)?
                    }
                    _ => normalize_responses_message_content_json(content)?,
                };
            }
            continue;
        }
        let typ_label = item
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        let payload =
            serde_json::to_string(item).map_err(MapperError::SerdeError)?;
        *item = json!({
            "type": "message",
            "role": "user",
            "content": format!(
                "[alephant:responses_input type={typ_label}]\n{payload}"
            )
        });
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod rewrite_input_tests {
    use async_openai::types::responses::CreateResponse;
    use bytes::Bytes;
    use serde_json::json;

    use super::{
        apply_openai_responses_wire_normalize, apply_responses_wire_normalize,
        apply_responses_wire_normalize_for_client,
        rewrite_responses_input_items_for_create_response,
    };
    use crate::ide_adapation::{
        client_profile::ClientProfile,
        cursor_responses_openrouter_bridge::create_response_to_chat_request,
    };

    #[test]
    fn web_search_rewritten_to_preview_and_deserializes() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-4o-mini",
                "input": "hi",
                "tools": [{ "type": "web_search" }]
            }))
            .unwrap(),
        );
        let out = apply_responses_wire_normalize(body).unwrap();
        let cr: CreateResponse = serde_json::from_slice(&out).unwrap();
        assert!(
            serde_json::to_string(&cr)
                .unwrap()
                .contains("web_search_preview")
        );
    }

    #[test]
    fn drops_nameless_hosted_tool() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-4o-mini",
                "input": "hi",
                "tools": [{ "type": "request_user_input" }]
            }))
            .unwrap(),
        );
        let out = apply_responses_wire_normalize(body).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            v.get("tools").is_none()
                || v["tools"].as_array().unwrap().is_empty()
        );
    }

    #[test]
    fn codex_tool_search_rewrites_to_function_and_deserializes() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hi",
                "tools": [{
                    "type": "tool_search",
                    "description": "Search deferred tools",
                    "execution": "client",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" },
                            "limit": { "type": "number" }
                        },
                        "required": ["query"]
                    }
                }]
            }))
            .unwrap(),
        );

        let out = apply_responses_wire_normalize_for_client(
            body,
            ClientProfile::CodexCli,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["name"], "tool_search");
        assert!(v["tools"][0].get("execution").is_none());
        let cr: CreateResponse = serde_json::from_slice(&out).unwrap();
        assert!(serde_json::to_string(&cr).unwrap().contains("tool_search"));
    }

    #[test]
    fn codex_namespace_tools_flatten_to_functions_and_deserialize() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "anthropic/claude-sonnet-4.5",
                "input": "hi",
                "tools": [{
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "description": "Tools for spawning and managing agents.",
                    "tools": [{
                        "name": "spawn_agent",
                        "description": "Spawn a sub-agent.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "message": { "type": "string" }
                            }
                        }
                    }]
                }]
            }))
            .unwrap(),
        );

        let out = apply_responses_wire_normalize_for_client(
            body,
            ClientProfile::CodexCli,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["name"], "multi_agent_v1__spawn_agent");
        assert_eq!(v["tools"][0]["description"], "Spawn a sub-agent.");
        let cr: CreateResponse = serde_json::from_slice(&out).unwrap();
        assert!(
            serde_json::to_string(&cr)
                .unwrap()
                .contains("multi_agent_v1__spawn_agent")
        );
    }

    #[test]
    fn routing_fields_accept_new_openai_service_tier_values() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hi",
                "stream": true,
                "service_tier": "priority"
            }))
            .unwrap(),
        );

        let fields = super::responses_request_routing_fields(&body).unwrap();

        assert_eq!(fields.model, "gpt-5.5");
        assert!(fields.stream);
    }

    #[test]
    fn cursor_tool_search_is_not_rewritten_by_codex_specific_normalize() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "gpt-5.5",
                "input": "hi",
                "tools": [{
                    "type": "tool_search",
                    "name": "tool_search",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" }
                        },
                        "required": ["query"]
                    }
                }]
            }))
            .unwrap(),
        );

        let out = apply_responses_wire_normalize_for_client(
            body,
            ClientProfile::CursorIde,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tools"][0]["type"], "tool_search");
        let err = serde_json::from_slice::<CreateResponse>(&out).unwrap_err();
        assert!(err.to_string().contains("unknown variant `tool_search`"));
    }

    #[test]
    fn function_call_input_rewrites_and_deserializes() {
        let mut v = json!({
            "model": "openai/gpt-4o-mini",
            "input": [
                { "type": "message", "role": "user", "content": "hi" },
                { "type": "function_call", "call_id": "c1", "name": "foo", "arguments": "{}" }
            ]
        });
        rewrite_responses_input_items_for_create_response(&mut v).unwrap();
        let cr: CreateResponse = serde_json::from_value(v).unwrap();
        let s = serde_json::to_string(&cr).unwrap();
        assert!(s.contains("function_call"));
        assert!(s.contains("alephant:responses_input"));
    }

    #[test]
    fn chat_style_text_content_array_deserializes_and_maps() {
        let mut v = json!({
            "model": "openai/gpt-4o-mini",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "text", "text": "hello from cursor" }]
            }]
        });
        rewrite_responses_input_items_for_create_response(&mut v).unwrap();
        let cr: CreateResponse = serde_json::from_value(v).unwrap();
        let chat = create_response_to_chat_request(cr).unwrap();
        assert_eq!(chat.messages.len(), 1);
    }

    #[test]
    fn openai_wire_rewrites_assistant_input_text_to_output_text() {
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "openai/gpt-5.4",
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "hello" }]
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "input_text", "text": "previous answer" }]
                    }
                ]
            }))
            .unwrap(),
        );

        let out = apply_openai_responses_wire_normalize(body).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(v["input"][1]["content"][0]["type"], "output_text");
        assert_eq!(v["input"][1]["content"][0]["text"], "previous answer");
        assert_eq!(v["input"][1]["content"][0]["annotations"], json!([]));
        let cr: CreateResponse = serde_json::from_slice(&out).unwrap();
        assert!(serde_json::to_string(&cr).unwrap().contains("output_text"));
    }

    #[test]
    fn compat_rewrite_keeps_assistant_input_text_mappable_to_chat() {
        let mut v = json!({
            "model": "openai/gpt-4o-mini",
            "input": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "input_text", "text": "previous answer" }]
            }]
        });

        rewrite_responses_input_items_for_create_response(&mut v).unwrap();
        assert_eq!(v["input"][0]["content"][0]["type"], "input_text");
        let cr: CreateResponse = serde_json::from_value(v).unwrap();
        let chat = create_response_to_chat_request(cr).unwrap();
        assert_eq!(chat.messages.len(), 1);
    }
}
