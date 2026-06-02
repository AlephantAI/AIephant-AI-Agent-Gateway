#![allow(clippy::large_futures, clippy::doc_markdown)]

use std::{collections::HashMap, net::TcpListener};

use ai_gateway::{
    config::{
        Config,
        alephant::AlephantFeatures,
        balance::{BalanceConfig, BalanceConfigInner, WeightedProvider},
        router::{RouterConfig, RouterConfigs},
    },
    tests::{TestDefault, harness::Harness, mock::MockArgs},
    types::{provider::InferenceProvider, router::RouterId},
};
use compact_str::CompactString;
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use nonempty_collections::nes;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tower::Service;
use url::Url;

const MASTER_KEY_ENCRYPTION_KEY_ENV: &str = "MASTER_KEY_ENCRYPTION_KEY";
const TEST_MASTER_KEY_ENCRYPTION_KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
const AWS_ACCESS_KEY_ID_ENV: &str = "AWS_ACCESS_KEY_ID";
const AWS_SECRET_ACCESS_KEY_ENV: &str = "AWS_SECRET_ACCESS_KEY";
const TEST_BEDROCK_ACCESS_KEY: &str = "bedrock-access-key-test";
const TEST_BEDROCK_SECRET_KEY: &str = "bedrock-secret-key-test";
const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const TEST_OPENAI_API_KEY: &str = "sk-openai-compatible-test";

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

struct AwsCredsGuard {
    previous_access_key: Option<String>,
    previous_secret_key: Option<String>,
}

impl AwsCredsGuard {
    fn set() -> Self {
        let previous_access_key = std::env::var(AWS_ACCESS_KEY_ID_ENV).ok();
        let previous_secret_key = std::env::var(AWS_SECRET_ACCESS_KEY_ENV).ok();
        unsafe {
            std::env::set_var(AWS_ACCESS_KEY_ID_ENV, TEST_BEDROCK_ACCESS_KEY);
            std::env::set_var(AWS_SECRET_ACCESS_KEY_ENV, TEST_BEDROCK_SECRET_KEY);
        }
        Self {
            previous_access_key,
            previous_secret_key,
        }
    }
}

impl Drop for AwsCredsGuard {
    fn drop(&mut self) {
        match &self.previous_access_key {
            Some(value) => unsafe {
                std::env::set_var(AWS_ACCESS_KEY_ID_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(AWS_ACCESS_KEY_ID_ENV);
            },
        }
        match &self.previous_secret_key {
            Some(value) => unsafe {
                std::env::set_var(AWS_SECRET_ACCESS_KEY_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(AWS_SECRET_ACCESS_KEY_ENV);
            },
        }
    }
}

struct OpenAICompatApiKeyGuard {
    previous: Option<String>,
}

impl OpenAICompatApiKeyGuard {
    fn set() -> Self {
        let previous = std::env::var(OPENAI_API_KEY_ENV).ok();
        unsafe {
            std::env::set_var(OPENAI_API_KEY_ENV, TEST_OPENAI_API_KEY);
        }
        Self { previous }
    }
}

impl Drop for OpenAICompatApiKeyGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(OPENAI_API_KEY_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(OPENAI_API_KEY_ENV);
            },
        }
    }
}

async fn response_parts(response: ai_gateway::app::AppResponse) -> (StatusCode, String) {
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn assert_openai_chat_completion_shape(body: &str) {
    let payload: Value = serde_json::from_str(body).expect("response should be valid json");
    assert_eq!(payload["object"], "chat.completion");
    assert_eq!(payload["choices"][0]["message"]["role"], "assistant");
    assert!(
        payload["choices"][0]["message"]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty()),
        "response should contain assistant content: {body}"
    );
}

/// Sending a request to https://localhost/router should
/// result in the proxied request targeting https://api.openai.com/v1/chat/completions
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn openai() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic provider
    // functionality
    config.alephant.features = AlephantFeatures::None;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
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
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn router_path_strips_session_headers_before_upstream() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
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
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, session router!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        .header("Alephant-Session-Id", "session-123")
        .header("Alephant-Session-Path", "workflow/router")
        .header("Alephant-Session-Name", "Router Planner")
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();

    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let upstream_requests = harness
        .mock
        .openai_mock
        .http_server
        .received_requests_for("POST", "/v1/chat/completions")
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

/// Sending a request to https://localhost/router should
// result in the proxied request targeting https://generativelanguage.googleapis.com/v1beta/openai/chat/completions
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn google_with_openai_request_style() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic provider
    // functionality
    config.alephant.features = AlephantFeatures::None;
    let router_config = RouterConfigs::new(HashMap::from([(
        RouterId::Named(CompactString::new("my-router")),
        RouterConfig {
            load_balance: BalanceConfig::google_gemini(),
            ..Default::default()
        },
    )]));
    config.routers = router_config;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:gemini:generate_content", 2.into()),
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
            "model": "gemini/gemini-2.0-flash",
            "messages": [
                {"role": "user", "content": "Explain to me how AI works"}
            ],
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini",
            "messages": [
                {"role": "user", "content": "Explain to me how AI works"}
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    let (status, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
}

/// Sending a request to https://localhost/router should
/// result in the proxied request targeting https://api.openai.com/v1/chat/completions
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn anthropic_with_openai_request_style() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic provider
    // functionality
    config.alephant.features = AlephantFeatures::None;
    let router_config = RouterConfigs::new(HashMap::from([(
        RouterId::Named(CompactString::new("my-router")),
        RouterConfig {
            load_balance: BalanceConfig::anthropic_chat(),
            ..Default::default()
        },
    )]));
    config.routers = router_config;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:anthropic:messages", 2.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "anthropic/claude-sonnet-4-0",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    let (status, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);

    // test that using an openai model name works as well
    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "openai/gpt-4o-mini",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    let (status, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
}

/// Sending a request to https://localhost/router should
/// result in the proxied request targeting Ollama chat completions endpoint
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn ollama() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic provider
    // functionality
    config.alephant.features = AlephantFeatures::None;
    let router_config = RouterConfigs::new(HashMap::from([(
        RouterId::Named(CompactString::new("my-router")),
        RouterConfig {
            load_balance: BalanceConfig::ollama_chat(),
            ..Default::default()
        },
    )]));
    config.routers = router_config;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:ollama:chat_completions", 1.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "ollama/llama3",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    let (status, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
}

/// Sending a request to https://localhost/router should
/// result in the proxied request targeting DeepSeek openai-compatible endpoint
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn deepseek_with_openai_request_style() {
    let _master_key_guard = MasterKeyGuard::set();
    let _openai_api_key_guard = OpenAICompatApiKeyGuard::set();
    let port = reserve_port();
    let mut config = Config::test_default();
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;
    config
        .providers
        .get_mut(&InferenceProvider::Named("deepseek".into()))
        .expect("deepseek provider config should exist")
        .base_url =
        Url::parse(&format!("http://127.0.0.1:{port}/")).expect("valid deepseek mock base url");
    let router_config = RouterConfigs::new(HashMap::from([(
        RouterId::Named(CompactString::new("my-router")),
        RouterConfig {
            load_balance: BalanceConfig(HashMap::from([(
                ai_gateway::endpoints::EndpointType::Chat,
                BalanceConfigInner::ProviderWeighted {
                    providers: nes![WeightedProvider {
                        provider: InferenceProvider::Named("deepseek".into()),
                        weight: Decimal::from(1),
                    }],
                },
            )])),
            ..Default::default()
        },
    )]));
    config.routers = router_config;
    let mock_args = MockArgs::builder()
        .openai_port(port)
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let request_body = axum_core::body::Body::from(
        serde_json::to_vec(&json!({
            "model": "deepseek/deepseek-chat",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    let (status, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK, "response body: {body}");
    assert_openai_chat_completion_shape(&body);
}

/// Sending a request to https://localhost/router should
/// result in the proxied request targeting Bedrock converse endpoint
#[tokio::test]
#[serial_test::serial(default_mock)]
async fn bedrock_with_openai_request_style() {
    let _master_key_guard = MasterKeyGuard::set();
    let _aws_creds_guard = AwsCredsGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    config.alephant.features = AlephantFeatures::None;
    let router_config = RouterConfigs::new(HashMap::from([(
        RouterId::Named(CompactString::new("my-router")),
        RouterConfig {
            load_balance: BalanceConfig::bedrock(),
            ..Default::default()
        },
    )]));
    config.routers = router_config;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:bedrock:converse", 1.into()),
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
            "model": "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(default_mock)]
async fn mistral() {
    let _master_key_guard = MasterKeyGuard::set();
    let mut config = Config::test_default();
    config.compat_mode = true;
    // Disable auth for this test since we're testing basic provider
    // functionality
    config.alephant.features = AlephantFeatures::None;
    let router_config = RouterConfigs::new(HashMap::from([(
        RouterId::Named(CompactString::new("my-router")),
        RouterConfig {
            load_balance: BalanceConfig::mistral(),
            ..Default::default()
        },
    )]));
    config.routers = router_config;
    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:mistral:chat_completion", 1.into()),
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
            "model": "mistral/mistral-large-latest",
            "messages": [
                {
                    "role": "user",
                    "content": "Hello, world!"
                }
            ]
        }))
        .unwrap(),
    );
    let request = Request::builder()
        .method(Method::POST)
        // default router
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
