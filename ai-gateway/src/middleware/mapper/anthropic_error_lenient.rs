//! Lenient parsing of upstream HTTP error bodies into Anthropic-shaped
//! [`AnthropicApiError`].
//!
//! Some Anthropic-compatible providers return non-standard JSON (or even plain
//! text / HTML) on `4xx`/`5xx`. Strict deserialization into
//! [`AnthropicApiError`] can then fail and incorrectly surface a proxy `500`.

use serde_json::Value;

use crate::endpoints::anthropic::messages::{AnthropicApiError, ErrorDetails};

const MESSAGE_MAX_CHARS: usize = 8192;
const DEFAULT_ERROR_TYPE: &str = "invalid_request_error";
const DEFAULT_TOP_LEVEL_TYPE: &str = "error";

/// Deserialize bytes into [`AnthropicApiError`], accepting strict Anthropic
/// shape and several common provider variants.
#[must_use]
pub(crate) fn deserialize_anthropic_error_lenient(
    bytes: &[u8],
) -> AnthropicApiError {
    if let Ok(v) = serde_json::from_slice::<AnthropicApiError>(bytes) {
        return v;
    }

    let value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => Value::String(trim_lossy_message(bytes)),
    };

    anthropic_error_from_json_value(&value)
}

fn anthropic_error_from_json_value(v: &Value) -> AnthropicApiError {
    if let Some(err_value) = v.get("error") {
        match err_value {
            Value::String(s) => {
                return AnthropicApiError {
                    error: ErrorDetails {
                        message: truncate_chars(s, MESSAGE_MAX_CHARS),
                        kind: DEFAULT_ERROR_TYPE.to_string(),
                    },
                    kind: DEFAULT_TOP_LEVEL_TYPE.to_string(),
                };
            }
            Value::Object(map) => {
                let message = map
                    .get("message")
                    .or_else(|| map.get("msg"))
                    .map(value_to_message_fragment)
                    .unwrap_or_else(|| {
                        if map.is_empty() {
                            "upstream error".to_string()
                        } else {
                            Value::Object(map.clone()).to_string()
                        }
                    });
                let error_type = map.get("type").map_or_else(
                    || DEFAULT_ERROR_TYPE.to_string(),
                    value_to_message_fragment,
                );
                let top_level_type = v.get("type").map_or_else(
                    || DEFAULT_TOP_LEVEL_TYPE.to_string(),
                    value_to_message_fragment,
                );
                return AnthropicApiError {
                    error: ErrorDetails {
                        message: truncate_chars(&message, MESSAGE_MAX_CHARS),
                        kind: truncate_chars(&error_type, 256),
                    },
                    kind: truncate_chars(&top_level_type, 256),
                };
            }
            _ => {}
        }
    }

    if let Some(map) = v.as_object() {
        let message = map
            .get("message")
            .or_else(|| map.get("msg"))
            .or_else(|| map.get("detail"))
            .map(value_to_message_fragment)
            .unwrap_or_else(|| {
                if map.is_empty() {
                    "upstream error".to_string()
                } else {
                    Value::Object(map.clone()).to_string()
                }
            });
        let error_type = map
            .get("type")
            .or_else(|| map.get("error_type"))
            .map_or_else(
                || DEFAULT_ERROR_TYPE.to_string(),
                value_to_message_fragment,
            );
        return AnthropicApiError {
            error: ErrorDetails {
                message: truncate_chars(&message, MESSAGE_MAX_CHARS),
                kind: truncate_chars(&error_type, 256),
            },
            kind: DEFAULT_TOP_LEVEL_TYPE.to_string(),
        };
    }

    let message = match v {
        Value::String(s) => truncate_chars(s, MESSAGE_MAX_CHARS),
        _ => truncate_chars(&v.to_string(), MESSAGE_MAX_CHARS),
    };

    AnthropicApiError {
        error: ErrorDetails {
            message,
            kind: DEFAULT_ERROR_TYPE.to_string(),
        },
        kind: DEFAULT_TOP_LEVEL_TYPE.to_string(),
    }
}

fn trim_lossy_message(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes).trim().to_string();
    truncate_chars(&s, MESSAGE_MAX_CHARS)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn value_to_message_fragment(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_anthropic_shape_round_trips() {
        let bytes = br#"{"type":"error","error":{"type":"invalid_request_error","message":"bad request"}}"#;
        let e = deserialize_anthropic_error_lenient(bytes);
        assert_eq!(e.kind, "error");
        assert_eq!(e.error.kind, "invalid_request_error");
        assert_eq!(e.error.message, "bad request");
    }

    #[test]
    fn flat_json_message_is_accepted() {
        let bytes = br#"{"message":"route not found","type":"not_found"}"#;
        let e = deserialize_anthropic_error_lenient(bytes);
        assert_eq!(e.kind, "error");
        assert_eq!(e.error.kind, "not_found");
        assert_eq!(e.error.message, "route not found");
    }

    #[test]
    fn non_json_body_becomes_message() {
        let bytes = b"<html>404 page not found</html>";
        let e = deserialize_anthropic_error_lenient(bytes);
        assert_eq!(e.kind, "error");
        assert_eq!(e.error.kind, "invalid_request_error");
        assert!(e.error.message.contains("404 page not found"));
    }
}
