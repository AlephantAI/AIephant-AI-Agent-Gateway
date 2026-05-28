//! Lenient parsing of upstream HTTP error bodies into OpenAI-shaped
//! [`WrappedError`].
//!
//! Some OpenAI-compatible providers (for example SiliconFlow) return `4xx` JSON
//! that does not wrap details in a top-level `"error"` object. The mapper
//! otherwise deserializes strictly as [`WrappedError`] and surfaces a confusing
//! `500` when serde fails.

use async_openai::error::{ApiError, WrappedError};
use serde_json::{Map, Value};

const MESSAGE_MAX_CHARS: usize = 8192;

/// Deserialize bytes into [`WrappedError`], accepting strict OpenAI shape and
/// several common provider variants.
#[must_use]
pub(crate) fn deserialize_wrapped_error_lenient(bytes: &[u8]) -> WrappedError {
    if let Ok(w) = serde_json::from_slice::<WrappedError>(bytes) {
        return w;
    }

    let value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => Value::String(trim_lossy_message(bytes)),
    };

    wrapped_error_from_json_value(&value)
}

fn trim_lossy_message(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes).trim().to_string();
    truncate_chars(&s, MESSAGE_MAX_CHARS)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn wrapped_error_from_json_value(v: &Value) -> WrappedError {
    if let Some(err) = v.get("error") {
        match err {
            Value::String(s) => {
                return WrappedError {
                    error: ApiError {
                        message: truncate_chars(s, MESSAGE_MAX_CHARS),
                        r#type: Some("invalid_request_error".to_string()),
                        param: None,
                        code: None,
                    },
                };
            }
            Value::Object(map) => {
                return WrappedError {
                    error: api_error_from_object(map),
                };
            }
            _ => {}
        }
    }

    if let Some(map) = v.as_object() {
        return WrappedError {
            error: api_error_from_object(map),
        };
    }

    let message = match v {
        Value::String(s) => truncate_chars(s, MESSAGE_MAX_CHARS),
        _ => truncate_chars(&v.to_string(), MESSAGE_MAX_CHARS),
    };

    WrappedError {
        error: ApiError {
            message,
            r#type: Some("invalid_request_error".to_string()),
            param: None,
            code: None,
        },
    }
}

fn api_error_from_object(map: &Map<String, Value>) -> ApiError {
    let message = map
        .get("message")
        .or_else(|| map.get("msg"))
        .and_then(value_as_message_fragment)
        .or_else(|| {
            map.get("detail").map(|d| {
                if let Value::String(s) = d {
                    s.clone()
                } else {
                    d.to_string()
                }
            })
        })
        .unwrap_or_else(|| {
            if map.is_empty() {
                "upstream error".to_string()
            } else {
                Value::Object(map.clone()).to_string()
            }
        });
    let message = truncate_chars(&message, MESSAGE_MAX_CHARS);

    let r#type = map
        .get("type")
        .or_else(|| map.get("error_type"))
        .map(|v| value_as_message_fragment(v).unwrap_or_else(|| v.to_string()))
        .filter(|s| !s.is_empty())
        .or_else(|| Some("invalid_request_error".to_string()));

    let param = map
        .get("param")
        .and_then(|p| p.as_str().map(std::string::ToString::to_string));

    let code = map
        .get("code")
        .map(|c| match c {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => c.to_string(),
        })
        .filter(|s| !s.is_empty());

    ApiError {
        message,
        r#type,
        param,
        code,
    }
}

fn value_as_message_fragment(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        Value::Array(_) | Value::Object(_) => Some(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_openai_shape_round_trips() {
        let bytes = br#"{"error":{"message":"bad","type":"invalid_request_error","param":null,"code":"x"}}"#;
        let w = deserialize_wrapped_error_lenient(bytes);
        assert_eq!(w.error.message, "bad");
        assert_eq!(w.error.r#type, Some("invalid_request_error".to_string()));
        assert_eq!(w.error.code, Some("x".to_string()));
    }

    #[test]
    fn flat_message_and_numeric_code() {
        let bytes = br#"{"message":"model not found","code":20012}"#;
        let w = deserialize_wrapped_error_lenient(bytes);
        assert_eq!(w.error.message, "model not found");
        assert_eq!(w.error.code, Some("20012".to_string()));
    }

    #[test]
    fn error_field_as_string() {
        let bytes = br#"{"error":"plain text"}"#;
        let w = deserialize_wrapped_error_lenient(bytes);
        assert_eq!(w.error.message, "plain text");
    }

    #[test]
    fn non_json_becomes_message() {
        let bytes = b"upstream refused: plain text body";
        let w = deserialize_wrapped_error_lenient(bytes);
        assert!(w.error.message.contains("upstream refused"));
    }
}
