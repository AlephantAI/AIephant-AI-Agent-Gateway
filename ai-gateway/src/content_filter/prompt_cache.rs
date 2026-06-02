//! Redis `prompt:cache:{workspace_id}:{prompt_id}` + header
//! `Alephant-Prompt-ID`: merge `template.messages` into the forward JSON body
//! (call order vs policy is defined in mapper/dispatcher; see policy-order
//! spec).

use std::sync::Arc;

use bytes::Bytes;
use http::header::HeaderName;
use opentelemetry::KeyValue;
use serde_json::Value;
use tracing::{info, warn};

use crate::{
    app_redis::AppRedis,
    error::{api::ApiError, invalid_req::InvalidRequestError},
    metrics::VkMetrics,
    middleware::prompts::templating::apply_prompt_inputs_to_body,
    types::extensions::PromptHeaderForRequestLog,
};

/// Lowercase HTTP header name (required for `HeaderName::from_static`).
pub static ALEPHANT_PROMPT_ID: HeaderName =
    HeaderName::from_static("alephant-prompt-id");

pub const PROMPT_CACHE_REDIS_PREFIX: &str = "prompt:cache:";

/// Maximum UTF-8 byte length for `Alephant-Prompt-ID` after trim; longer values
/// yield HTTP 400.
pub const MAX_PROMPT_ID_BYTES: usize = 256;

#[must_use]
pub fn prompt_cache_redis_key(workspace_id: &str, prompt_id: &str) -> String {
    let mut s = String::with_capacity(
        PROMPT_CACHE_REDIS_PREFIX.len()
            + workspace_id.len()
            + 1
            + prompt_id.len(),
    );
    s.push_str(PROMPT_CACHE_REDIS_PREFIX);
    s.push_str(workspace_id);
    s.push(':');
    s.push_str(prompt_id);
    s
}

/// From a parsed Redis root `Value`, read `template.messages` (must be an
/// array, may be empty).
pub fn template_messages_from_prompt_cache_value(
    root: &Value,
) -> Result<Vec<Value>, String> {
    let Some(template) = root.get("template") else {
        return Err("`template` missing in prompt cache".to_string());
    };
    let Some(messages) = template.get("messages") else {
        return Err("`template.messages` missing in prompt cache".to_string());
    };
    let Some(arr) = messages.as_array() else {
        return Err("`template.messages` must be a JSON array".to_string());
    };
    Ok(arr.clone())
}

/// On Redis hit with non-empty string, parse and read
/// `template.messages` (must be an array, may be empty).
pub fn template_messages_from_prompt_cache_json(
    redis_value: &str,
) -> Result<Vec<Value>, String> {
    let root: Value = serde_json::from_str(redis_value)
        .map_err(|_| "Invalid prompt cache JSON".to_string())?;
    template_messages_from_prompt_cache_value(&root)
}

/// Design `template.current_version`: non-empty string, or decimal string of a
/// JSON number; else `None`.
#[must_use]
pub fn current_version_for_request_log_from_cache_root(
    root: &Value,
) -> Option<String> {
    let current = root.get("template")?.get("current_version")?;
    if current.is_null() {
        return None;
    }
    if let Some(s) = current.as_str() {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        return Some(t.to_owned());
    }
    current.as_number().map(std::string::ToString::to_string)
}

/// Prepend `prefix` onto `root["messages"]`; if `messages` is missing, set it
/// to `prefix`. `prefix` must be non-empty (caller should skip when `prefix` is
/// empty).
pub fn prepend_messages_array(
    mut root: Value,
    prefix: Vec<Value>,
) -> Result<Value, String> {
    if prefix.is_empty() {
        return Ok(root);
    }
    let Value::Object(ref mut map) = root else {
        return Err("Request body must be a JSON object".to_string());
    };
    match map.remove("messages") {
        None => {
            map.insert("messages".to_string(), Value::Array(prefix));
        }
        Some(Value::Array(mut existing)) => {
            let mut merged = prefix;
            merged.append(&mut existing);
            map.insert("messages".to_string(), Value::Array(merged));
        }
        Some(_) => {
            return Err("`messages` must be a JSON array".to_string());
        }
    }
    Ok(root)
}

fn prompt_id_from_headers(headers: &http::HeaderMap) -> Option<&str> {
    headers
        .get(&ALEPHANT_PROMPT_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// True when [`merge_prompt_cache_messages_into_body`] treats the request as
/// carrying a usable `Alephant-Prompt-ID` (trimmed non-empty). Used by mapper
/// and dispatcher to order merge before policy when the header is present; see
/// `docs/superpowers/specs/2026-04-25-alephant-prompt-id-policy-order-design.
/// md`.
#[must_use]
pub fn has_nonempty_alephant_prompt_id(headers: &http::HeaderMap) -> bool {
    prompt_id_from_headers(headers).is_some()
}

fn invalid_prompt_cache(message: String) -> ApiError {
    ApiError::InvalidRequest(InvalidRequestError::PromptCacheInvalid {
        message,
    })
}

fn merge_prompt_cache_hit_into_body(
    prompt_id_raw: &str,
    redis_key: &str,
    redis_str: &str,
    forward_body: Bytes,
    vk_metrics: &VkMetrics,
) -> Result<(Bytes, Option<PromptHeaderForRequestLog>), ApiError> {
    let cache_root: Value = match serde_json::from_str(redis_str) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                key = %redis_key,
                prompt_id = %prompt_id_raw,
                error = %e,
                "prompt cache invalid JSON, fail-open"
            );
            vk_metrics
                .policy_prompt_cache_messages
                .add(1, &[KeyValue::new("outcome", "cache_invalid_skip")]);
            return Ok((forward_body, None));
        }
    };
    let template_messages =
        match template_messages_from_prompt_cache_value(&cache_root) {
            Ok(m) => m,
            Err(msg) => {
                warn!(
                    key = %redis_key,
                    prompt_id = %prompt_id_raw,
                    reason = %msg,
                    "prompt cache invalid shape, fail-open"
                );
                vk_metrics
                    .policy_prompt_cache_messages
                    .add(1, &[KeyValue::new("outcome", "cache_invalid_skip")]);
                return Ok((forward_body, None));
            }
        };

    let header_log = PromptHeaderForRequestLog {
        prompt_id: prompt_id_raw.to_owned(),
        prompt_version: current_version_for_request_log_from_cache_root(
            &cache_root,
        ),
    };

    if template_messages.is_empty() {
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "hit_noop_empty_template")]);
        return Ok((forward_body, Some(header_log.clone())));
    }

    let mut body_root: Value =
        serde_json::from_slice(&forward_body).map_err(|_| {
            vk_metrics
                .policy_prompt_cache_messages
                .add(1, &[KeyValue::new("outcome", "parse_or_shape_400")]);
            invalid_prompt_cache(
                "Request body must be valid JSON when using Alephant-Prompt-ID"
                    .to_string(),
            )
        })?;

    body_root = match prepend_messages_array(body_root, template_messages) {
        Ok(v) => v,
        Err(msg) => {
            vk_metrics
                .policy_prompt_cache_messages
                .add(1, &[KeyValue::new("outcome", "parse_or_shape_400")]);
            return Err(invalid_prompt_cache(msg));
        }
    };

    body_root = apply_prompt_inputs_to_body(body_root)?;

    let out = serde_json::to_vec(&body_root).map_err(|e| {
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "parse_or_shape_400")]);
        invalid_prompt_cache(format!("Failed to serialize request body: {e}"))
    })?;

    vk_metrics
        .policy_prompt_cache_messages
        .add(1, &[KeyValue::new("outcome", "hit_injected")]);

    Ok((Bytes::from(out), Some(header_log)))
}

/// When `Alephant-Prompt-ID` is present (trimmed non-empty), merge Redis
/// `prompt:cache:*` template `messages` into the JSON body (and apply
/// `inputs`-style templating). Callers may run this **before** or **after**
/// policy `Evaluate`; when run **before**, token estimates and PII-attached
/// `EvaluateRequest.body` see the merged payload (per
/// `docs/superpowers/specs/2026-04-25-alephant-prompt-id-policy-order-design.
/// md`).
pub async fn merge_prompt_cache_messages_into_body(
    redis: Option<&Arc<AppRedis>>,
    headers: &http::HeaderMap,
    workspace_id: &str,
    forward_body: Bytes,
    vk_metrics: &VkMetrics,
) -> Result<(Bytes, Option<PromptHeaderForRequestLog>), ApiError> {
    let Some(prompt_id_raw) = prompt_id_from_headers(headers) else {
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "skipped_no_header")]);
        return Ok((forward_body, None));
    };

    if workspace_id.is_empty() {
        info!(
            "Alephant-Prompt-ID prompt_cache: Redis GET skipped (empty \
             workspace_id); prompt_id={prompt_id_raw}"
        );
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "skipped_no_workspace")]);
        return Ok((forward_body, None));
    }

    if prompt_id_raw.len() > MAX_PROMPT_ID_BYTES {
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "prompt_id_too_long_400")]);
        return Err(invalid_prompt_cache(format!(
            "Alephant-Prompt-ID exceeds maximum length ({MAX_PROMPT_ID_BYTES} \
             bytes)"
        )));
    }

    let key = prompt_cache_redis_key(workspace_id, prompt_id_raw);
    let Some(r) = redis else {
        info!(
            "Alephant-Prompt-ID prompt_cache: redis_key={key} Redis GET not \
             executed (no redis client)"
        );
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "no_redis")]);
        return Ok((forward_body, None));
    };

    info!("Alephant-Prompt-ID prompt_cache: Redis GET redis_key={key}");
    let raw_opt = match r.get_opt_string(&key).await {
        Ok(v) => v,
        Err(e) => {
            info!(
                "Alephant-Prompt-ID prompt_cache: redis_key={key} Redis GET \
                 error: {e}"
            );
            warn!(key = %key, error = %e, "prompt cache redis GET failed");
            vk_metrics
                .policy_prompt_cache_messages
                .add(1, &[KeyValue::new("outcome", "redis_error")]);
            return Ok((forward_body, None));
        }
    };

    info!(
        "Alephant-Prompt-ID prompt_cache: redis_key={key} redis_value={}",
        match &raw_opt {
            None => "<nil>".to_string(),
            Some(s) if s.is_empty() => "<empty string>".to_string(),
            Some(s) => s.clone(),
        }
    );

    let Some(ref redis_str) = raw_opt else {
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "miss")]);
        return Ok((forward_body, None));
    };

    if redis_str.is_empty() {
        vk_metrics
            .policy_prompt_cache_messages
            .add(1, &[KeyValue::new("outcome", "miss")]);
        return Ok((forward_body, None));
    }

    merge_prompt_cache_hit_into_body(
        prompt_id_raw,
        &key,
        redis_str,
        forward_body,
        vk_metrics,
    )
}

#[cfg(test)]
mod tests {
    use axum_core::response::IntoResponse;
    use http::{HeaderValue, StatusCode};
    use opentelemetry::global;
    use serde_json::json;

    use super::*;
    use crate::{error::invalid_req::InvalidRequestError, metrics::VkMetrics};

    const SAMPLE_REDIS: &str = r#"{
        "channel": "prompt.change.notify",
        "revision": "1776088795476",
        "reason": "test001212",
        "template": {
            "slug": "test001212",
            "messages": [
                {"role": "system", "content": "11111"},
                {"role": "user", "content": "222222"}
            ]
        }
    }"#;

    #[test]
    fn template_messages_extracts_array() {
        let got =
            template_messages_from_prompt_cache_json(SAMPLE_REDIS).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["role"], json!("system"));
        assert_eq!(got[1]["content"], json!("222222"));
    }

    #[test]
    fn template_missing_errors() {
        let err = template_messages_from_prompt_cache_json("{}").unwrap_err();
        assert!(err.contains("template"));
    }

    #[test]
    fn messages_missing_errors() {
        let err =
            template_messages_from_prompt_cache_json(r#"{"template":{}}"#)
                .unwrap_err();
        assert!(err.contains("template.messages"));
    }

    #[test]
    fn messages_not_array_errors() {
        let err = template_messages_from_prompt_cache_json(
            r#"{"template":{"messages":{}}}"#,
        )
        .unwrap_err();
        assert!(err.contains("array"));
    }

    #[test]
    fn prepend_puts_template_first() {
        let root = json!({"messages":[{"role":"user","content":"x"}]});
        let prefix = vec![json!({"role":"system","content":"s"})];
        let out = prepend_messages_array(root, prefix).unwrap();
        let arr = out["messages"].as_array().unwrap();
        assert_eq!(arr[0]["role"], "system");
        assert_eq!(arr[1]["role"], "user");
    }

    #[test]
    fn prepend_creates_messages_when_absent() {
        let root = json!({"model":"gpt-4"});
        let prefix = vec![json!({"role":"user","content":"hi"})];
        let out = prepend_messages_array(root, prefix).unwrap();
        assert_eq!(out["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn prompt_cache_template_messages_can_use_legacy_inputs() {
        let body = json!({
            "model": "openai/gpt-4",
            "inputs": {"name": "legend"},
            "messages": [{"role":"user","content":"Please generate a greeting from the template."}]
        });
        let template_messages = vec![json!({
            "role": "system",
            "content": "Name: {{name}}"
        })];

        let merged = prepend_messages_array(body, template_messages).unwrap();
        let out = crate::middleware::prompts::templating::apply_prompt_inputs_to_body(merged)
            .unwrap();

        assert_eq!(out["messages"][0]["content"], "Name: legend");
        assert!(out.get("inputs").is_none());
    }

    #[test]
    fn prepend_rejects_non_array_messages() {
        let root = json!({"messages":"not-array"});
        let err = prepend_messages_array(root, vec![json!("x")]).unwrap_err();
        assert!(err.contains("messages"));
    }

    #[test]
    fn redis_key_format() {
        assert_eq!(
            prompt_cache_redis_key(
                "7f798905-540d-437e-a781-6c7a2e5fdb82",
                "test001212",
            ),
            "prompt:cache:7f798905-540d-437e-a781-6c7a2e5fdb82:test001212"
        );
    }

    #[test]
    fn has_nonempty_alephant_prompt_id_false_when_absent() {
        let h = http::HeaderMap::new();
        assert!(!has_nonempty_alephant_prompt_id(&h));
    }

    #[test]
    fn has_nonempty_alephant_prompt_id_false_when_empty_trim() {
        let mut h = http::HeaderMap::new();
        h.insert(
            ALEPHANT_PROMPT_ID.clone(),
            http::HeaderValue::from_static("   "),
        );
        assert!(!has_nonempty_alephant_prompt_id(&h));
    }

    #[test]
    fn has_nonempty_alephant_prompt_id_true_when_present() {
        let mut h = http::HeaderMap::new();
        h.insert(
            ALEPHANT_PROMPT_ID.clone(),
            http::HeaderValue::from_static("my-prompt"),
        );
        assert!(has_nonempty_alephant_prompt_id(&h));
    }

    #[test]
    fn current_version_trims_string() {
        let v: Value = serde_json::from_str(
            r#"{"template":{"current_version":"  v1  ","messages":[]}}"#,
        )
        .expect("json");
        assert_eq!(
            current_version_for_request_log_from_cache_root(&v).as_deref(),
            Some("v1")
        );
    }

    #[test]
    fn current_version_from_integer_json() {
        let v: Value = serde_json::from_str(
            r#"{"template":{"current_version":42,"messages":[]}}"#,
        )
        .expect("json");
        assert_eq!(
            current_version_for_request_log_from_cache_root(&v).as_deref(),
            Some("42")
        );
    }

    #[test]
    fn current_version_omits_null_object_and_array() {
        for json in [
            r#"{"template":{"current_version":null,"messages":[]}}"#,
            r#"{"template":{"current_version":{},"messages":[]}}"#,
            r#"{"template":{"current_version":[],"messages":[]}}"#,
        ] {
            let v: Value = serde_json::from_str(json).expect("json");
            assert!(
                current_version_for_request_log_from_cache_root(&v).is_none(),
                "{json}"
            );
        }
    }

    fn test_vk_metrics() -> VkMetrics {
        let meter = global::meter("prompt_cache_tests");
        VkMetrics::new(&meter)
    }

    #[test]
    fn invalid_cache_hit_should_fail_open_without_injection() {
        let vk_metrics = test_vk_metrics();
        let original_body = Bytes::from_static(
            br#"{"model":"openai/gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        );

        let got = merge_prompt_cache_hit_into_body(
            "prompt-1",
            "prompt:cache:ws:prompt-1",
            "{not-json",
            original_body.clone(),
            &vk_metrics,
        )
        .expect("invalid cache should fail-open");

        assert_eq!(got.0, original_body);
        assert!(got.1.is_none());
    }

    #[test]
    fn invalid_cache_shape_should_fail_open_without_injection() {
        let vk_metrics = test_vk_metrics();
        let original_body = Bytes::from_static(
            br#"{"model":"openai/gpt-4","messages":[{"role":"user","content":"hi"}]}"#,
        );

        let got = merge_prompt_cache_hit_into_body(
            "prompt-1",
            "prompt:cache:ws:prompt-1",
            r#"{"template":{"messages":"not-array"}}"#,
            original_body.clone(),
            &vk_metrics,
        )
        .expect("invalid cache shape should fail-open");

        assert_eq!(got.0, original_body);
        assert!(got.1.is_none());
    }

    #[tokio::test]
    async fn prompt_id_too_long_still_returns_prompt_cache_invalid_400() {
        let vk_metrics = test_vk_metrics();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            ALEPHANT_PROMPT_ID.clone(),
            HeaderValue::from_str(&"a".repeat(MAX_PROMPT_ID_BYTES + 1))
                .expect("header value"),
        );

        let err = merge_prompt_cache_messages_into_body(
            None,
            &headers,
            "workspace-1",
            Bytes::from_static(br#"{"messages":[]}"#),
            &vk_metrics,
        )
        .await
        .expect_err("too long prompt id should return error");

        let response = match err {
            ApiError::InvalidRequest(
                InvalidRequestError::PromptCacheInvalid { message },
            ) => {
                assert!(message.contains("exceeds maximum length"));
                InvalidRequestError::PromptCacheInvalid { message }
                    .into_response()
            }
            other => panic!("unexpected error variant: {other:?}"),
        };
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
