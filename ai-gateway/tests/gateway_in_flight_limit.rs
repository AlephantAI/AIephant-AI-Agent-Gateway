#![allow(clippy::large_futures)]

use std::collections::HashMap;

use ai_gateway::{
    config::{
        Config,
        alephant::AlephantFeatures,
        gateway_in_flight_limit::{GatewayInFlightBackend, GatewayInFlightLimitConfig},
    },
    tests::{TestDefault, harness::Harness, mock::MockArgs},
    types::request::Request,
};
use http::{Method, StatusCode, header};
use tower::Service;

async fn harness_with_gifl(cfg: GatewayInFlightLimitConfig) -> Harness {
    let mut config = Config::test_default();
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;
    config.global.gateway_in_flight_limit = Some(cfg);

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
async fn when_disabled_health_is_not_limited() {
    let mut harness = harness_with_gifl(GatewayInFlightLimitConfig {
        enabled: false,
        max_concurrent: 1,
        backend: GatewayInFlightBackend::Memory,
        redis_key_prefix: "gw:gifl:test:".to_string(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(default_mock)]
async fn second_request_429_while_first_holds_response_body() {
    let harness = harness_with_gifl(GatewayInFlightLimitConfig {
        enabled: true,
        max_concurrent: 1,
        backend: GatewayInFlightBackend::Memory,
        redis_key_prefix: "gw:gifl:test:".to_string(),
    })
    .await;

    let addr = harness.socket_addr;
    let fa = harness.app_factory.clone();
    let fa_bg = fa.clone();

    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();

    let bg = tokio::spawn(async move {
        let mut f = fa_bg;
        let mut svc = Service::call(&mut f, addr).await.unwrap();
        futures::future::poll_fn(|cx| Service::poll_ready(&mut svc, cx))
            .await
            .unwrap();
        let r = Service::call(&mut svc, health_get()).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let _ = gate_tx.send(());
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        drop(r);
    });

    gate_rx.await.unwrap();

    let mut f2 = fa.clone();
    let mut svc2 = Service::call(&mut f2, addr).await.unwrap();
    futures::future::poll_fn(|cx| Service::poll_ready(&mut svc2, cx))
        .await
        .unwrap();
    let r2 = Service::call(&mut svc2, health_get()).await.unwrap();
    assert_eq!(r2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        r2.headers().contains_key(header::RETRY_AFTER),
        "expected Retry-After on in-flight 429"
    );

    bg.await.unwrap();
    harness.shutdown().await;
}
