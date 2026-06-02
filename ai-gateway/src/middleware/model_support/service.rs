use std::task::{Context, Poll};

use axum_core::body::Body;
use futures::future::BoxFuture;
use http::{Method, header::USER_AGENT};
use http_body_util::{BodyExt, Full, Limited};
use tower::{Layer, Service};

use crate::{
    app_state::AppState,
    error::{api::ApiError, internal::InternalError, invalid_req::InvalidRequestError},
    middleware::model_support::parse::{
        MODEL_SUPPORT_MAX_BODY_BYTES, catalog_redis_key, model_field_from_json_body,
        split_provider_model,
    },
    types::{
        extensions::{AuthContext, RequestKind},
        provider::InferenceProvider,
        request::Request,
        response::Response,
    },
};

/// Every **POST** through `MetaRouter` may carry JSON with a top-level `model`.
/// We buffer the body once; if `model` is absent or body is not JSON, we
/// forward without catalog checks. Non-POST requests skip body collection.
#[inline]
fn should_inspect_post_body(method: &Method) -> bool {
    *method == Method::POST
}

fn should_skip_model_support(request_kind: Option<RequestKind>) -> bool {
    matches!(request_kind, Some(RequestKind::UnifiedApi))
}

fn should_bypass_model_support_before_body_read(request_kind: Option<RequestKind>) -> bool {
    matches!(request_kind, Some(RequestKind::AgentEvents))
}

fn canonical_provider_code(provider_raw: &str) -> Option<String> {
    InferenceProvider::from_provider_code(provider_raw)
        .ok()
        .map(|provider| provider.as_provider_code().to_string())
}

/// Used by `default_model` and this middleware; same Redis→DB resolution.
pub(crate) async fn gateway_model_supported(
    app_state: &AppState,
    provider_raw: &str,
    model_raw: &str,
) -> Result<bool, ApiError> {
    let canonical_provider = canonical_provider_code(provider_raw);

    let in_redis = if let Some(client) = app_state.redis() {
        let raw_key = catalog_redis_key(provider_raw, model_raw);
        let raw_exists = match client.key_exists(&raw_key).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    key = %raw_key,
                    "model_support: catalog key_exists failed; falling back to DB"
                );
                false
            }
        };
        if raw_exists {
            true
        } else if let Some(canonical_provider) = canonical_provider.as_deref() {
            if canonical_provider.eq_ignore_ascii_case(provider_raw) {
                false
            } else {
                let canonical_key = catalog_redis_key(canonical_provider, model_raw);
                match client.key_exists(&canonical_key).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            key = %canonical_key,
                            "model_support: catalog key_exists failed; falling back to DB"
                        );
                        false
                    }
                }
            }
        } else {
            false
        }
    } else {
        false
    };

    if in_redis {
        return Ok(true);
    }

    let Some(store) = app_state.router_store() else {
        return Ok(true);
    };

    let supported = store
        .gateway_model_pair_supported(provider_raw, model_raw)
        .await
        .map_err(ApiError::Internal)?;
    if supported {
        return Ok(true);
    }

    if let Some(canonical_provider) = canonical_provider.as_deref() {
        if canonical_provider.eq_ignore_ascii_case(provider_raw) {
            Ok(false)
        } else {
            store
                .gateway_model_pair_supported(canonical_provider, model_raw)
                .await
                .map_err(ApiError::Internal)
        }
    } else {
        Ok(false)
    }
}

fn restrict_bare_model_candidates_to_allowed_providers(
    candidates: Vec<String>,
    allowed_providers: Option<&[InferenceProvider]>,
) -> Vec<String> {
    let Some(allowed_providers) = allowed_providers.filter(|p| !p.is_empty()) else {
        return candidates;
    };

    candidates
        .into_iter()
        .filter(|candidate| {
            let Ok(parsed) = split_provider_model(candidate) else {
                return false;
            };
            allowed_providers.iter().any(|provider| {
                parsed
                    .provider_raw
                    .eq_ignore_ascii_case(provider.as_provider_code())
            })
        })
        .collect()
}

fn is_router_master_key_bare_model_passthrough(
    app_state: &AppState,
    extensions: &http::Extensions,
    model: &str,
) -> bool {
    if model.contains('/') {
        return false;
    }

    let Some(auth) = extensions.get::<AuthContext>() else {
        return false;
    };
    let Some(providers) = auth.master_key_allowed_providers.as_deref() else {
        return false;
    };
    let [provider] = providers else {
        return false;
    };

    app_state.provider_skips_model_mapping_catalog(provider)
}

/// Resolve a bare `model_id` (without `provider/` prefix) to a full
/// `provider/model_id` string. Looks up `BareModelExpandIndex` first,
/// falls back to DB.
///
/// Returns:
/// - `Ok(full_model)` when exactly one provider matches.
/// - `Err(UnsupportedGatewayModel)` when no provider matches.
/// - `Err(AmbiguousBareModel)` when multiple providers match.
async fn resolve_bare_model(
    app_state: &AppState,
    bare_model_id: &str,
    allowed_providers: Option<&[InferenceProvider]>,
) -> Result<String, ApiError> {
    let index = app_state.get_bare_model_expand_index();
    let mut candidates = index.gateway_models_for_bare_id(bare_model_id);

    if candidates.is_empty()
        && let Some(store) = app_state.router_store()
    {
        let db_rows = store
            .find_providers_for_bare_model(bare_model_id)
            .await
            .map_err(ApiError::Internal)?;
        candidates = db_rows
            .into_iter()
            .map(|(code, model_id)| format!("{code}/{model_id}"))
            .collect();
    }

    candidates = restrict_bare_model_candidates_to_allowed_providers(candidates, allowed_providers);

    match candidates.len() {
        0 => Err(ApiError::InvalidRequest(
            InvalidRequestError::UnsupportedGatewayModel(bare_model_id.to_string()),
        )),
        1 => Ok(candidates.into_iter().next().expect("len checked")),
        _ => Err(ApiError::InvalidRequest(
            InvalidRequestError::AmbiguousBareModel {
                model_id: bare_model_id.to_string(),
                candidates,
            },
        )),
    }
}

/// Replace the `"model"` field in a JSON body with `new_model`, returning
/// the re-serialized bytes. Returns `None` if the body is not valid JSON
/// or has no `"model"` field.
fn rewrite_model_in_body(body: &[u8], new_model: &str) -> Option<bytes::Bytes> {
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("model")?;
    v["model"] = serde_json::Value::String(new_model.to_string());
    serde_json::to_vec(&v).ok().map(bytes::Bytes::from)
}

fn normalize_async_openai_claude_model(model: &str) -> Option<String> {
    if !model.contains("claude-") {
        return None;
    }

    let (head, minor) = model.rsplit_once('-')?;
    if !minor.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (prefix, major) = head.rsplit_once('-')?;
    if !major.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    Some(format!("{prefix}-{major}.{minor}"))
}

fn maybe_rewrite_async_openai_claude_model(user_agent: &str, body: &[u8]) -> Option<bytes::Bytes> {
    if !user_agent.contains("AsyncOpenAI") {
        return None;
    }

    let model = model_field_from_json_body(body)?;
    let normalized = normalize_async_openai_claude_model(&model)?;
    rewrite_model_in_body(body, &normalized)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum_core::body::Body;
    use bytes::Bytes;
    use http::{Extensions, HeaderValue, Method, StatusCode, header::USER_AGENT};
    use http_body_util::BodyExt;
    use rustc_hash::FxHashMap;
    use tower::{Service, ServiceExt, service_fn};
    use uuid::Uuid;

    use super::{
        ModelSupportService, canonical_provider_code, is_router_master_key_bare_model_passthrough,
        maybe_rewrite_async_openai_claude_model,
        restrict_bare_model_candidates_to_allowed_providers, rewrite_model_in_body,
        should_bypass_model_support_before_body_read, should_inspect_post_body,
        should_skip_model_support,
    };
    use crate::{
        error::{api::ApiError, invalid_req::InvalidRequestError},
        types::{
            extensions::{AuthContext, RequestKind},
            org::OrgId,
            provider::InferenceProvider,
            request::Request,
            response::Response,
            secret::Secret,
            user::UserId,
        },
    };

    fn auth_context_with_allowed(providers: Option<Vec<InferenceProvider>>) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(Uuid::new_v4()),
            workspace_type: None,
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: "vk-test".to_string(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: None,
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            registered_agent_name: None,
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: providers,
        }
    }

    async fn app_with_router_flags(
        flags: impl IntoIterator<Item = (&'static str, bool)>,
    ) -> crate::app::App {
        let app = crate::app::build_test_app(crate::config::Config::default())
            .await
            .expect("build app");
        let mut map = FxHashMap::default();
        for (provider, is_router) in flags {
            map.insert(provider.to_string(), is_router);
        }
        app.state.set_provider_is_router_flags(map);
        app
    }

    async fn call_model_support(
        request_kind: RequestKind,
        body: &'static str,
        user_agent: Option<&'static str>,
    ) -> Result<Option<String>, ApiError> {
        let app = crate::app::build_test_app(crate::config::Config::default())
            .await
            .expect("build app");
        let forwarded_body = Arc::new(Mutex::new(None));
        let forwarded_body_for_inner = forwarded_body.clone();
        let inner = service_fn(move |req: Request| {
            let forwarded_body = forwarded_body_for_inner.clone();
            async move {
                let bytes = req
                    .into_body()
                    .collect()
                    .await
                    .expect("collect forwarded body")
                    .to_bytes();
                *forwarded_body.lock().expect("lock forwarded body") = Some(bytes);
                Ok::<Response, ApiError>(
                    http::Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::empty())
                        .expect("response"),
                )
            }
        });
        let mut service = ModelSupportService {
            inner,
            app_state: app.state,
        };
        let mut builder = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions");
        if let Some(user_agent) = user_agent {
            builder = builder.header(USER_AGENT, HeaderValue::from_static(user_agent));
        }
        let mut req = builder
            .body(Body::new(http_body_util::Full::new(Bytes::from_static(
                body.as_bytes(),
            ))))
            .expect("request");
        req.extensions_mut().insert(request_kind);

        service.ready().await?.call(req).await?;

        let forwarded = forwarded_body
            .lock()
            .expect("lock forwarded body")
            .clone()
            .expect("inner service called");
        let v: serde_json::Value = serde_json::from_slice(&forwarded).expect("valid json");
        Ok(v.get("model")
            .and_then(|model| model.as_str())
            .map(ToOwned::to_owned))
    }

    async fn forwarded_model_support_body(
        request_kind: RequestKind,
        body: &'static [u8],
        user_agent: Option<&'static str>,
    ) -> Result<Bytes, ApiError> {
        let app = crate::app::build_test_app(crate::config::Config::default())
            .await
            .expect("build app");
        let forwarded_body = Arc::new(Mutex::new(None));
        let forwarded_body_for_inner = forwarded_body.clone();
        let inner = service_fn(move |req: Request| {
            let forwarded_body = forwarded_body_for_inner.clone();
            async move {
                let bytes = req
                    .into_body()
                    .collect()
                    .await
                    .expect("collect forwarded body")
                    .to_bytes();
                *forwarded_body.lock().expect("lock forwarded body") = Some(bytes);
                Ok::<Response, ApiError>(
                    http::Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::empty())
                        .expect("response"),
                )
            }
        });
        let mut service = ModelSupportService {
            inner,
            app_state: app.state,
        };
        let mut builder = http::Request::builder()
            .method(Method::POST)
            .uri("/v1/agent/events");
        if let Some(user_agent) = user_agent {
            builder = builder.header(USER_AGENT, HeaderValue::from_static(user_agent));
        }
        let mut req = builder
            .body(Body::new(http_body_util::Full::new(Bytes::from_static(
                body,
            ))))
            .expect("request");
        req.extensions_mut().insert(request_kind);

        service.ready().await?.call(req).await?;

        Ok(forwarded_body
            .lock()
            .expect("lock forwarded body")
            .clone()
            .expect("inner service called"))
    }

    #[test]
    fn inspects_post_only() {
        assert!(should_inspect_post_body(&Method::POST));
        assert!(!should_inspect_post_body(&Method::GET));
        assert!(!should_inspect_post_body(&Method::PUT));
        assert!(!should_inspect_post_body(&Method::PATCH));
    }

    #[test]
    fn bypasses_model_support_before_body_read_for_agent_events_only() {
        assert!(should_bypass_model_support_before_body_read(Some(
            RequestKind::AgentEvents
        )));
        assert!(!should_bypass_model_support_before_body_read(Some(
            RequestKind::UnifiedApi
        )));
        assert!(!should_bypass_model_support_before_body_read(Some(
            RequestKind::DirectProxy
        )));
        assert!(!should_bypass_model_support_before_body_read(None));
    }

    #[test]
    fn skips_model_support_after_body_read_for_unified_api_only() {
        assert!(should_skip_model_support(Some(RequestKind::UnifiedApi)));
        assert!(!should_skip_model_support(Some(RequestKind::AgentEvents)));
        assert!(!should_skip_model_support(Some(RequestKind::DirectProxy)));
        assert!(!should_skip_model_support(None));
    }

    #[test]
    fn canonical_provider_code_maps_gemini_to_google() {
        assert_eq!(canonical_provider_code("gemini").as_deref(), Some("google"));
    }

    #[test]
    fn restrict_bare_model_candidates_uses_master_key_provider_code() {
        let candidates = vec![
            "minimax/minimax-m2".to_string(),
            "minimax-cn/minimax-m2".to_string(),
        ];
        let allowed = vec![InferenceProvider::Named("minimax-cn".into())];

        let filtered = restrict_bare_model_candidates_to_allowed_providers(
            candidates,
            Some(allowed.as_slice()),
        );

        assert_eq!(filtered, vec!["minimax-cn/minimax-m2".to_string()]);
    }

    #[tokio::test]
    async fn router_master_key_bare_model_passthrough_matches_single_router_provider() {
        let app = app_with_router_flags([("openrouter", true)]).await;
        let mut extensions = Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::Named("openrouter".into()),
        ])));

        assert!(is_router_master_key_bare_model_passthrough(
            &app.state,
            &extensions,
            "gpt-5.4",
        ));
    }

    #[tokio::test]
    async fn router_bare_model_helper_allows_service_to_skip_bare_resolution() {
        let app = app_with_router_flags([("openrouter", true)]).await;
        let mut extensions = Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::Named("openrouter".into()),
        ])));

        let candidate = "gpt-5.4";
        let should_skip_bare_resolution =
            is_router_master_key_bare_model_passthrough(&app.state, &extensions, candidate);

        assert!(should_skip_bare_resolution);
    }

    #[tokio::test]
    async fn router_master_key_bare_model_passthrough_rejects_provider_prefixed_model() {
        let app = app_with_router_flags([("openrouter", true)]).await;
        let mut extensions = Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::Named("openrouter".into()),
        ])));

        assert!(!is_router_master_key_bare_model_passthrough(
            &app.state,
            &extensions,
            "openrouter/gpt-5.4",
        ));
    }

    #[tokio::test]
    async fn router_master_key_bare_model_passthrough_rejects_non_router_provider() {
        let app = app_with_router_flags([("openrouter", false)]).await;
        let mut extensions = Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::Named("openrouter".into()),
        ])));

        assert!(!is_router_master_key_bare_model_passthrough(
            &app.state,
            &extensions,
            "gpt-5.4",
        ));
    }

    #[tokio::test]
    async fn router_master_key_bare_model_passthrough_rejects_multi_provider_key() {
        let app = app_with_router_flags([("openrouter", true), ("anthropic", false)]).await;
        let mut extensions = Extensions::new();
        extensions.insert(auth_context_with_allowed(Some(vec![
            InferenceProvider::Named("openrouter".into()),
            InferenceProvider::Anthropic,
        ])));

        assert!(!is_router_master_key_bare_model_passthrough(
            &app.state,
            &extensions,
            "gpt-5.4",
        ));
    }

    #[test]
    fn rewrite_model_replaces_field() {
        let body = br#"{"model":"gpt-4o-mini","messages":[]}"#;
        let rewritten = rewrite_model_in_body(body, "openai/gpt-4o-mini").expect("should rewrite");
        let v: serde_json::Value = serde_json::from_slice(&rewritten).expect("valid json");
        assert_eq!(v["model"], "openai/gpt-4o-mini");
        assert!(v["messages"].is_array());
    }

    #[test]
    fn rewrite_model_no_model_field_returns_none() {
        let body = br#"{"messages":[]}"#;
        assert!(rewrite_model_in_body(body, "openai/gpt-4o-mini").is_none());
    }

    #[test]
    fn rewrite_model_invalid_json_returns_none() {
        let body = b"not json";
        assert!(rewrite_model_in_body(body, "openai/gpt-4o-mini").is_none());
    }

    #[test]
    fn rewrite_model_preserves_other_fields() {
        let body = br#"{"model":"GPT-5","temperature":0.7,"max_tokens":100}"#;
        let rewritten = rewrite_model_in_body(body, "openai/gpt-5").expect("should rewrite");
        let v: serde_json::Value = serde_json::from_slice(&rewritten).expect("valid json");
        assert_eq!(v["model"], "openai/gpt-5");
        assert_eq!(v["temperature"], 0.7);
        assert_eq!(v["max_tokens"], 100);
    }

    #[test]
    fn async_openai_rewrites_claude_sonnet_dash_version_to_dot() {
        let body = br#"{"model":"anthropic/claude-sonnet-4-6","messages":[]}"#;
        let rewritten = maybe_rewrite_async_openai_claude_model("AsyncOpenAI/0.29.0", body)
            .expect("should rewrite");
        let v: serde_json::Value = serde_json::from_slice(&rewritten).expect("valid json");
        assert_eq!(v["model"], "anthropic/claude-sonnet-4.6");
    }

    #[tokio::test]
    async fn unified_api_unknown_explicit_model_passes_through() {
        let model = call_model_support(
            RequestKind::UnifiedApi,
            r#"{"model":"claude-sonnet-4.6","messages":[]}"#,
            None,
        )
        .await
        .expect("unified api should pass through");

        assert_eq!(model.as_deref(), Some("claude-sonnet-4.6"));
    }

    #[tokio::test]
    async fn unified_api_bare_model_keeps_original_body_model() {
        let model = call_model_support(
            RequestKind::UnifiedApi,
            r#"{"model":"gpt-4o","messages":[]}"#,
            None,
        )
        .await
        .expect("unified api should pass through");

        assert_eq!(model.as_deref(), Some("gpt-4o"));
    }

    #[tokio::test]
    async fn unified_api_async_openai_claude_dash_version_still_rewrites() {
        let model = call_model_support(
            RequestKind::UnifiedApi,
            r#"{"model":"claude-sonnet-4-6","messages":[]}"#,
            Some("AsyncOpenAI/0.29.0"),
        )
        .await
        .expect("unified api should pass through after rewrite");

        assert_eq!(model.as_deref(), Some("claude-sonnet-4.6"));
    }

    #[tokio::test]
    async fn agent_events_forwards_original_body_without_rewrite() {
        let body = br#"{"model":"claude-sonnet-4-6","events":[{"type":"metric"}]}"#;

        let forwarded = forwarded_model_support_body(
            RequestKind::AgentEvents,
            body,
            Some("AsyncOpenAI/0.29.0"),
        )
        .await
        .expect("agent events should pass through");

        assert_eq!(forwarded.as_ref(), body);
    }

    #[tokio::test]
    async fn direct_proxy_bare_model_still_returns_unsupported_gateway_model() {
        let err = call_model_support(
            RequestKind::DirectProxy,
            r#"{"model":"gpt-4o","messages":[]}"#,
            None,
        )
        .await
        .expect_err("direct proxy should validate model");

        assert!(matches!(
            err,
            ApiError::InvalidRequest(InvalidRequestError::UnsupportedGatewayModel(_))
        ));
    }

    #[tokio::test]
    async fn custom_provider_unknown_model_still_passes_through() {
        let model = call_model_support(
            RequestKind::CustomProvider,
            r#"{"model":"unknown-custom-model","messages":[]}"#,
            None,
        )
        .await
        .expect("custom provider should pass through");

        assert_eq!(model.as_deref(), Some("unknown-custom-model"));
    }

    #[test]
    fn async_openai_rewrites_claude_opus_and_haiku_dash_versions_to_dot() {
        let opus = maybe_rewrite_async_openai_claude_model(
            "AsyncOpenAI",
            br#"{"model":"claude-opus-4-6"}"#,
        )
        .expect("should rewrite opus");
        let opus: serde_json::Value = serde_json::from_slice(&opus).expect("valid json");
        assert_eq!(opus["model"], "claude-opus-4.6");

        let haiku = maybe_rewrite_async_openai_claude_model(
            "AsyncOpenAI",
            br#"{"model":"claude-haiku-4-5"}"#,
        )
        .expect("should rewrite haiku");
        let haiku: serde_json::Value = serde_json::from_slice(&haiku).expect("valid json");
        assert_eq!(haiku["model"], "claude-haiku-4.5");
    }

    #[test]
    fn async_openai_rewrites_future_claude_dash_version_to_dot() {
        let rewritten = maybe_rewrite_async_openai_claude_model(
            "AsyncOpenAI",
            br#"{"model":"anthropic/claude-sonnet-4-7"}"#,
        )
        .expect("should rewrite future claude version");
        let v: serde_json::Value = serde_json::from_slice(&rewritten).expect("valid json");
        assert_eq!(v["model"], "anthropic/claude-sonnet-4.7");
    }

    #[test]
    fn non_async_openai_user_agent_does_not_rewrite_claude_dash_version() {
        let body = br#"{"model":"anthropic/claude-sonnet-4-6","messages":[]}"#;
        assert!(maybe_rewrite_async_openai_claude_model("curl/8.0.0", body).is_none());
    }
}

#[derive(Clone)]
pub struct ModelSupportLayer {
    pub app_state: AppState,
}

impl<S> Layer<S> for ModelSupportLayer {
    type Service = ModelSupportService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ModelSupportService {
            inner,
            app_state: self.app_state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ModelSupportService<S> {
    inner: S,
    app_state: AppState,
}

impl<S> Service<Request> for ModelSupportService<S>
where
    S: Service<Request, Response = Response, Error = ApiError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(name = "model_support", skip_all)]
    fn call(&mut self, req: Request) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let app_state = self.app_state.clone();

        Box::pin(async move {
            if matches!(
                req.extensions().get::<RequestKind>(),
                Some(RequestKind::X402Agent)
            ) {
                return inner.call(req).await;
            }

            let (parts, body) = req.into_parts();
            let request_kind = parts.extensions.get::<RequestKind>().copied();

            if should_bypass_model_support_before_body_read(request_kind) {
                let req = Request::from_parts(parts, body);
                return inner.call(req).await;
            }

            let need_validate = should_inspect_post_body(&parts.method);

            if !need_validate {
                let req = Request::from_parts(parts, body);
                return inner.call(req).await;
            }

            let mut bytes = match Limited::new(body, MODEL_SUPPORT_MAX_BODY_BYTES)
                .collect()
                .await
            {
                Ok(c) => c.to_bytes(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "model_support: failed to collect request body"
                    );
                    return Err(ApiError::Internal(InternalError::Internal));
                }
            };

            if let Some(user_agent) = parts
                .headers
                .get(USER_AGENT)
                .and_then(|value| value.to_str().ok())
            {
                if let Some(rewritten) = maybe_rewrite_async_openai_claude_model(user_agent, &bytes)
                {
                    bytes = rewritten;
                }
            }

            if should_skip_model_support(request_kind) {
                let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
                return inner.call(req).await;
            }

            let body_model = model_field_from_json_body(&bytes);
            let model_candidates = body_model
                .clone()
                .map(|model| vec![model])
                .unwrap_or_default();

            if model_candidates.is_empty() {
                let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
                return inner.call(req).await;
            }

            if matches!(request_kind, Some(RequestKind::CustomProvider)) {
                let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
                return inner.call(req).await;
            }

            let is_direct_proxy = matches!(request_kind, Some(RequestKind::DirectProxy));
            let allowed_providers = parts
                .extensions
                .get::<AuthContext>()
                .and_then(|auth| auth.master_key_allowed_providers.clone());
            let allowed_providers = allowed_providers.as_deref();

            for candidate in &model_candidates {
                if is_direct_proxy || candidate.contains('/') {
                    let Ok(parsed) = split_provider_model(candidate) else {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(candidate.clone()),
                        ));
                    };
                    let supported =
                        gateway_model_supported(&app_state, parsed.provider_raw, parsed.model_raw)
                            .await?;
                    if !supported {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(candidate.clone()),
                        ));
                    }
                } else if is_router_master_key_bare_model_passthrough(
                    &app_state,
                    &parts.extensions,
                    candidate,
                ) {
                    continue;
                } else {
                    let resolved =
                        resolve_bare_model(&app_state, candidate, allowed_providers).await?;
                    if let Some(rewritten) = rewrite_model_in_body(&bytes, &resolved) {
                        bytes = rewritten;
                    }
                    let Ok(parsed) = split_provider_model(&resolved) else {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(resolved),
                        ));
                    };
                    let supported =
                        gateway_model_supported(&app_state, parsed.provider_raw, parsed.model_raw)
                            .await?;
                    if !supported {
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::UnsupportedGatewayModel(resolved),
                        ));
                    }
                }
            }

            let req = Request::from_parts(parts, Body::new(Full::new(bytes)));
            inner.call(req).await
        })
    }
}
