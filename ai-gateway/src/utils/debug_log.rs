use serde_json::Value;

pub(crate) const DEBUG_BODY_MAX_LOG_BYTES: usize = 256 * 1024;
const DEBUG_HEADERS_ENV: &str = "AI_GATEWAY_DEBUG_HEADERS";
const DEBUG_BODY_ENV: &str = "AI_GATEWAY_DEBUG_BODY";
const DEBUG_HEADERS_HEADER: &str = "alephant-debug-headers";
const DEBUG_BODY_HEADER: &str = "alephant-debug-body";
const REDACTED: &str = "<redacted>";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct DebugLogConfig {
    pub(crate) headers: bool,
    pub(crate) body: bool,
}

impl DebugLogConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            headers: debug_headers_enabled(),
            body: debug_body_enabled(),
        }
    }

    pub(crate) fn from_headers(headers: &mut http::HeaderMap) -> Self {
        Self::from_headers_with_env(headers, |key| {
            std::env::var_os(key).map(|_| "")
        })
    }

    pub(crate) fn from_headers_with_env<'a>(
        headers: &mut http::HeaderMap,
        getenv: impl Fn(&str) -> Option<&'a str>,
    ) -> Self {
        let env_headers = debug_headers_enabled_with(&getenv);
        let env_body = debug_body_enabled_with(getenv);
        let header_override =
            take_debug_bool_header(headers, DEBUG_HEADERS_HEADER);
        let body_override = take_debug_bool_header(headers, DEBUG_BODY_HEADER);
        Self {
            headers: header_override.unwrap_or(env_headers),
            body: body_override.unwrap_or(env_body),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DebugBodyPreview {
    pub(crate) body_len: usize,
    pub(crate) truncated: bool,
    pub(crate) body: String,
}

pub(crate) fn remove_debug_control_headers(headers: &mut http::HeaderMap) {
    headers.remove(DEBUG_HEADERS_HEADER);
    headers.remove(DEBUG_BODY_HEADER);
}

fn take_debug_bool_header(
    headers: &mut http::HeaderMap,
    name: &'static str,
) -> Option<bool> {
    let value = headers.remove(name)?;
    value.to_str().ok().and_then(parse_debug_bool_header_value)
}

fn parse_debug_bool_header_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn debug_headers_enabled() -> bool {
    debug_headers_enabled_with(|key| std::env::var_os(key).map(|_| ""))
}

pub(crate) fn debug_body_enabled() -> bool {
    debug_body_enabled_with(|key| std::env::var_os(key).map(|_| ""))
}

fn debug_headers_enabled_with<'a>(
    getenv: impl Fn(&str) -> Option<&'a str>,
) -> bool {
    getenv(DEBUG_HEADERS_ENV).is_some()
}

fn debug_body_enabled_with<'a>(
    getenv: impl Fn(&str) -> Option<&'a str>,
) -> bool {
    getenv(DEBUG_BODY_ENV).is_some()
}

fn header_value_should_redact_for_debug_log(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
        || name.eq_ignore_ascii_case("cookie")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("x-api-key")
}

pub(crate) fn debug_header_lines(headers: &http::HeaderMap) -> String {
    let mut lines = Vec::new();
    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        let display = if header_value_should_redact_for_debug_log(name_str) {
            "<redacted>"
        } else {
            value.to_str().unwrap_or("<non-utf8>")
        };
        lines.push(format!("{name_str}: {display}"));
    }
    lines.join("\n")
}

pub(crate) fn debug_body_preview(body: &[u8]) -> DebugBodyPreview {
    debug_body_preview_with_limit(body, DEBUG_BODY_MAX_LOG_BYTES)
}

pub(crate) fn debug_body_preview_with_limit(
    body: &[u8],
    max_log_bytes: usize,
) -> DebugBodyPreview {
    let body_len = body.len();
    let display_body = redacted_json_body(body)
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let truncated = display_body.len() > max_log_bytes;
    let body = if truncated {
        String::from_utf8_lossy(&display_body.as_bytes()[..max_log_bytes])
            .into_owned()
    } else {
        display_body
    };

    DebugBodyPreview {
        body_len,
        truncated,
        body,
    }
}

fn redacted_json_body(body: &[u8]) -> Option<String> {
    let mut value = serde_json::from_slice::<Value>(body).ok()?;
    redact_json_value(&mut value);
    serde_json::to_string(&value).ok()
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if json_key_should_redact(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json_value(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn json_key_should_redact(key: &str) -> bool {
    let compact = key
        .chars()
        .filter(|ch| !matches!(ch, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect::<String>();

    compact == "authorization"
        || compact == "key"
        || compact == "password"
        || compact == "secret"
        || compact == "token"
        || compact.ends_with("apikey")
        || compact.ends_with("accesstoken")
        || compact.ends_with("refreshtoken")
        || compact.ends_with("idtoken")
        || compact.ends_with("secretkey")
        || compact.ends_with("privatekey")
        || compact.ends_with("providerkey")
}

pub(crate) fn maybe_log_headers(
    scope: &'static str,
    headers: &http::HeaderMap,
    config: DebugLogConfig,
) {
    if !config.headers {
        return;
    }
    let joined = debug_header_lines(headers);
    tracing::info!(
        %joined,
        "{scope}: request headers (debug headers enabled)"
    );
}

pub(crate) fn maybe_log_body(
    scope: &'static str,
    body: &[u8],
    config: DebugLogConfig,
) {
    if !config.body {
        return;
    }
    let preview = debug_body_preview(body);
    tracing::info!(
        body_len = preview.body_len,
        truncated = preview.truncated,
        body = %preview.body,
        "{scope}: request body (debug body enabled)"
    );
}

pub(crate) fn maybe_log_body_with_target(
    scope: &'static str,
    target_url: impl std::fmt::Display,
    body: &[u8],
    config: DebugLogConfig,
) {
    if !config.body {
        return;
    }
    let preview = debug_body_preview(body);
    tracing::info!(
        target_url = %target_url,
        body_len = preview.body_len,
        truncated = preview.truncated,
        body = %preview.body,
        "{scope} body (debug body enabled)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_body_preview_keeps_small_plain_body() {
        let preview = debug_body_preview_with_limit(b"hello", 32);

        assert_eq!(preview.body_len, 5);
        assert!(!preview.truncated);
        assert_eq!(preview.body, "hello");
    }

    #[test]
    fn debug_body_preview_truncates_large_plain_body() {
        let preview = debug_body_preview_with_limit(b"abcdef", 3);

        assert_eq!(preview.body_len, 6);
        assert!(preview.truncated);
        assert_eq!(preview.body, "abc");
    }

    #[test]
    fn debug_body_preview_redacts_sensitive_json_keys_recursively() {
        let body = br#"{
            "api_key": "sk-secret",
            "nested": {
                "authorization": "Bearer secret",
                "items": [
                    {"access_token": "access-secret"},
                    {"safe": "visible"}
                ]
            }
        }"#;

        let preview = debug_body_preview_with_limit(body, 1024);

        assert!(!preview.body.contains("sk-secret"));
        assert!(!preview.body.contains("Bearer secret"));
        assert!(!preview.body.contains("access-secret"));
        assert!(preview.body.contains("\"api_key\":\"<redacted>\""));
        assert!(preview.body.contains("\"authorization\":\"<redacted>\""));
        assert!(preview.body.contains("\"access_token\":\"<redacted>\""));
        assert!(preview.body.contains("\"safe\":\"visible\""));
    }

    #[test]
    fn debug_body_preview_handles_case_insensitive_json_keys() {
        let body = br#"{"ApiKey":"secret","RefreshToken":"refresh"}"#;

        let preview = debug_body_preview_with_limit(body, 1024);

        assert!(!preview.body.contains("secret"));
        assert!(!preview.body.contains("refresh"));
        assert!(preview.body.contains("\"ApiKey\":\"<redacted>\""));
        assert!(preview.body.contains("\"RefreshToken\":\"<redacted>\""));
    }

    #[test]
    fn debug_body_preview_keeps_non_json_lossy_utf8() {
        let preview = debug_body_preview_with_limit(&[b'o', b'k', 0xff], 32);

        assert_eq!(preview.body_len, 3);
        assert!(!preview.truncated);
        assert_eq!(preview.body, "ok\u{fffd}");
    }

    #[test]
    fn debug_headers_switch_uses_unified_env_name_only() {
        assert!(super::debug_headers_enabled_with(|key| {
            (key == "AI_GATEWAY_DEBUG_HEADERS").then_some("true")
        }));
        assert!(!super::debug_headers_enabled_with(|key| {
            (key == "AI_GATEWAY_DEBUG_REQUEST_HEADERS").then_some("true")
        }));
    }

    #[test]
    fn debug_body_switch_uses_unified_env_name_only() {
        assert!(super::debug_body_enabled_with(|key| {
            (key == "AI_GATEWAY_DEBUG_BODY").then_some("true")
        }));
        assert!(!super::debug_body_enabled_with(|key| {
            (key == "AI_GATEWAY_DEBUG_REQUEST_BODY").then_some("true")
        }));
    }

    #[test]
    fn debug_log_config_uses_env_defaults_without_request_headers() {
        let mut headers = http::HeaderMap::new();
        let cfg =
            super::DebugLogConfig::from_headers_with_env(&mut headers, |key| {
                (key == "AI_GATEWAY_DEBUG_HEADERS"
                    || key == "AI_GATEWAY_DEBUG_BODY")
                    .then_some("true")
            });

        assert!(cfg.headers);
        assert!(cfg.body);
    }

    #[test]
    fn debug_log_config_request_headers_override_env_and_are_removed() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "alephant-debug-headers",
            http::HeaderValue::from_static("false"),
        );
        headers.insert(
            "alephant-debug-body",
            http::HeaderValue::from_static("true"),
        );

        let cfg =
            super::DebugLogConfig::from_headers_with_env(&mut headers, |key| {
                (key == "AI_GATEWAY_DEBUG_HEADERS").then_some("true")
            });

        assert!(!cfg.headers);
        assert!(cfg.body);
        assert!(!headers.contains_key("alephant-debug-headers"));
        assert!(!headers.contains_key("alephant-debug-body"));
    }
}
