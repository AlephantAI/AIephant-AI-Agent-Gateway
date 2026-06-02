#![allow(clippy::large_futures)]

//! Integration tests verifying that `dispatcher/service.rs` routes both
//! `Router` and `UnifiedApi` request kinds through `resolved_global_retry`,
//! i.e. a single `fallback-policy.retry` configuration drives retries for
//! all non-DirectProxy paths.

use std::collections::HashMap;

use ai_gateway::{
    config::{
        Config, alephant::AlephantFeatures,
        fallback_policy::FallbackPolicyConfig,
    },
    tests::{TestDefault, harness::Harness, mock::MockArgs},
};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::Service;

fn test_config_with_global_retry() -> Config {
    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::All;
    // Use fallback-policy retry only — do NOT set per-path retries.
    config.fallback_policy = FallbackPolicyConfig {
        enabled: true,
        retry: ai_gateway::config::retry::RetryConfig::test_default(),
        ..FallbackPolicyConfig::default()
    };
    // Ensure no legacy per-path retries are set.
    config.unified_api.retries = None;
    config.global.retries = None;
    config
}

fn openai_body() -> axum_core::body::Body {
    axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .unwrap(),
    )
}

// ─── UnifiedApi path ─────────────────────────────────────────────────────────

/// With only `fallback-policy.retry` set (no `unified_api.retries`), the
/// `UnifiedApi` path must still retry. The mock returns 3 server errors, which
/// exhausts the default test retry count (2 extra), so the final response is
/// still 500 — proving retries happened.
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn global_retry_drives_unified_api_retries() {
    let config = test_config_with_global_retry();

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("internal_error:openai:chat_completion", 3.into()),
            ("success:s3:upload_request", 1.into()),
            ("success:alephant:log_request", 1.into()),
            ("success:alephant:sign_s3_url", 1.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;

    let req = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", "Bearer sk-alephant-test-key")
        .body(openai_body())
        .unwrap();

    let response = harness.call(req).await.unwrap();
    // All 3 attempts exhausted → still 500.
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "global retry should drive UnifiedApi retries"
    );
    let _ = response.into_body().collect().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
}

// ─── Router path ─────────────────────────────────────────────────────────────

/// With only `fallback-policy.retry` set (no router-level retries field), the
/// Router path must also retry.
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn global_retry_drives_router_retries() {
    let config = test_config_with_global_retry();

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("internal_error:openai:chat_completion", 3.into()),
            ("success:s3:upload_request", 1.into()),
            ("success:alephant:log_request", 1.into()),
            ("success:alephant:sign_s3_url", 1.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;

    let req = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", "Bearer sk-alephant-test-key")
        .body(openai_body())
        .unwrap();

    let response = harness.call(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "global retry should drive Router retries"
    );
    let _ = response.into_body().collect().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
}
