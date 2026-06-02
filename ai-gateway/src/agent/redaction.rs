use serde_json::Value;

const REDACTED: &str = "[redacted]";
const SENSITIVE_KEYS: [&str; 13] = [
    "authorization",
    "cookie",
    "set-cookie",
    "api_key",
    "apikey",
    "x-api-key",
    "token",
    "access_token",
    "refresh_token",
    "password",
    "secret",
    "client_secret",
    "payment_signature",
];

#[must_use]
pub fn redact_metadata(mut value: Value) -> Value {
    redact_value(&mut value);
    value
}

fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if is_sensitive_key(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEYS
        .iter()
        .any(|sensitive| lower == *sensitive || lower.contains(sensitive))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_sensitive_metadata_keys_recursively() {
        let input = json!({
            "authorization": "Bearer secret",
            "Authorization_Header": "Bearer also-secret",
            "nested": {
                "api_key": "sk-test",
                "request": {
                    "set-cookie": "session=secret"
                },
                "safe": "value"
            },
            "items": [
                { "password": "pw" },
                { "name": "kept" }
            ]
        });

        let redacted = redact_metadata(input);

        assert_eq!(redacted["authorization"], "[redacted]");
        assert_eq!(redacted["Authorization_Header"], "[redacted]");
        assert_eq!(redacted["nested"]["api_key"], "[redacted]");
        assert_eq!(redacted["nested"]["request"]["set-cookie"], "[redacted]");
        assert_eq!(redacted["nested"]["safe"], "value");
        assert_eq!(redacted["items"][0]["password"], "[redacted]");
        assert_eq!(redacted["items"][1]["name"], "kept");
    }
}
