use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, Method, StatusCode};
use sha2::{Digest, Sha256};

use crate::x402::types::{X402EndpointSnapshot, X402TargetHeaderPolicyItem};

pub const PAYMENT_SIGNATURE_HEADER: &str = "payment-signature";
pub const PAYMENT_SERVICE_KEY_HEADER: &str = "x-payment-service-key";

pub fn build_upstream_url(
    original_target_url: &str,
    endpoint_path: &str,
    remaining_path: &str,
    query: Option<&str>,
) -> Result<reqwest::Url, url::ParseError> {
    let base = original_target_url.trim_end_matches('/');
    let endpoint_path = endpoint_path.trim_matches('/');
    let remaining_path = remaining_path.trim_start_matches('/');

    let mut upstream = reqwest::Url::parse(base)?;
    if upstream.query().is_some() || upstream.fragment().is_some() {
        return Err(url::ParseError::RelativeUrlWithoutBase);
    }

    {
        let mut segments = upstream
            .path_segments_mut()
            .map_err(|()| url::ParseError::RelativeUrlWithoutBase)?;
        segments.pop_if_empty();

        push_safe_path_segments(&mut segments, endpoint_path)?;
        push_safe_path_segments(&mut segments, remaining_path)?;
    }

    if let Some(query) = query.filter(|query| !query.is_empty()) {
        upstream.set_query(Some(query));
    }

    Ok(upstream)
}

fn push_safe_path_segments(
    segments: &mut url::PathSegmentsMut<'_>,
    path: &str,
) -> Result<(), url::ParseError> {
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        if is_dangerous_path_segment(segment) {
            return Err(url::ParseError::RelativeUrlWithoutBase);
        }
        segments.push(segment);
    }
    Ok(())
}

fn is_dangerous_path_segment(segment: &str) -> bool {
    let decoded = percent_decode_ascii(segment);
    matches!(decoded.as_deref(), Some("." | ".."))
}

fn percent_decode_ascii(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
            continue;
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub fn method_for_upstream(
    inbound: &Method,
    snapshot: &X402EndpointSnapshot,
) -> Result<Method, String> {
    if snapshot.target.forward_method == "preserve" {
        return Ok(inbound.clone());
    }

    snapshot
        .method
        .parse::<Method>()
        .map_err(|err| err.to_string())
}

#[must_use]
pub fn filtered_upstream_headers(
    inbound: &HeaderMap,
    policy: &[X402TargetHeaderPolicyItem],
) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    let connection_tokens = connection_header_tokens(inbound);
    let allowed_headers = allowed_header_names(policy);

    for (name, value) in inbound {
        if should_strip_header(name, &allowed_headers, &connection_tokens) {
            continue;
        }
        filtered.append(name, value.clone());
    }

    filtered
}

fn allowed_header_names(policy: &[X402TargetHeaderPolicyItem]) -> HashSet<HeaderName> {
    policy
        .iter()
        .filter_map(|item| HeaderName::from_bytes(item.name.as_bytes()).ok())
        .collect()
}

fn connection_header_tokens(inbound: &HeaderMap) -> HashSet<HeaderName> {
    inbound
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect()
}

fn should_strip_header(
    name: &HeaderName,
    allowed_headers: &HashSet<HeaderName>,
    connection_tokens: &HashSet<HeaderName>,
) -> bool {
    let name_str = name.as_str();
    if !allowed_headers.contains(name) {
        return true;
    }

    if name_str.eq_ignore_ascii_case(PAYMENT_SIGNATURE_HEADER) {
        return true;
    }

    if name_str.eq_ignore_ascii_case(PAYMENT_SERVICE_KEY_HEADER) {
        return true;
    }

    if is_hop_by_hop_or_framing_header(name_str) || connection_tokens.contains(name) {
        return true;
    }

    name_str.eq_ignore_ascii_case("payment-required")
        || name_str.eq_ignore_ascii_case("payment-response")
        || name_str
            .get(.."payment-".len().min(name_str.len()))
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("payment-"))
}

fn is_hop_by_hop_or_framing_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("upgrade")
        || name.eq_ignore_ascii_case("te")
        || name.eq_ignore_ascii_case("trailer")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("proxy-authenticate")
}

pub struct UpstreamProxyResult {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub latency_ms: u128,
    pub response_hash: String,
}

#[must_use]
pub fn hash_body(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}

pub async fn proxy_paid_request(
    client: &reqwest::Client,
    method: Method,
    url: reqwest::Url,
    headers: HeaderMap,
    body: Bytes,
    timeout: Duration,
) -> Result<UpstreamProxyResult, reqwest::Error> {
    let started_at = Instant::now();
    let response = client
        .request(method, url)
        .headers(headers)
        .body(body)
        .timeout(timeout)
        .send()
        .await?;

    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await?;
    let latency_ms = started_at.elapsed().as_millis();
    let response_hash = hash_body(&body);

    Ok(UpstreamProxyResult {
        status,
        headers,
        body,
        latency_ms,
        response_hash,
    })
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue, Method, header};
    use uuid::Uuid;

    use super::{
        PAYMENT_SERVICE_KEY_HEADER, PAYMENT_SIGNATURE_HEADER, build_upstream_url,
        filtered_upstream_headers, hash_body, method_for_upstream,
    };
    use crate::x402::types::{
        X402EndpointSnapshot, X402OriginAuthSnapshot, X402PolicySnapshot,
        X402TargetHeaderPolicyItem, X402TargetSnapshot,
    };

    fn test_snapshot(method: &str, forward_method: &str) -> X402EndpointSnapshot {
        X402EndpointSnapshot {
            endpoint_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            workspace_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            agent_id: None,
            status: "active".to_string(),
            name: "Weather API".to_string(),
            slug: "weather".to_string(),
            endpoint_type: Some("agent".to_string()),
            method: method.to_string(),
            path: "/weather".to_string(),
            pricing_model: "fixed".to_string(),
            price_amount: "0.25".to_string(),
            asset: "USDC".to_string(),
            network: "eip155:8453".to_string(),
            receive_wallet_address: "0xabc".to_string(),
            fee_bps: 100,
            body_schema: serde_json::Value::Null,
            target: X402TargetSnapshot {
                kind: "http".to_string(),
                original_target_url: "https://example.com".to_string(),
                forward_method: forward_method.to_string(),
                path_rewrite: serde_json::json!({}),
                headers_policy: vec![],
                origin_signature_required: false,
            },
            origin_auth: X402OriginAuthSnapshot {
                active_secret_version: 1,
            },
            policy: X402PolicySnapshot {
                policy_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                buyer_access: "public".to_string(),
                rate_limit_rpm: 60,
                max_request_size: 1024,
                timeout_seconds: 30,
                payment_retry_attempts: 1,
                schema_validation_required: false,
                facilitator: None,
                cache_billing_mode: "full".to_string(),
                cache_hit_discount_bps: 0,
            },
            snapshot_revision: 42,
            config_revision: Some(42),
            compiled_at: None,
        }
    }

    fn header_policy(names: &[&str]) -> Vec<X402TargetHeaderPolicyItem> {
        names
            .iter()
            .map(|name| X402TargetHeaderPolicyItem {
                name: (*name).to_string(),
                value: Some("ignored".to_string()),
            })
            .collect()
    }

    #[test]
    fn upstream_url_keeps_endpoint_path_remaining_path_and_query() {
        let url = build_upstream_url(
            "https://target.test/",
            "/service/test-agent",
            "/foo",
            Some("a=1"),
        )
        .unwrap();

        assert_eq!(
            url.as_str(),
            "https://target.test/service/test-agent/foo?a=1"
        );
    }

    #[test]
    fn upstream_url_avoids_duplicate_slashes() {
        let url =
            build_upstream_url("https://target.test///", "/test-agent/", "foo/bar", None).unwrap();

        assert_eq!(url.as_str(), "https://target.test/test-agent/foo/bar");
    }

    #[test]
    fn upstream_url_rejects_remaining_path_dot_segments() {
        assert!(
            build_upstream_url("https://target.test", "test-agent", "../admin", None,).is_err()
        );
        assert!(
            build_upstream_url("https://target.test", "test-agent", "%2e%2e/admin", None,).is_err()
        );
    }

    #[test]
    fn upstream_url_rejects_base_url_query_and_fragment() {
        assert!(
            build_upstream_url(
                "https://target.test/base?token=secret",
                "test-agent",
                "foo",
                Some("a=1"),
            )
            .is_err()
        );
        assert!(
            build_upstream_url("https://target.test/base#frag", "test-agent", "foo", None,)
                .is_err()
        );
    }

    #[test]
    fn header_filter_keeps_only_policy_allowlist_headers() {
        let mut inbound = HeaderMap::new();
        inbound.insert("x-keep", HeaderValue::from_static("keep"));
        inbound.insert("x-drop", HeaderValue::from_static("drop"));
        inbound.insert("authorization", HeaderValue::from_static("bearer"));

        let filtered = filtered_upstream_headers(&inbound, &header_policy(&["x-keep"]));

        assert_eq!(filtered.get("x-keep").unwrap(), "keep");
        assert!(!filtered.contains_key("x-drop"));
        assert!(!filtered.contains_key("authorization"));
        assert!(!filtered.contains_key("x-alephant-trace-id"));
        assert!(!filtered.contains_key("x-alephant-request-id"));
    }

    #[test]
    fn header_filter_keeps_allowlist_case_insensitively() {
        let mut inbound = HeaderMap::new();
        inbound.insert("x-request-context", HeaderValue::from_static("ctx"));
        inbound.insert("x-drop", HeaderValue::from_static("drop"));

        let filtered = filtered_upstream_headers(&inbound, &header_policy(&["X-REQUEST-CONTEXT"]));

        assert_eq!(filtered.get("x-request-context").unwrap(), "ctx");
        assert!(!filtered.contains_key("x-drop"));
    }

    #[test]
    fn header_filter_strips_hop_by_hop_even_when_allowlisted() {
        let mut inbound = HeaderMap::new();
        inbound.insert(header::HOST, HeaderValue::from_static("gateway.test"));
        inbound.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-secret"),
        );
        inbound.insert(header::CONTENT_LENGTH, HeaderValue::from_static("42"));
        inbound.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        inbound.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        inbound.insert(header::TE, HeaderValue::from_static("trailers"));
        inbound.insert(header::TRAILER, HeaderValue::from_static("expires"));
        inbound.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        inbound.insert(
            header::PROXY_AUTHORIZATION,
            HeaderValue::from_static("basic abc"),
        );
        inbound.insert(
            header::PROXY_AUTHENTICATE,
            HeaderValue::from_static("basic"),
        );
        inbound.insert("x-secret", HeaderValue::from_static("strip"));
        inbound.insert("x-keep", HeaderValue::from_static("keep"));
        inbound.insert(PAYMENT_SIGNATURE_HEADER, HeaderValue::from_static("sig"));
        inbound.insert(
            PAYMENT_SERVICE_KEY_HEADER,
            HeaderValue::from_static("service-key"),
        );
        inbound.insert("payment-required", HeaderValue::from_static("yes"));
        inbound.insert("payment-custom", HeaderValue::from_static("custom"));

        let filtered = filtered_upstream_headers(
            &inbound,
            &header_policy(&[
                "host",
                "connection",
                "content-length",
                "transfer-encoding",
                "upgrade",
                "te",
                "trailer",
                "keep-alive",
                "proxy-authorization",
                "proxy-authenticate",
                "x-secret",
                "x-keep",
                PAYMENT_SIGNATURE_HEADER,
                PAYMENT_SERVICE_KEY_HEADER,
                "payment-required",
                "payment-custom",
            ]),
        );

        assert!(!filtered.contains_key(header::HOST));
        assert!(!filtered.contains_key(header::CONNECTION));
        assert!(!filtered.contains_key(header::CONTENT_LENGTH));
        assert!(!filtered.contains_key(header::TRANSFER_ENCODING));
        assert!(!filtered.contains_key(header::UPGRADE));
        assert!(!filtered.contains_key(header::TE));
        assert!(!filtered.contains_key(header::TRAILER));
        assert!(!filtered.contains_key("keep-alive"));
        assert!(!filtered.contains_key(header::PROXY_AUTHORIZATION));
        assert!(!filtered.contains_key(header::PROXY_AUTHENTICATE));
        assert!(!filtered.contains_key("x-secret"));
        assert!(!filtered.contains_key(PAYMENT_SIGNATURE_HEADER));
        assert!(!filtered.contains_key(PAYMENT_SERVICE_KEY_HEADER));
        assert!(!filtered.contains_key("payment-required"));
        assert!(!filtered.contains_key("payment-custom"));
        assert_eq!(filtered.get("x-keep").unwrap(), "keep");
    }

    #[test]
    fn empty_header_policy_forwards_no_inbound_headers() {
        let mut inbound = HeaderMap::new();
        inbound.insert("x-keep", HeaderValue::from_static("keep"));
        inbound.insert("authorization", HeaderValue::from_static("bearer"));

        let filtered = filtered_upstream_headers(&inbound, &[]);

        assert!(filtered.is_empty());
    }

    #[test]
    fn hash_body_is_stable_and_changes_with_body() {
        let first = hash_body(b"hello");
        let second = hash_body(b"hello");
        let changed = hash_body(b"world");

        assert_eq!(first, second);
        assert_ne!(first, changed);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn method_for_upstream_preserves_or_uses_snapshot_method() {
        let preserve = test_snapshot("POST", "preserve");
        let override_method = test_snapshot("PATCH", "POST");

        assert_eq!(
            method_for_upstream(&Method::GET, &preserve).unwrap(),
            Method::GET
        );
        assert_eq!(
            method_for_upstream(&Method::GET, &override_method).unwrap(),
            Method::PATCH
        );
    }

    #[test]
    fn proxy_result_body_hash_matches_hash_body_contract() {
        let body = Bytes::from_static(b"proxy-body");

        assert_eq!(hash_body(&body), hash_body(b"proxy-body"));
    }
}
