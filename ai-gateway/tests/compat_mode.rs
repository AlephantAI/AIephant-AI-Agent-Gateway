#![allow(clippy::large_futures)]

use std::collections::HashMap;

use ai_gateway::{
    config::{Config, alephant::AlephantFeatures},
    tests::{TestDefault, harness::Harness, mock::MockArgs},
};
use http::{Method, Request, StatusCode};
use serde_json::json;
use tower::Service;

const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Ensures `OPENAI_API_KEY` is set for compat-mode env-based upstream auth.
struct OpenAiKeyGuard {
    previous: Option<String>,
}

impl OpenAiKeyGuard {
    fn set(value: &str) -> Self {
        let previous = std::env::var(OPENAI_API_KEY_ENV).ok();
        unsafe {
            std::env::set_var(OPENAI_API_KEY_ENV, value);
        }
        Self { previous }
    }
}

impl Drop for OpenAiKeyGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe {
                std::env::set_var(OPENAI_API_KEY_ENV, v);
            },
            None => unsafe {
                std::env::remove_var(OPENAI_API_KEY_ENV);
            },
        }
    }
}

/// Without Postgres: `compat_mode` + `features=none` can start and complete one
/// `/v1/chat/completions`; when the body uses an Anthropic model name it should
/// still hit the OpenAI mock (forced OpenAI).
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn compat_mode_starts_and_routes_unified_api_to_openai() {
    let _openai_key = OpenAiKeyGuard::set("sk-compat-test-openai");

    let mut config = Config::test_default();
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
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

    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "anthropic/claude-sonnet-4-0",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .unwrap(),
    );

    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/v1/chat/completions")
        .header("content-type", "application/json")
        .body(request_body)
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
