#![allow(clippy::large_futures)]

use ai_gateway::{
    app::build_test_gateway_app,
    config::{Config, alephant::AlephantFeatures},
    tests::TestDefault,
};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use tower::{Service, ServiceExt};

#[tokio::test]
#[serial_test::serial]
async fn openai_compatible_models_lists_configured_gateway_models() {
    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::None;
    config.compat_mode = true;
    let mut app = build_test_gateway_app(config)
        .await
        .expect("build test gateway app");

    let request = Request::builder()
        .method(Method::GET)
        .uri("http://router.alephant.test/v1/models")
        .body(axum_core::body::Body::empty())
        .unwrap();

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().expect("data should be an array");
    assert!(
        data.iter().any(|model| model["id"] == "gpt-4o-mini"
            && model["object"] == "model"
            && model["created"].is_number()
            && model["owned_by"].is_string()),
        "expected gpt-4o-mini model entry in {body}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn openai_compatible_models_requires_auth_when_auth_enabled() {
    let mut config = Config::test_default();
    config.alephant.features = AlephantFeatures::Auth;
    config.compat_mode = true;
    let mut app = build_test_gateway_app(config)
        .await
        .expect("build test gateway app");

    let request = Request::builder()
        .method(Method::GET)
        .uri("http://router.alephant.test/v1/models")
        .body(axum_core::body::Body::empty())
        .unwrap();

    let response = app.ready().await.unwrap().call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
