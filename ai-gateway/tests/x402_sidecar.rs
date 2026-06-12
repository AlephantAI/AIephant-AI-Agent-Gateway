use std::{
    collections::HashMap,
    fmt::Debug,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ai_gateway::{
    app::App,
    config::Config,
    payment_proto::{
        GetPaymentRequirementsRequest, GetPaymentRequirementsResponse, RecordServiceResultRequest,
        RecordServiceResultResponse, SettlePaymentRequest, SettlePaymentResponse,
        VerifyAndSettlePaymentRequest, VerifyAndSettlePaymentResponse, VerifyPaymentRequest,
        VerifyPaymentResponse,
        x402_payment_service_server::{X402PaymentService, X402PaymentServiceServer},
    },
    policy_proto::{
        AgentPolicyScope, EvaluateRequest, EvaluateResponse, ValidateAgentPolicyRequest,
        ValidateAgentPolicyResponse, X402InboundDetail, X402InboundEvaluateRequest,
        X402InboundEvaluateResponse,
        policy_service_server::{PolicyService, PolicyServiceServer},
    },
    types::secret::Secret,
    x402::{forward_signature::endpoint_secret_redis_key, snapshot::endpoint_snapshot_redis_key},
};
use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt as _;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, oneshot},
};
use tonic::{Request as GrpcRequest, Response as GrpcResponse, Status};
use tower::{Service, ServiceExt as _};
use uuid::Uuid;

const ENDPOINT_ID: &str = "11111111-1111-1111-1111-111111111111";
const WORKSPACE_ID: &str = "22222222-2222-2222-2222-222222222222";

#[test]
fn x402_config_defaults_disabled() {
    assert!(!Config::default().x402.enabled);
}

#[tokio::test]
async fn x402_agent_post_without_authorization_builds_with_app() {
    let mut config = Config::default();
    config.compat_mode = true;
    #[cfg(feature = "external")]
    {
        use ai_gateway::{config::cloudflare_kv::CloudflareKvConfig, types::secret::Secret};

        config.cloudflare_kv = Some(CloudflareKvConfig {
            api_base: "https://api.cloudflare.com/client/v4".into(),
            account_id: "test".into(),
            namespace_id: "test".into(),
            api_token: Secret::from("test-token".to_string()),
        });
    }
    assert!(!config.x402.enabled);
    let mut app = App::new(config).await.expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/test-agent")
        .body(Body::empty())
        .expect("x402 agent request");

    assert!(request.headers().get(http::header::AUTHORIZATION).is_none());

    let response = app.ready().await.unwrap().call(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn x402_api_post_without_authorization_builds_with_app() {
    let mut config = Config::default();
    config.compat_mode = true;
    #[cfg(feature = "external")]
    {
        use ai_gateway::{config::cloudflare_kv::CloudflareKvConfig, types::secret::Secret};

        config.cloudflare_kv = Some(CloudflareKvConfig {
            api_base: "https://api.cloudflare.com/client/v4".into(),
            account_id: "test".into(),
            namespace_id: "test".into(),
            api_token: Secret::from("test-token".to_string()),
        });
    }
    assert!(!config.x402.enabled);
    let mut app = App::new(config).await.expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/test-api")
        .body(Body::empty())
        .expect("x402 api request");

    assert!(request.headers().get(http::header::AUTHORIZATION).is_none());

    let response = app.ready().await.unwrap().call(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn x402_request_with_invalid_body_schema_returns_400_before_policy() {
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let policy = spawn_policy_service(policy_calls.clone()).await;
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let upstream = spawn_upstream_server().await;
    let redis = spawn_redis_fixture(&[
        (
            endpoint_snapshot_redis_key("schema-agent", "POST"),
            endpoint_snapshot_json(
                "schema-agent",
                &upstream.base_url,
                wallet_schema(),
                serde_json::json!([]),
            ),
        ),
        (
            endpoint_secret_redis_key(Uuid::parse_str(ENDPOINT_ID).expect("endpoint id")),
            "test-endpoint-secret".to_string(),
        ),
    ])
    .await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/schema-agent")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"chain":"base"}"#))
        .expect("x402 request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    let status = response.status();
    let body = response_body_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "x402_body_schema_invalid");
    assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(payment_calls.load(Ordering::SeqCst), 0);
    assert_eq!(upstream.requests.lock().await.len(), 0);
}

#[tokio::test]
async fn paid_x402_request_forwards_allowlisted_headers_and_injects_signature() {
    let policy = spawn_policy_service(Arc::new(AtomicUsize::new(0))).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let upstream = spawn_upstream_server().await;
    let redis = spawn_redis_fixture(&[
        (
            endpoint_snapshot_redis_key("paid-agent", "POST"),
            endpoint_snapshot_json(
                "paid-agent",
                &upstream.base_url,
                serde_json::Value::Null,
                serde_json::json!([
                    {"name": "X-API-Key", "value": "ignored"},
                    {"name": "X-Payment-Service-Key", "value": "ignored"}
                ]),
            ),
        ),
        (
            endpoint_secret_redis_key(Uuid::parse_str(ENDPOINT_ID).expect("endpoint id")),
            "test-endpoint-secret".to_string(),
        ),
    ])
    .await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/paid-agent")
        .header("X-API-Key", "inbound-secret")
        .header("X-Drop-Me", "no")
        .header("PAYMENT-SIGNATURE", "paid-test-signature")
        .body(Body::from(r#"{"chain":"base"}"#))
        .expect("paid x402 request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let requests = upstream.requests.lock().await;
    assert_eq!(requests.len(), 1);
    let headers = &requests[0].headers;
    assert_eq!(
        headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("inbound-secret")
    );
    let signature = headers
        .get("x-alephant-signature")
        .and_then(|value| value.to_str().ok())
        .expect("signature header should be present");
    assert!(signature.starts_with("v2="));
    assert_eq!(signature.len(), 67);
    assert!(
        signature["v2=".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(headers.contains_key("x-alephant-timestamp"));
    assert_eq!(
        headers
            .get("x-alephant-endpoint-id")
            .and_then(|value| value.to_str().ok()),
        Some(ENDPOINT_ID)
    );
    assert!(!headers.contains_key("x-drop-me"));
    assert!(!headers.contains_key("payment-signature"));
    assert!(!headers.contains_key("x-payment-service-key"));
    assert_eq!(payment_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn x402_unified_route_accepts_http_api_endpoint_type_with_payment_required_header() {
    let policy = spawn_policy_service(Arc::new(AtomicUsize::new(0))).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let upstream = spawn_upstream_server().await;
    let redis = spawn_redis_fixture(&[
        (
            endpoint_snapshot_redis_key("api-endpoint", "POST"),
            endpoint_snapshot_json_with_endpoint_type(
                "api-endpoint",
                &upstream.base_url,
                serde_json::Value::Null,
                serde_json::json!([]),
                "http_api",
            ),
        ),
        (
            endpoint_secret_redis_key(Uuid::parse_str(ENDPOINT_ID).expect("endpoint id")),
            "test-endpoint-secret".to_string(),
        ),
    ])
    .await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/api-endpoint")
        .body(Body::from("{}"))
        .expect("x402 api request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    let status = response.status();
    let payment_required_header = response
        .headers()
        .get("payment-required")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response_body_json(response).await;

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(payment_required_header.as_deref(), Some("payment-required"));
    assert_eq!(body["error"]["code"], "x402_payment_required");
    let object = body.as_object().expect("x402 body should be a JSON object");
    assert_eq!(object.len(), 1);
    assert!(object.contains_key("error"));
    assert!(body.get("paymentRequirements").is_none());
    assert!(body.get("accepts").is_none());
    assert_eq!(payment_calls.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.requests.lock().await.len(), 0);
}

#[tokio::test]
async fn x402_unified_route_accepts_agent_endpoint_type() {
    let policy = spawn_policy_service(Arc::new(AtomicUsize::new(0))).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let upstream = spawn_upstream_server().await;
    let redis = spawn_redis_fixture(&[(
        endpoint_snapshot_redis_key("agent-endpoint", "POST"),
        endpoint_snapshot_json_with_endpoint_type(
            "agent-endpoint",
            &upstream.base_url,
            serde_json::Value::Null,
            serde_json::json!([]),
            "agent",
        ),
    )])
    .await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/agent-endpoint")
        .body(Body::from("{}"))
        .expect("x402 request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    let status = response.status();
    let body = response_body_json(response).await;

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["code"], "x402_payment_required");
    assert_eq!(payment_calls.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.requests.lock().await.len(), 0);
}

#[tokio::test]
async fn x402_unified_route_accepts_http_api_endpoint_type() {
    let policy = spawn_policy_service(Arc::new(AtomicUsize::new(0))).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let upstream = spawn_upstream_server().await;
    let redis = spawn_redis_fixture(&[(
        endpoint_snapshot_redis_key("api-only", "POST"),
        endpoint_snapshot_json_with_endpoint_type(
            "api-only",
            &upstream.base_url,
            serde_json::Value::Null,
            serde_json::json!([]),
            "http_api",
        ),
    )])
    .await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/api-only")
        .body(Body::from("{}"))
        .expect("x402 request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    let status = response.status();
    let body = response_body_json(response).await;

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["code"], "x402_payment_required");
    assert_eq!(payment_calls.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.requests.lock().await.len(), 0);
}

#[tokio::test]
async fn x402_unified_route_accepts_redis_snapshot_without_endpoint_type() {
    let policy = spawn_policy_service(Arc::new(AtomicUsize::new(0))).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let upstream = spawn_upstream_server().await;
    let redis = spawn_redis_fixture(&[(
        endpoint_snapshot_redis_key("legacy-agent", "POST"),
        endpoint_snapshot_json_without_endpoint_type(
            "legacy-agent",
            &upstream.base_url,
            serde_json::Value::Null,
            serde_json::json!([]),
        ),
    )])
    .await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/legacy-agent")
        .body(Body::from("{}"))
        .expect("x402 request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    let status = response.status();
    let body = response_body_json(response).await;

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["code"], "x402_payment_required");
    assert_eq!(payment_calls.load(Ordering::SeqCst), 1);
    assert_eq!(upstream.requests.lock().await.len(), 0);
}

#[tokio::test]
async fn x402_agents_old_route_returns_404() {
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let policy = spawn_policy_service(policy_calls.clone()).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let redis = spawn_redis_fixture(&[]).await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/agents/weather")
        .body(Body::from("{}"))
        .expect("old x402 agents request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(payment_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn x402_api_old_route_returns_404() {
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let policy = spawn_policy_service(policy_calls.clone()).await;
    let payment_calls = Arc::new(AtomicUsize::new(0));
    let payment = spawn_payment_service(payment_calls.clone()).await;
    let redis = spawn_redis_fixture(&[]).await;
    let mut app = App::new(x402_config(
        &policy.endpoint,
        &payment.endpoint,
        &redis.endpoint,
    ))
    .await
    .expect("app");

    let request = Request::builder()
        .method(Method::POST)
        .uri("/x402/api/weather")
        .body(Body::from("{}"))
        .expect("old x402 api request");

    let response = app.ready().await.unwrap().call(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(payment_calls.load(Ordering::SeqCst), 0);
}

fn x402_config(policy_endpoint: &str, payment_endpoint: &str, redis_endpoint: &str) -> Config {
    let mut config = Config::default();
    config.compat_mode = true;
    config.x402.enabled = true;
    config.x402.payment_grpc_endpoint = payment_endpoint.to_string();
    config.x402.payment_service_key = Secret::from("payment-service-secret".to_string());
    config.policy.grpc_endpoint = policy_endpoint.to_string();
    config.request_log.log_queue_redis_url = Some(redis_endpoint.parse().expect("redis url"));
    #[cfg(feature = "external")]
    {
        use ai_gateway::{config::cloudflare_kv::CloudflareKvConfig, types::secret::Secret};

        config.cloudflare_kv = Some(CloudflareKvConfig {
            api_base: "https://api.cloudflare.com/client/v4".into(),
            account_id: "test".into(),
            namespace_id: "test".into(),
            api_token: Secret::from("test-token".to_string()),
        });
    }
    config
}

async fn response_body_json<B>(response: http::Response<B>) -> serde_json::Value
where
    B: http_body::Body<Data = Bytes>,
    B::Error: Debug,
{
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("json error response")
}

fn wallet_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["wallet_address", "chain"],
        "properties": {
            "wallet_address": {"type": "string"},
            "chain": {
                "type": "string",
                "enum": ["base", "ethereum", "solana"]
            }
        }
    })
}

fn endpoint_snapshot_json(
    slug: &str,
    upstream_base_url: &str,
    body_schema: serde_json::Value,
    headers_policy: serde_json::Value,
) -> String {
    endpoint_snapshot_json_with_endpoint_type(
        slug,
        upstream_base_url,
        body_schema,
        headers_policy,
        "agent",
    )
}

fn endpoint_snapshot_json_with_endpoint_type(
    slug: &str,
    upstream_base_url: &str,
    body_schema: serde_json::Value,
    headers_policy: serde_json::Value,
    endpoint_type: &str,
) -> String {
    endpoint_snapshot_value(
        slug,
        upstream_base_url,
        body_schema,
        headers_policy,
        Some(endpoint_type),
    )
    .to_string()
}

fn endpoint_snapshot_json_without_endpoint_type(
    slug: &str,
    upstream_base_url: &str,
    body_schema: serde_json::Value,
    headers_policy: serde_json::Value,
) -> String {
    endpoint_snapshot_value(slug, upstream_base_url, body_schema, headers_policy, None).to_string()
}

fn endpoint_snapshot_value(
    slug: &str,
    upstream_base_url: &str,
    body_schema: serde_json::Value,
    headers_policy: serde_json::Value,
    endpoint_type: Option<&str>,
) -> serde_json::Value {
    let mut snapshot = serde_json::json!({
        "endpoint_id": ENDPOINT_ID,
        "workspace_id": WORKSPACE_ID,
        "status": "active",
        "name": "Test Agent",
        "slug": slug,
        "method": "POST",
        "path": format!("/{slug}"),
        "pricing_model": "per_call",
        "price_amount": "1.00000000",
        "asset": "USDC",
        "network": "base",
        "receive_wallet_address": "0xtesttesttesttesttesttesttest",
        "fee_bps": 0,
        "body_schema": body_schema,
        "target": {
            "kind": "http_proxy",
            "original_target_url": upstream_base_url,
            "forward_method": "preserve",
            "path_rewrite": {},
            "headers_policy": headers_policy,
            "origin_signature_required": true
        },
        "origin_auth": {"active_secret_version": 1},
        "policy": {
            "policy_id": "33333333-3333-3333-3333-333333333333",
            "buyer_access": "Public",
            "rate_limit_rpm": 100,
            "max_request_size": 1000000,
            "timeout_seconds": 5,
            "payment_retry_attempts": 1,
            "schema_validation_required": true,
            "facilitator": "coinbase",
            "cache_billing_mode": "disabled",
            "cache_hit_discount_bps": 0
        },
        "snapshot_revision": 1,
        "config_revision": 1,
        "compiled_at": "2026-05-22T00:00:00Z"
    });
    if let Some(endpoint_type) = endpoint_type {
        snapshot
            .as_object_mut()
            .expect("snapshot must be an object")
            .insert(
                "endpoint_type".to_string(),
                serde_json::json!(endpoint_type),
            );
    }
    snapshot
}

struct GrpcFixture {
    endpoint: String,
}

async fn spawn_policy_service(calls: Arc<AtomicUsize>) -> GrpcFixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind policy");
    let addr = listener.local_addr().expect("policy addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let service = TestPolicyService { calls };
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(PolicyServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .expect("policy server");
    });
    GrpcFixture {
        endpoint: format!("http://{addr}"),
    }
}

#[derive(Clone)]
struct TestPolicyService {
    calls: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl PolicyService for TestPolicyService {
    async fn evaluate(
        &self,
        _request: GrpcRequest<EvaluateRequest>,
    ) -> Result<GrpcResponse<EvaluateResponse>, Status> {
        Ok(GrpcResponse::new(EvaluateResponse {
            allowed: true,
            reason: "allowed".to_string(),
            ..Default::default()
        }))
    }

    async fn evaluate_x402_inbound(
        &self,
        _request: GrpcRequest<X402InboundEvaluateRequest>,
    ) -> Result<GrpcResponse<X402InboundEvaluateResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GrpcResponse::new(X402InboundEvaluateResponse {
            allowed: true,
            reason: "x402_endpoint_ok".to_string(),
            snapshot_revision: 1,
            detail: Some(X402InboundDetail {
                max_amount_usdc: 1.0,
                rate_limit_remaining: 99,
                fund_policy: "hold_on_service_failure".to_string(),
            }),
            ..Default::default()
        }))
    }

    async fn validate_agent_policy(
        &self,
        _request: GrpcRequest<ValidateAgentPolicyRequest>,
    ) -> Result<GrpcResponse<ValidateAgentPolicyResponse>, Status> {
        Ok(GrpcResponse::new(ValidateAgentPolicyResponse {
            allowed: true,
            reason: "agent_policy_allowed".to_string(),
            policy_scope: AgentPolicyScope::Agent as i32,
            ..Default::default()
        }))
    }
}

async fn spawn_payment_service(calls: Arc<AtomicUsize>) -> GrpcFixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind payment");
    let addr = listener.local_addr().expect("payment addr");
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let service = TestPaymentService { calls };
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(X402PaymentServiceServer::new(service))
            .serve_with_incoming(incoming)
            .await
            .expect("payment server");
    });
    GrpcFixture {
        endpoint: format!("http://{addr}"),
    }
}

#[derive(Clone)]
struct TestPaymentService {
    calls: Arc<AtomicUsize>,
}

#[tonic::async_trait]
impl X402PaymentService for TestPaymentService {
    async fn get_payment_requirements(
        &self,
        _request: GrpcRequest<GetPaymentRequirementsRequest>,
    ) -> Result<GrpcResponse<GetPaymentRequirementsResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GrpcResponse::new(GetPaymentRequirementsResponse {
            activity_id: "activity-1".to_string(),
            payment_required_header: "payment-required".to_string(),
            success: true,
            accepts: vec![ai_gateway::payment_proto::PaymentAcceptSummary {
                scheme: "exact".to_string(),
                network: "base".to_string(),
                asset: "USDC".to_string(),
                amount: "1.00000000".to_string(),
                pay_to: "0xtesttesttesttesttesttesttest".to_string(),
                resource: "https://target.test/resource".to_string(),
                facilitator: "coinbase".to_string(),
                accept_hash: "accept-hash".to_string(),
            }],
            ..Default::default()
        }))
    }

    async fn verify_payment(
        &self,
        _request: GrpcRequest<VerifyPaymentRequest>,
    ) -> Result<GrpcResponse<VerifyPaymentResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GrpcResponse::new(VerifyPaymentResponse {
            activity_id: "activity-1".to_string(),
            payment_status: "success".to_string(),
            buyer_wallet: "0xbuyer".to_string(),
            success: true,
            ..Default::default()
        }))
    }

    async fn verify_and_settle_payment(
        &self,
        _request: GrpcRequest<VerifyAndSettlePaymentRequest>,
    ) -> Result<GrpcResponse<VerifyAndSettlePaymentResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GrpcResponse::new(VerifyAndSettlePaymentResponse {
            activity_id: "activity-1".to_string(),
            payment_status: "success".to_string(),
            settlement_status: "success".to_string(),
            buyer_wallet: "0xbuyer".to_string(),
            payment_response_header: "payment-response".to_string(),
            success: true,
            ..Default::default()
        }))
    }

    async fn settle_payment(
        &self,
        _request: GrpcRequest<SettlePaymentRequest>,
    ) -> Result<GrpcResponse<SettlePaymentResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GrpcResponse::new(SettlePaymentResponse {
            activity_id: "activity-1".to_string(),
            settlement_status: "success".to_string(),
            payment_response_header: "payment-response".to_string(),
            success: true,
            ..Default::default()
        }))
    }

    async fn record_service_result(
        &self,
        _request: GrpcRequest<RecordServiceResultRequest>,
    ) -> Result<GrpcResponse<RecordServiceResultResponse>, Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(GrpcResponse::new(RecordServiceResultResponse {
            activity_id: "activity-1".to_string(),
            service_status: "succeeded".to_string(),
            event_enqueued: true,
            success: true,
            ..Default::default()
        }))
    }

    async fn authorize_outbound_spend(
        &self,
        _request: GrpcRequest<ai_gateway::payment_proto::AuthorizeOutboundSpendRequest>,
    ) -> Result<GrpcResponse<ai_gateway::payment_proto::AuthorizeOutboundSpendResponse>, Status>
    {
        unimplemented!("not used by x402 inbound tests")
    }

    async fn record_outbound_result(
        &self,
        _request: GrpcRequest<ai_gateway::payment_proto::RecordOutboundResultRequest>,
    ) -> Result<GrpcResponse<ai_gateway::payment_proto::RecordOutboundResultResponse>, Status> {
        unimplemented!("not used by x402 inbound tests")
    }
}

#[derive(Clone)]
struct UpstreamRequest {
    headers: HeaderMap,
}

struct UpstreamFixture {
    base_url: String,
    requests: Arc<Mutex<Vec<UpstreamRequest>>>,
}

async fn spawn_upstream_server() -> UpstreamFixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_task = requests.clone();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let requests = requests_for_task.clone();
            tokio::spawn(async move {
                let (headers, body_start, buffer) = read_http_request(&mut stream).await;
                let content_length = header_value(&buffer, "content-length")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or_default();
                let already_read_body = buffer.len().saturating_sub(body_start);
                if content_length > already_read_body {
                    let mut remaining = vec![0; content_length - already_read_body];
                    stream.read_exact(&mut remaining).await.expect("read body");
                }
                requests.lock().await.push(UpstreamRequest { headers });
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .await
                    .expect("write upstream response");
            });
        }
    });
    UpstreamFixture {
        base_url: format!("http://{addr}"),
        requests,
    }
}

struct RedisFixture {
    endpoint: String,
    _shutdown: oneshot::Sender<()>,
}

async fn spawn_redis_fixture(values: &[(String, String)]) -> RedisFixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind redis");
    let addr = listener.local_addr().expect("redis addr");
    let values: Arc<HashMap<String, String>> = Arc::new(values.iter().cloned().collect());
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted.expect("redis accept");
                    let values = values.clone();
                    tokio::spawn(handle_redis_connection(stream, values));
                }
            }
        }
    });
    RedisFixture {
        endpoint: format!("redis://{addr}"),
        _shutdown: shutdown_tx,
    }
}

async fn handle_redis_connection(mut stream: TcpStream, values: Arc<HashMap<String, String>>) {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0_u8; 1024];
        let read = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);

        while let Some((command, consumed)) = parse_resp_command(&buffer) {
            buffer.drain(..consumed);
            let response = redis_response(&command, &values);
            if stream.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }
}

fn redis_response(command: &[String], values: &HashMap<String, String>) -> String {
    match command
        .first()
        .map(|command| command.to_ascii_uppercase())
        .as_deref()
    {
        Some("GET") => command
            .get(1)
            .and_then(|key| values.get(key))
            .map_or_else(|| "$-1\r\n".to_string(), bulk_string),
        Some("PING") => "+PONG\r\n".to_string(),
        Some("XADD") => "$3\r\n0-1\r\n".to_string(),
        Some("CLIENT" | "HELLO" | "SET" | "EXPIRE") => "+OK\r\n".to_string(),
        _ => "+OK\r\n".to_string(),
    }
}

fn bulk_string(value: &String) -> String {
    format!("${}\r\n{}\r\n", value.len(), value)
}

fn parse_resp_command(buffer: &[u8]) -> Option<(Vec<String>, usize)> {
    if buffer.first().copied()? != b'*' {
        return None;
    }
    let (count_line, mut index) = read_line(buffer, 1)?;
    let count = std::str::from_utf8(count_line)
        .ok()?
        .parse::<usize>()
        .ok()?;
    let mut parts = Vec::with_capacity(count);

    for _ in 0..count {
        if buffer.get(index).copied()? != b'$' {
            return None;
        }
        let (len_line, next_index) = read_line(buffer, index + 1)?;
        index = next_index;
        let len = std::str::from_utf8(len_line).ok()?.parse::<usize>().ok()?;
        if buffer.len() < index + len + 2 {
            return None;
        }
        let part = std::str::from_utf8(&buffer[index..index + len])
            .ok()?
            .to_string();
        parts.push(part);
        index += len;
        if buffer.get(index..index + 2)? != b"\r\n" {
            return None;
        }
        index += 2;
    }

    Some((parts, index))
}

fn read_line(buffer: &[u8], start: usize) -> Option<(&[u8], usize)> {
    let end = buffer[start..]
        .windows(2)
        .position(|window| window == b"\r\n")?
        + start;
    Some((&buffer[start..end], end + 2))
}

async fn read_http_request(stream: &mut TcpStream) -> (HeaderMap, usize, Vec<u8>) {
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await.expect("read request");
        assert_ne!(read, 0, "upstream connection closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut headers = HeaderMap::new();
    for line in header_text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(
            http::HeaderName::from_bytes(name.trim().as_bytes()).expect("header name"),
            http::HeaderValue::from_str(value.trim()).expect("header value"),
        );
    }
    (headers, header_end, buffer)
}

fn header_value(buffer: &[u8], name: &str) -> Option<String> {
    let header_text = String::from_utf8_lossy(buffer);
    for line in header_text.lines() {
        let Some((line_name, value)) = line.split_once(':') else {
            continue;
        };
        if line_name.eq_ignore_ascii_case(name) {
            return Some(value.trim().to_string());
        }
    }
    None
}
