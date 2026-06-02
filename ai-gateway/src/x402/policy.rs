use std::collections::HashMap;

use bytes::Bytes;
use http::HeaderMap;

use crate::{
    app_state::AppState,
    config::policy::{OnUnavailable, POLICY_GRPC_EVALUATE_TIMEOUT},
    error::{api::ApiError, internal::InternalError},
    policy_proto::{X402InboundEvaluateRequest, X402InboundEvaluateResponse},
    x402::types::X402EndpointSnapshot,
};

#[must_use]
pub fn headers_for_policy(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let value = value.to_str().ok()?;
            Some((name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

#[must_use]
pub fn body_for_policy(body: &Bytes, max_bytes: usize) -> String {
    let end = body.len().min(max_bytes);
    String::from_utf8_lossy(&body[..end]).into_owned()
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_policy_request(
    snapshot: &X402EndpointSnapshot,
    headers: &HeaderMap,
    body: &Bytes,
    max_body_bytes: usize,
    trace_id: &str,
    request_id: &str,
    session_id: &str,
) -> X402InboundEvaluateRequest {
    X402InboundEvaluateRequest {
        workspace_id: snapshot.workspace_id.to_string(),
        endpoint_id: snapshot.endpoint_id.to_string(),
        req_tokens: String::new(),
        req_body: body_for_policy(body, max_body_bytes),
        headers: headers_for_policy(headers),
        trace_id: trace_id.to_string(),
        request_id: request_id.to_string(),
        session_id: session_id.to_string(),
        req_estimated_usd: String::new(),
        buyer_wallet: String::new(),
        amount_usdc: snapshot.price_amount.clone(),
        network: snapshot.network.clone(),
        source: "direct".to_string(),
    }
}

fn unavailable_response() -> X402InboundEvaluateResponse {
    X402InboundEvaluateResponse {
        allowed: true,
        reason: "policy_unavailable_allowed".to_string(),
        blocked_by: String::new(),
        route_hint: String::new(),
        snapshot_revision: 0,
        detail: None,
    }
}

fn unavailable_result(
    on_unavailable: OnUnavailable,
    message: String,
) -> Result<X402InboundEvaluateResponse, ApiError> {
    match on_unavailable {
        OnUnavailable::Allow => {
            tracing::warn!(%message, "x402 policy unavailable; allowing per policy");
            Ok(unavailable_response())
        }
        OnUnavailable::Deny => Err(ApiError::Internal(InternalError::ContentFilterUnavailable(
            message,
        ))),
    }
}

pub async fn evaluate_x402_inbound(
    app_state: &AppState,
    req: X402InboundEvaluateRequest,
) -> Result<X402InboundEvaluateResponse, ApiError> {
    let cfg = &app_state.config().policy;
    let Some(client) = app_state.content_filter_client().await else {
        return unavailable_result(
            cfg.on_unavailable,
            "x402 policy client not initialised".to_string(),
        );
    };

    let mut inner = client.inner();
    let call = inner.evaluate_x402_inbound(req);
    let result = tokio::time::timeout(POLICY_GRPC_EVALUATE_TIMEOUT, call).await;

    match result {
        Ok(Ok(resp)) => Ok(resp.into_inner()),
        Ok(Err(status)) => unavailable_result(cfg.on_unavailable, status.to_string()),
        Err(_elapsed) => unavailable_result(
            cfg.on_unavailable,
            "x402 policy evaluate timed out".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::{body_for_policy, build_policy_request};
    use crate::x402::types::{
        X402EndpointSnapshot, X402OriginAuthSnapshot, X402PolicySnapshot, X402TargetSnapshot,
    };

    fn test_snapshot() -> X402EndpointSnapshot {
        X402EndpointSnapshot {
            endpoint_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            workspace_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            agent_id: None,
            status: "active".to_string(),
            name: "Weather API".to_string(),
            slug: "weather".to_string(),
            endpoint_type: Some("agent".to_string()),
            method: "POST".to_string(),
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
                original_target_url: "https://example.com/weather".to_string(),
                forward_method: "POST".to_string(),
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

    #[test]
    fn build_policy_request_fills_snapshot_fields_headers_body_amount_and_network() {
        let snapshot = test_snapshot();
        let mut headers = HeaderMap::new();
        headers.insert("X-Buyer-Wallet", HeaderValue::from_static("0xbuyer"));
        headers.insert("X-Trace", HeaderValue::from_static("abc"));
        headers.insert(
            "X-Invalid",
            HeaderValue::from_bytes(b"\xffnot-utf8").unwrap(),
        );
        let body = Bytes::from_static(b"{\"city\":\"shanghai\"}");

        let req = build_policy_request(
            &snapshot,
            &headers,
            &body,
            10,
            "trace-1",
            "request-1",
            "session-1",
        );

        assert_eq!(req.workspace_id, snapshot.workspace_id.to_string());
        assert_eq!(req.endpoint_id, snapshot.endpoint_id.to_string());
        assert_eq!(req.req_body, "{\"city\":\"s");
        assert_eq!(req.trace_id, "trace-1");
        assert_eq!(req.request_id, "request-1");
        assert_eq!(req.session_id, "session-1");
        assert_eq!(req.amount_usdc, "0.25");
        assert_eq!(req.network, "eip155:8453");
        assert_eq!(req.source, "direct");
        assert_eq!(req.req_tokens, "");
        assert_eq!(req.req_estimated_usd, "");
        assert_eq!(req.buyer_wallet, "");
        assert_eq!(
            req.headers.get("x-buyer-wallet").map(String::as_str),
            Some("0xbuyer")
        );
        assert_eq!(req.headers.get("x-trace").map(String::as_str), Some("abc"));
        assert!(!req.headers.contains_key("x-invalid"));
    }

    #[test]
    fn body_for_policy_truncates_by_bytes_and_uses_lossy_utf8() {
        let body = Bytes::from_static("aé中".as_bytes());

        assert_eq!(body_for_policy(&body, 2), "a�");
        assert_eq!(body_for_policy(&body, 0), "");
    }
}
