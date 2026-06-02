#![allow(clippy::large_futures)]

use std::collections::HashMap;

use ai_gateway::{
    config::{Config, alephant::AlephantFeatures},
    tests::{TestDefault, harness::Harness, mock::MockArgs},
};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::Service;

const MASTER_KEY_ENCRYPTION_KEY_ENV: &str = "MASTER_KEY_ENCRYPTION_KEY";
const TEST_MASTER_KEY_ENCRYPTION_KEY_B64: &str =
    "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

struct MasterKeyGuard {
    previous: Option<String>,
}

impl MasterKeyGuard {
    fn set() -> Self {
        let previous = std::env::var(MASTER_KEY_ENCRYPTION_KEY_ENV).ok();
        unsafe {
            std::env::set_var(
                MASTER_KEY_ENCRYPTION_KEY_ENV,
                TEST_MASTER_KEY_ENCRYPTION_KEY_B64,
            );
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

#[tokio::test]
#[serial_test::serial]
async fn unauthorized() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::Auth;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:anthropic:messages", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/v1/chat/completions")
        .body(axum_core::body::Body::empty())
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response_body = response.into_body().collect().await.unwrap();
    let response_body = serde_json::from_slice::<
        async_openai::error::WrappedError,
    >(&response_body.to_bytes());
    assert!(
        response_body.is_ok(),
        "should be able to deserialize error json into openai error format"
    );
    let response_body = response_body.unwrap();
    assert_eq!(
        response_body.error.r#type,
        Some("invalid_request_error".to_string())
    );
    assert_eq!(
        response_body.error.code,
        Some("invalid_api_key".to_string())
    );
}

#[tokio::test]
#[serial_test::serial]
async fn invalid_request_body() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:anthropic:messages", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/v1/chat/completions")
        .body(axum_core::body::Body::empty())
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_body = response.into_body().collect().await.unwrap();
    let response_body = serde_json::from_slice::<
        async_openai::error::WrappedError,
    >(&response_body.to_bytes());
    assert!(
        response_body.is_ok(),
        "should be able to deserialize error json into openai error format"
    );
    let response_body = response_body.unwrap();
    assert_eq!(
        response_body.error.r#type,
        Some("invalid_request_error".to_string())
    );
    assert_eq!(response_body.error.code, None);
}

#[tokio::test]
#[serial_test::serial]
async fn unsupported_model_keeps_openai_error_shape() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:anthropic:messages", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;

    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/v1/chat/completions")
        .header("content-type", "application/json")
        .body(axum_core::body::Body::from(
            serde_json::to_vec(&json!({
                "model": "unknown-provider/demo-model",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ]
            }))
            .unwrap(),
        ))
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_body = response.into_body().collect().await.unwrap();
    let response_body = serde_json::from_slice::<
        async_openai::error::WrappedError,
    >(&response_body.to_bytes());
    assert!(
        response_body.is_ok(),
        "should be able to deserialize unsupported model response into openai \
         error format"
    );
    let response_body = response_body.unwrap();
    assert_eq!(
        response_body.error.r#type,
        Some("invalid_request_error".to_string())
    );
    assert_eq!(response_body.error.code, None);
    assert_eq!(
        response_body.error.message,
        "Unsupported model: unknown-provider/demo-model"
    );
}
