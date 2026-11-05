use http::StatusCode;
use serde_json::{Value, json};

pub(crate) const MAX_PROBE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BusinessFailureKind {
    InvalidRequest,
    ContentPolicy,
    RateLimited,
    ModelUnavailable,
    Service,
}

#[derive(Debug, Clone)]
pub(crate) struct BusinessFailure {
    pub kind: BusinessFailureKind,
    pub code: String,
}

impl BusinessFailure {
    pub fn status(&self) -> StatusCode {
        match self.kind {
            BusinessFailureKind::InvalidRequest | BusinessFailureKind::ContentPolicy => {
                StatusCode::BAD_REQUEST
            }
            BusinessFailureKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            BusinessFailureKind::ModelUnavailable => StatusCode::NOT_FOUND,
            BusinessFailureKind::Service => StatusCode::BAD_GATEWAY,
        }
    }

    pub fn public_body(&self) -> Value {
        let message = match self.kind {
            BusinessFailureKind::InvalidRequest => {
                "The upstream service rejected the request parameters."
            }
            BusinessFailureKind::ContentPolicy => {
                "The upstream service rejected the request under its content policy."
            }
            BusinessFailureKind::RateLimited => {
                "The upstream service is rate limited. Retry later."
            }
            BusinessFailureKind::ModelUnavailable => "The requested upstream model is unavailable.",
            BusinessFailureKind::Service => "The upstream service reported a business failure.",
        };
        json!({"error": {"code": self.code, "type": "upstream_error", "message": message}})
    }
}

pub(crate) fn classify_business_failure(body: &[u8]) -> Option<BusinessFailure> {
    if body.len() > MAX_PROBE_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let object = value.as_object()?;
    let error = ["/error", "/data/error", "/result/error"]
        .iter()
        .filter_map(|path| value.pointer(path))
        .find(|value| match value {
            Value::Null | Value::Bool(false) => false,
            Value::String(text) => !text.trim().is_empty(),
            Value::Object(fields) => !fields.is_empty(),
            Value::Array(items) => !items.is_empty(),
            Value::Number(number) => number.as_f64() != Some(0.0),
            _ => true,
        });
    let status_failed = object
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "error" | "failed" | "failure" | "rejected"
            )
        });
    if error.is_none() && object.get("success") != Some(&Value::Bool(false)) && !status_failed {
        return None;
    }
    let code = error
        .and_then(|e| e.get("code"))
        .or_else(|| object.get("code"));
    let signal = [
        code.and_then(Value::as_str),
        error.and_then(|e| e.get("type")).and_then(Value::as_str),
        error.and_then(|e| e.get("message")).and_then(Value::as_str),
        error.and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_ascii_lowercase();
    let kind = if [
        "rate_limit",
        "rate limit",
        "too many requests",
        "quota_exceeded",
    ]
    .iter()
    .any(|s| signal.contains(s))
    {
        BusinessFailureKind::RateLimited
    } else if ["content_policy", "content_filter", "safety"]
        .iter()
        .any(|s| signal.contains(s))
    {
        BusinessFailureKind::ContentPolicy
    } else if [
        "invalid_request",
        "invalid_argument",
        "bad_request",
        "invalid parameter",
    ]
    .iter()
    .any(|s| signal.contains(s))
    {
        BusinessFailureKind::InvalidRequest
    } else if [
        "model_not_found",
        "model_not_available",
        "model_unavailable",
    ]
    .iter()
    .any(|s| signal.contains(s))
    {
        BusinessFailureKind::ModelUnavailable
    } else {
        BusinessFailureKind::Service
    };
    let safe_code = code
        .and_then(Value::as_str)
        .filter(|text| {
            !text.is_empty()
                && text.len() <= 128
                && text
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        })
        .unwrap_or("upstream_business_error");
    Some(BusinessFailure {
        kind,
        code: safe_code.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_failures_keep_safe_codes_and_do_not_echo_provider_values() {
        for (code, status) in [
            ("invalid_request", 400),
            ("content_policy_violation", 400),
            ("rate_limit_exceeded", 429),
            ("model_not_found", 404),
            ("internal_failure", 502),
        ] {
            let failure = classify_business_failure(
                &serde_json::to_vec(&json!({
                    "success":false,"error":{"code":code,"message":"private-value"}
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(failure.status().as_u16(), status);
            assert_eq!(failure.public_body()["error"]["code"], code);
            assert!(!failure.public_body().to_string().contains("private-value"));
        }
    }

    #[test]
    fn normal_fields_invalid_json_and_large_bodies_are_not_false_failures() {
        for body in [
            br#"{"success":true,"error":null}"#.as_slice(),
            br#"{"error":false}"#,
            br#"{"error":0}"#,
            br#"{"error":0.0}"#,
            br#"{"error":""}"#,
            br#"{"error":{}}"#,
            br#"{"status":"completed","code":200}"#,
            b"not-json",
        ] {
            assert!(classify_business_failure(body).is_none());
        }
        assert!(classify_business_failure(&vec![b' '; MAX_PROBE_BYTES + 1]).is_none());
    }

    #[test]
    fn nested_errors_and_explicit_failure_flags_are_recognized() {
        for body in [
            br#"{"data":{"error":"failed"}}"#.as_slice(),
            br#"{"result":{"error":{"code":"failed"}}}"#,
            br#"{"success":false}"#,
            br#"{"status":"FAILED"}"#,
        ] {
            assert!(classify_business_failure(body).is_some());
        }
        let failure =
            classify_business_failure(br#"{"error":{"code":"https://secret.invalid/key"}}"#)
                .unwrap();
        assert_eq!(failure.code, "upstream_business_error");
    }
}
