#![allow(clippy::large_futures)]

use std::collections::HashMap;

use ai_gateway::{
    config::{Config, alephant::AlephantFeatures},
    tests::{TestDefault, harness::Harness, mock::MockArgs},
};
use http::{Method, Request, StatusCode};
use serde_json::json;
use tower::Service;

const MASTER_KEY_ENCRYPTION_KEY_ENV: &str = "MASTER_KEY_ENCRYPTION_KEY";
const TEST_MASTER_KEY_ENCRYPTION_KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

struct MasterKeyGuard {
    previous: Option<String>,
}

impl MasterKeyGuard {
    fn set() -> Self {
        let previous = std::env::var(MASTER_KEY_ENCRYPTION_KEY_ENV).ok();
        if previous.is_none() {
            unsafe {
                std::env::set_var(
                    MASTER_KEY_ENCRYPTION_KEY_ENV,
                    TEST_MASTER_KEY_ENCRYPTION_KEY_B64,
                );
            }
        }
        Self { previous }
    }
}

impl Drop for MasterKeyGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(MASTER_KEY_ENCRYPTION_KEY_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(MASTER_KEY_ENCRYPTION_KEY_ENV);
            },
        }
    }
}

/// Test that requests are properly passed through to the `OpenAI` provider
/// when using the /{provider} base url.
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai_direct_proxy() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic passthrough
    // functionality
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:fake_endpoint", 1.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;

    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "test": "data"
        }))
        .unwrap(),
    );

    let request = Request::builder()
        .method(Method::POST)
        // Route to the fake endpoint through the default router
        .uri("http://router.alephant.test/openai/v1/fake_endpoint")
        .header("content-type", "application/json")
        .body(request_body)
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Test that requests are properly passed through to the Anthropic provider
/// when using the /{provider} base url.
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_direct_proxy() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic passthrough
    // functionality
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:anthropic:fake_endpoint", 1.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;

    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "test": "data"
        }))
        .unwrap(),
    );

    let request = Request::builder()
        .method(Method::POST)
        // Route to the fake endpoint through the default router
        .uri("http://router.alephant.test/anthropic/v1/fake_endpoint")
        .header("content-type", "application/json")
        .body(request_body)
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai_direct_proxy_strips_session_headers_before_upstream() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:fake_endpoint", 1.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();

    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;

    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "test": "data"
        }))
        .unwrap(),
    );

    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/openai/v1/fake_endpoint")
        .header("content-type", "application/json")
        .header("Alephant-Session-Id", "session-123")
        .header("Alephant-Session-Path", "workflow/direct")
        .header("Alephant-Session-Name", "Direct Planner")
        .body(request_body)
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let upstream_requests = harness
        .mock
        .openai_mock
        .http_server
        .received_requests_for("POST", "/v1/fake_endpoint")
        .await
        .unwrap_or_default();

    assert_eq!(upstream_requests.len(), 1, "expected one upstream request");

    let upstream_headers = &upstream_requests[0].headers;
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-id")
    );
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-path")
    );
    assert!(
        !upstream_headers
            .keys()
            .any(|name| name.as_str() == "alephant-session-name")
    );
}
