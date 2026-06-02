use std::{collections::HashMap, net::SocketAddr};

use bytes::Bytes;
use http::Extensions;

use crate::{
    policy_proto::EvaluateRequest,
    types::{
        extensions::{AuthContext, RequestContext},
        provider::InferenceProvider,
        router::RouterId,
    },
};

// `EvaluateRequest.body` is attached before evaluate when piicache Redis is
// true. Extension hooks live elsewhere.
#[allow(dead_code)]
#[must_use]
pub fn truncate_body_for_policy(body: &Bytes, max: usize) -> String {
    let cow = String::from_utf8_lossy(body.as_ref());
    let s = cow.as_ref();
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[must_use]
pub fn extract_model_hint(body: &Bytes) -> String {
    serde_json::from_slice::<serde_json::Value>(body.as_ref())
        .ok()
        .and_then(|v| {
            v.get("model")
                .and_then(|m| m.as_str())
                .map(std::string::ToString::to_string)
        })
        .unwrap_or_default()
}

/// Client IP for policy: leftmost `X-Forwarded-For` entry when present, else
/// the peer address from the server's `SocketAddr` extension.
#[must_use]
pub fn resolve_client_ip(headers: &http::HeaderMap, extensions: &Extensions) -> String {
    if let Some(raw) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok())
        && let Some(first) = raw.split(',').next()
    {
        let trimmed = first.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    extensions
        .get::<SocketAddr>()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_default()
}

#[must_use]
pub fn build_policy_headers_map(headers: &http::HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();

    for (name, value) in headers {
        let Ok(value) = value.to_str() else {
            continue;
        };

        out.entry(name.as_str().to_owned())
            .and_modify(|existing: &mut String| {
                existing.push(',');
                existing.push_str(value);
            })
            .or_insert_with(|| value.to_owned());
    }

    out
}

/// `department_id` comes from [`AuthContext`] (set at VK auth). When the
/// lookup yields none (represented as [`Uuid::nil()`] in context), policy
/// receives an empty string.
#[must_use]
pub fn build_evaluate_request(
    extensions: &Extensions,
    headers: &http::HeaderMap,
    auth: &AuthContext,
    body: &Bytes,
) -> Option<EvaluateRequest> {
    let vk_id = auth.virtual_key_id?;
    let request_id = headers
        .get("x-request-id")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut request_id = request_id;
    if request_id.is_empty()
        && let Some(rid) = extensions.get::<RouterId>()
    {
        request_id = rid.to_string();
    }
    let provider = extensions
        .get::<InferenceProvider>()
        .map(std::string::ToString::to_string)
        .unwrap_or_default();

    Some(EvaluateRequest {
        workspace_id: auth.org_id.to_string(),
        request_id,
        model: extract_model_hint(body),
        provider,
        virtual_key_id: vk_id.to_string(),
        client_ip: resolve_client_ip(headers, extensions),
        locale: String::new(),
        department_id: if auth.department_id.is_nil() {
            String::new()
        } else {
            auth.department_id.to_string()
        },
        body: Default::default(),
        estimated_input_tokens: 0,
        estimated_input_usd: 0.0,
        headers: build_policy_headers_map(headers),
    })
}

/// Returns `true` if this request should be checked (VK auth present in
/// context).
#[must_use]
pub fn vk_auth_present(extensions: &Extensions) -> bool {
    let Some(ctx) = extensions.get::<std::sync::Arc<RequestContext>>() else {
        return false;
    };
    let Some(auth) = ctx.auth_context.as_ref() else {
        return false;
    };
    auth.virtual_key_id.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_ascii() {
        let body = Bytes::from("hello world");
        assert_eq!(truncate_body_for_policy(&body, 5), "hello");
    }

    #[test]
    fn truncate_does_not_split_utf8_char() {
        let s = "aé"; // `é` is two bytes in UTF-8
        let body = Bytes::copy_from_slice(s.as_bytes());
        let out = truncate_body_for_policy(&body, 1);
        assert_eq!(out, "a");
    }

    #[test]
    fn extract_model_from_json() {
        let body = Bytes::from(r#"{"model":"gpt-4o","messages":[]}"#);
        assert_eq!(extract_model_hint(&body), "gpt-4o");
    }

    #[test]
    fn resolve_client_ip_prefers_x_forwarded_for_first_hop() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            http::HeaderValue::from_static("203.0.113.1, 10.0.0.1"),
        );
        let ext = Extensions::new();
        assert_eq!(resolve_client_ip(&headers, &ext), "203.0.113.1".to_string());
    }

    #[test]
    fn resolve_client_ip_falls_back_to_socket_addr() {
        let headers = http::HeaderMap::new();
        let mut ext = Extensions::new();
        ext.insert(SocketAddr::from(([192, 0, 2, 1], 443)));
        assert_eq!(resolve_client_ip(&headers, &ext), "192.0.2.1".to_string());
    }

    #[test]
    fn build_policy_headers_map_returns_empty_for_empty_headers() {
        let headers = http::HeaderMap::new();

        assert!(build_policy_headers_map(&headers).is_empty());
    }

    #[test]
    fn build_policy_headers_map_preserves_single_header_value() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-request-id", http::HeaderValue::from_static("req-123"));

        let out = build_policy_headers_map(&headers);

        assert_eq!(out.get("x-request-id").map(String::as_str), Some("req-123"));
    }

    #[test]
    fn build_policy_headers_map_joins_same_name_values_with_comma() {
        let mut headers = http::HeaderMap::new();
        headers.append("accept", http::HeaderValue::from_static("application/json"));
        headers.append(
            "accept",
            http::HeaderValue::from_static("text/event-stream"),
        );

        let out = build_policy_headers_map(&headers);

        assert_eq!(
            out.get("accept").map(String::as_str),
            Some("application/json,text/event-stream")
        );
    }

    #[test]
    fn build_policy_headers_map_skips_non_utf8_values() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-binary",
            http::HeaderValue::from_bytes(&[0xff]).expect("opaque bytes are valid header values"),
        );
        headers.insert("x-text", http::HeaderValue::from_static("ok"));

        let out = build_policy_headers_map(&headers);

        assert!(!out.contains_key("x-binary"));
        assert_eq!(out.get("x-text").map(String::as_str), Some("ok"));
    }
}
