use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::Value;

use crate::error::{api::ApiError, invalid_req::InvalidRequestError};

fn invalid_prompt_inputs(message: impl Into<String>) -> ApiError {
    ApiError::InvalidRequest(InvalidRequestError::InvalidPromptInputs(
        message.into(),
    ))
}

pub(crate) fn apply_prompt_inputs_to_body(
    mut body: Value,
) -> Result<Value, ApiError> {
    let typed_variable_regex = Regex::new(
        r"\{\{\s*hc\s*:\s*([a-zA-Z_-][a-zA-Z0-9_-]*)\s*:\s*([a-zA-Z_-][a-zA-Z0-9_-]*)\s*\}\}",
    )
    .expect("typed prompt variable regex must compile");
    let legacy_variable_regex =
        Regex::new(r"\{\{\s*([a-zA-Z_-][a-zA-Z0-9_-]*)\s*\}\}")
            .expect("legacy prompt variable regex must compile");

    let Some(body_obj) = body.as_object_mut() else {
        return Ok(body);
    };

    let inputs = body_obj
        .get("inputs")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<HashMap<String, Value>>()
        });

    let has_template_variables = ["messages", "response_format", "tools"]
        .into_iter()
        .filter_map(|key| body_obj.get(key))
        .any(|value| {
            contains_prompt_variables(
                value,
                &typed_variable_regex,
                &legacy_variable_regex,
            )
        });

    if has_template_variables && inputs.is_none() {
        return Err(invalid_prompt_inputs(
            "Prompt inputs are required for template variables",
        ));
    }

    let Some(inputs) = inputs.as_ref() else {
        return Ok(body);
    };

    if let Some(messages_value) = body_obj.get_mut("messages") {
        process_messages_value(
            messages_value,
            inputs,
            &typed_variable_regex,
            &legacy_variable_regex,
        )?;
    }

    if let Some(response_format_value) = body_obj.get_mut("response_format") {
        *response_format_value = process_prompt_schema(
            response_format_value.clone(),
            inputs,
            &typed_variable_regex,
            &legacy_variable_regex,
        )?;
    }

    if let Some(tools_value) = body_obj.get_mut("tools") {
        *tools_value = process_prompt_schema(
            tools_value.clone(),
            inputs,
            &typed_variable_regex,
            &legacy_variable_regex,
        )?;
    }

    body_obj.remove("inputs");

    Ok(body)
}

fn contains_prompt_variables(
    value: &Value,
    typed_variable_regex: &Regex,
    legacy_variable_regex: &Regex,
) -> bool {
    match value {
        Value::String(text) => {
            typed_variable_regex.is_match(text)
                || legacy_variable_regex.is_match(text)
        }
        Value::Array(values) => values.iter().any(|value| {
            contains_prompt_variables(
                value,
                typed_variable_regex,
                legacy_variable_regex,
            )
        }),
        Value::Object(map) => map.iter().any(|(key, value)| {
            typed_variable_regex.is_match(key)
                || legacy_variable_regex.is_match(key)
                || contains_prompt_variables(
                    value,
                    typed_variable_regex,
                    legacy_variable_regex,
                )
        }),
        _ => false,
    }
}

fn process_messages_value(
    messages_value: &mut Value,
    inputs: &HashMap<String, Value>,
    typed_variable_regex: &Regex,
    legacy_variable_regex: &Regex,
) -> Result<(), ApiError> {
    let Some(messages_array) = messages_value.as_array_mut() else {
        return Ok(());
    };

    let mut validated_typed_variables = HashSet::new();

    for message_value in messages_array {
        process_message_variables(
            message_value,
            inputs,
            typed_variable_regex,
            legacy_variable_regex,
            &mut validated_typed_variables,
        )?;
    }

    Ok(())
}

fn process_prompt_schema(
    value: Value,
    inputs: &HashMap<String, Value>,
    typed_variable_regex: &Regex,
    legacy_variable_regex: &Regex,
) -> Result<Value, ApiError> {
    match value {
        Value::String(text) => {
            if let Some((variable_name, variable_type)) =
                extract_whole_typed_variable(&text, typed_variable_regex)
            {
                let input_value =
                    inputs.get(&variable_name).ok_or_else(|| {
                        invalid_prompt_inputs(format!(
                            "Missing prompt input: {variable_name}"
                        ))
                    })?;
                validate_variable_type(input_value, &variable_type)?;
                return Ok(input_value.clone());
            }

            if let Some(variable_name) =
                extract_whole_legacy_variable(&text, legacy_variable_regex)
            {
                let input_value =
                    inputs.get(&variable_name).ok_or_else(|| {
                        invalid_prompt_inputs(format!(
                            "Missing prompt input: {variable_name}"
                        ))
                    })?;
                return Ok(input_value.clone());
            }

            let processed_text = replace_variables(
                &text,
                inputs,
                typed_variable_regex,
                legacy_variable_regex,
                &mut HashSet::new(),
            )?;
            Ok(Value::String(processed_text))
        }
        Value::Array(arr) => {
            let mut processed_array = Vec::with_capacity(arr.len());
            for item in arr {
                processed_array.push(process_prompt_schema(
                    item,
                    inputs,
                    typed_variable_regex,
                    legacy_variable_regex,
                )?);
            }
            Ok(Value::Array(processed_array))
        }
        Value::Object(obj) => {
            let mut processed_object = serde_json::Map::new();
            for (key, val) in obj {
                let processed_key =
                    if let Some((variable_name, variable_type)) =
                        extract_whole_typed_variable(&key, typed_variable_regex)
                    {
                        let input_value =
                            inputs.get(&variable_name).ok_or_else(|| {
                                invalid_prompt_inputs(format!(
                                    "Missing prompt input: {variable_name}"
                                ))
                            })?;
                        validate_variable_type(input_value, &variable_type)?;
                        input_value.as_str().map(str::to_string).ok_or_else(
                            || {
                                invalid_prompt_inputs(format!(
                                    "Variable '{variable_name}' in object \
                                     schema key must be a string, got: \
                                     {input_value}"
                                ))
                            },
                        )?
                    } else if let Some(variable_name) =
                        extract_whole_legacy_variable(
                            &key,
                            legacy_variable_regex,
                        )
                    {
                        let input_value =
                            inputs.get(&variable_name).ok_or_else(|| {
                                invalid_prompt_inputs(format!(
                                    "Missing prompt input: {variable_name}"
                                ))
                            })?;
                        input_value.as_str().map(str::to_string).ok_or_else(
                            || {
                                invalid_prompt_inputs(format!(
                                    "Variable '{variable_name}' in object \
                                     schema key must be a string, got: \
                                     {input_value}"
                                ))
                            },
                        )?
                    } else {
                        replace_variables(
                            &key,
                            inputs,
                            typed_variable_regex,
                            legacy_variable_regex,
                            &mut HashSet::new(),
                        )?
                    };

                let processed_value = process_prompt_schema(
                    val,
                    inputs,
                    typed_variable_regex,
                    legacy_variable_regex,
                )?;
                processed_object.insert(processed_key, processed_value);
            }
            Ok(Value::Object(processed_object))
        }
        _ => Ok(value),
    }
}

fn extract_whole_typed_variable(
    text: &str,
    typed_variable_regex: &Regex,
) -> Option<(String, String)> {
    let captures = typed_variable_regex.captures(text)?;
    let full_match = captures.get(0)?;
    if full_match.as_str() != text {
        return None;
    }

    Some((
        captures.get(1)?.as_str().to_string(),
        captures.get(2)?.as_str().to_string(),
    ))
}

fn extract_whole_legacy_variable(
    text: &str,
    legacy_variable_regex: &Regex,
) -> Option<String> {
    let captures = legacy_variable_regex.captures(text)?;
    let full_match = captures.get(0)?;
    if full_match.as_str() != text {
        return None;
    }

    Some(captures.get(1)?.as_str().to_string())
}

fn process_message_variables(
    message_value: &mut Value,
    inputs: &HashMap<String, Value>,
    typed_variable_regex: &Regex,
    legacy_variable_regex: &Regex,
    validated_typed_variables: &mut HashSet<String>,
) -> Result<(), ApiError> {
    if let Some(content_value) = message_value.get_mut("content") {
        match content_value {
            Value::String(text) => {
                let processed_text = replace_variables(
                    text,
                    inputs,
                    typed_variable_regex,
                    legacy_variable_regex,
                    validated_typed_variables,
                )?;
                *content_value = Value::String(processed_text);
            }
            Value::Array(parts) => {
                for part in parts {
                    if let Some(text_value) = part.get_mut("text")
                        && let Some(text_str) = text_value.as_str()
                    {
                        let processed_text = replace_variables(
                            text_str,
                            inputs,
                            typed_variable_regex,
                            legacy_variable_regex,
                            validated_typed_variables,
                        )?;
                        *text_value = Value::String(processed_text);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn replace_variables(
    text: &str,
    inputs: &HashMap<String, Value>,
    typed_variable_regex: &Regex,
    legacy_variable_regex: &Regex,
    validated_typed_variables: &mut HashSet<String>,
) -> Result<String, ApiError> {
    for captures in typed_variable_regex.captures_iter(text) {
        let variable_name = captures
            .get(1)
            .ok_or_else(|| invalid_prompt_inputs("Invalid variable name"))?;
        let variable_type = captures
            .get(2)
            .ok_or_else(|| invalid_prompt_inputs("Invalid variable type"))?;

        if validated_typed_variables.contains(variable_name.as_str()) {
            continue;
        }

        let value = inputs.get(variable_name.as_str()).ok_or_else(|| {
            invalid_prompt_inputs(format!(
                "Missing prompt input: {}",
                variable_name.as_str()
            ))
        })?;

        validate_variable_type(value, variable_type.as_str())?;
        validated_typed_variables.insert(variable_name.as_str().to_string());
    }

    for captures in legacy_variable_regex.captures_iter(text) {
        let Some(variable_name) = captures.get(1) else {
            return Err(invalid_prompt_inputs("Invalid variable name"));
        };
        if variable_name.as_str() == "hc" {
            continue;
        }

        if !inputs.contains_key(variable_name.as_str()) {
            return Err(invalid_prompt_inputs(format!(
                "Missing prompt input: {}",
                variable_name.as_str()
            )));
        }
    }

    let typed_processed =
        typed_variable_regex.replace_all(text, |captures: &regex::Captures| {
            let variable_name = &captures[1];
            inputs.get(variable_name).map_or_else(
                || captures.get(0).unwrap().as_str().to_string(),
                Value::to_string,
            )
        });

    let legacy_processed = legacy_variable_regex.replace_all(
        typed_processed.as_ref(),
        |captures: &regex::Captures| {
            let variable_name = &captures[1];
            if variable_name == "hc" {
                return captures.get(0).unwrap().as_str().to_string();
            }

            inputs.get(variable_name).map_or_else(
                || captures.get(0).unwrap().as_str().to_string(),
                legacy_value_to_string,
            )
        },
    );

    Ok(legacy_processed.to_string())
}

fn legacy_value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

fn validate_variable_type(
    value: &Value,
    expected_type: &str,
) -> Result<String, ApiError> {
    let value_string = value.to_string();

    match expected_type {
        "number" => {
            if matches!(value, Value::Number(_)) {
                return Ok(value_string);
            }

            value_string
                .parse::<f64>()
                .map(|_| value_string.clone())
                .map_err(|_| {
                    invalid_prompt_inputs(format!(
                        "Variable value '{value_string}' cannot be converted \
                         to number"
                    ))
                })
        }
        "boolean" => {
            if matches!(value, Value::Bool(_)) {
                return Ok(value_string);
            }

            let lowercase_value = value_string.to_lowercase();
            match lowercase_value.as_str() {
                "true" | "false" | "yes" | "no" => Ok(value_string),
                _ => Err(invalid_prompt_inputs(format!(
                    "Variable value '{value_string}' is not a valid boolean \
                     (expected: true, false, yes, no)"
                ))),
            }
        }
        _ => Ok(value_string),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::apply_prompt_inputs_to_body;

    #[test]
    fn apply_prompt_inputs_errors_when_template_variables_exist_but_inputs_missing()
     {
        let body = json!({
            "messages": [
                {"role": "system", "content": "hello {{hc:name:string}}"}
            ]
        });

        let err = apply_prompt_inputs_to_body(body).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("Prompt inputs are required"));
    }

    #[test]
    fn apply_prompt_inputs_replaces_typed_and_legacy_message_content() {
        let body = json!({
            "inputs": {"name": "legend", "age": 30},
            "messages": [
                {"role": "system", "content": "typed={{hc:name:string}}, legacy={{name}}"},
                {"role": "user", "content": [
                    {"type": "text", "text": "age={{hc:age:number}}, nick={{name}}"}
                ]}
            ]
        });

        let out = apply_prompt_inputs_to_body(body).unwrap();

        assert_eq!(
            out["messages"][0]["content"],
            "typed=\"legend\", legacy=legend"
        );
        assert_eq!(
            out["messages"][1]["content"][0]["text"],
            "age=30, nick=legend"
        );
        assert!(
            out.get("inputs").is_none(),
            "`inputs` is gateway-only and must not be forwarded after \
             substitution"
        );
    }

    #[test]
    fn apply_prompt_inputs_errors_when_named_variable_missing() {
        let body = json!({
            "inputs": {},
            "messages": [
                {"role": "system", "content": "hello {{name}}"}
            ]
        });

        let err = apply_prompt_inputs_to_body(body).unwrap_err();
        assert!(err.to_string().contains("Missing prompt input: name"));
    }

    #[test]
    fn apply_prompt_inputs_errors_when_number_type_invalid() {
        let body = json!({
            "inputs": {"age": "abc"},
            "messages": [
                {"role": "system", "content": "age={{hc:age:number}}"}
            ]
        });

        let err = apply_prompt_inputs_to_body(body).unwrap_err();
        assert!(err.to_string().contains("cannot be converted to number"));
    }

    #[test]
    fn apply_prompt_inputs_processes_response_format_and_tools() {
        let body = json!({
            "inputs": {"field_name": "topic"},
            "response_format": {
                "json_schema": {
                    "schema": {
                        "properties": {
                            "{{field_name}}": {"type": "string"}
                        }
                    }
                }
            },
            "tools": [{
                "function": {
                    "name": "lookup_{{field_name}}"
                }
            }]
        });

        let out = apply_prompt_inputs_to_body(body).unwrap();

        assert!(
            out["response_format"]["json_schema"]["schema"]["properties"]
                .get("topic")
                .is_some()
        );
        assert_eq!(out["tools"][0]["function"]["name"], "lookup_topic");
        assert!(out.get("inputs").is_none());
    }

    #[test]
    fn apply_prompt_inputs_allows_body_without_variables_and_without_inputs() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "hello world"}
            ]
        });

        let out = apply_prompt_inputs_to_body(body.clone()).unwrap();

        assert_eq!(out, body);
    }

    #[test]
    fn apply_prompt_inputs_strips_inputs_when_present_but_no_template_variables()
     {
        let body = json!({
            "inputs": {"name": "x"},
            "messages": [{"role": "user", "content": "plain"}]
        });

        let out = apply_prompt_inputs_to_body(body).unwrap();

        assert_eq!(out["messages"][0]["content"], "plain");
        assert!(out.get("inputs").is_none());
    }
}
