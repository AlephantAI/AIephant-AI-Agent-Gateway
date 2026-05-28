use ai_gateway::{
    app::build_test_app,
    config::Config,
    endpoints::{ApiEndpoint, openai::OpenAI},
    error::{api::ApiError, invalid_req::InvalidRequestError},
    middleware::mapper::service::enforce_vk_model_policy_for_source_endpoint,
    types::extensions::VkPolicy,
};
use uuid::Uuid;

fn body_with_model(model: &str) -> bytes::Bytes {
    bytes::Bytes::from(
        serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        })
        .to_string(),
    )
}

#[test]
fn router_policy_check_skips_when_no_vk_policy() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = rt
        .block_on(build_test_app(Config::default()))
        .expect("build app");
    let ext = http::Extensions::new();
    let endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
    let body = body_with_model("gpt-4");
    assert!(
        enforce_vk_model_policy_for_source_endpoint(
            &app.state, &ext, &endpoint, &body
        )
        .is_ok()
    );
}

#[test]
fn router_policy_check_denies_blocked_model() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let app = rt
        .block_on(build_test_app(Config::default()))
        .expect("build app");
    let mut ext = http::Extensions::new();
    ext.insert(VkPolicy {
        virtual_key_id: Uuid::new_v4(),
        allowed_models: None,
        blocked_models: Some(vec!["gpt-4".to_string()]),
    });
    let endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
    let body = body_with_model("gpt-4");
    let err = enforce_vk_model_policy_for_source_endpoint(
        &app.state, &ext, &endpoint, &body,
    )
    .expect_err("expected denial");
    assert!(matches!(
        err,
        ApiError::InvalidRequest(InvalidRequestError::ModelAccessDenied(_))
    ));
}
