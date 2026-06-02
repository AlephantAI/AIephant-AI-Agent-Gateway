#![allow(clippy::large_futures)]

use std::collections::HashMap;

use ai_gateway::{
    config::{
        Config,
        alephant::AlephantFeatures,
        client_ip_rate_limit::{ClientIpRateLimitBackend, ClientIpRateLimitConfig},
    },
    tests::{TestDefault, harness::Harness, mock::MockArgs},
    types::request::Request,
};
use http::{Method, StatusCode, header};
use tower::Service;

async fn harness_with_iprl(cfg: ClientIpRateLimitConfig) -> Harness {
    let mut config = Config::test_default();
    // No PostgreSQL: like `compat_mode` tests; relies on the `default_mock`
    // serial group.
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;
    config.global.client_ip_rate_limit = Some(cfg);

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:anthropic:messages", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await
}

fn health_get() -> Request {
    http::Request::builder()
        .method(Method::GET)
        .uri("http://router.alephant.test/health")
        .body(axum_core::body::Body::empty())
        .unwrap()
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn memory_backend_second_request_in_window_returns_429() {
    let mut harness = harness_with_iprl(ClientIpRateLimitConfig {
        enabled: true,
        requests_per_second: 1,
        backend: ClientIpRateLimitBackend::Memory,
        trusted_proxy_cidrs: Vec::new(),
        redis_key_prefix: "gw:iprl:test:".to_string(),
    })
    .await;

    let r1 = harness.call(health_get()).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = harness.call(health_get()).await.unwrap();
    assert_eq!(r2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        r2.headers().contains_key(header::RETRY_AFTER),
        "expected Retry-After on client-ip 429"
    );

    harness.shutdown().await;
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn when_disabled_health_is_not_limited() {
    let mut harness = harness_with_iprl(ClientIpRateLimitConfig {
        enabled: false,
        requests_per_second: 1,
        backend: ClientIpRateLimitBackend::Memory,
        trusted_proxy_cidrs: Vec::new(),
        redis_key_prefix: "gw:iprl:test:".to_string(),
    })
    .await;

    assert_eq!(
        harness.call(health_get()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        harness.call(health_get()).await.unwrap().status(),
        StatusCode::OK
    );

    harness.shutdown().await;
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn trusted_x_forwarded_for_uses_distinct_client_ips() {
    let mut harness = harness_with_iprl(ClientIpRateLimitConfig {
        enabled: true,
        requests_per_second: 1,
        backend: ClientIpRateLimitBackend::Memory,
        trusted_proxy_cidrs: vec!["127.0.0.0/8".to_string()],
        redis_key_prefix: "gw:iprl:test:".to_string(),
    })
    .await;

    let req_a = http::Request::builder()
        .method(Method::GET)
        .uri("http://router.alephant.test/health")
        .header("x-forwarded-for", "198.51.100.10")
        .body(axum_core::body::Body::empty())
        .unwrap();
    let req_b_same = http::Request::builder()
        .method(Method::GET)
        .uri("http://router.alephant.test/health")
        .header("x-forwarded-for", "198.51.100.10")
        .body(axum_core::body::Body::empty())
        .unwrap();
    let req_c_other = http::Request::builder()
        .method(Method::GET)
        .uri("http://router.alephant.test/health")
        .header("x-forwarded-for", "198.51.100.11")
        .body(axum_core::body::Body::empty())
        .unwrap();

    assert_eq!(harness.call(req_a).await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        harness.call(req_b_same).await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        harness.call(req_c_other).await.unwrap().status(),
        StatusCode::OK
    );

    harness.shutdown().await;
}
