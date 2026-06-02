use std::{
    future::Future,
    pin::{Pin, pin},
    task::{Context, Poll},
};

use bytes::Bytes;
use futures::{future::BoxFuture, ready};
use http::uri::PathAndQuery;
use http_body_util::{BodyExt, combinators::Collect};
use indexmap::IndexSet;
use pin_project_lite::pin_project;
use serde::Serialize;
use tower::Service as _;

use crate::{
    app_state::AppState,
    default_model::choose_default_gateway_model,
    endpoints::{ApiEndpoint, anthropic::Anthropic, openai::OpenAI},
    error::{
        api::ApiError, init::InitError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    ide_adapation::{
        apply_chat_completions_body_redirect_if_needed,
        client_profile::resolve_client_profile,
    },
    middleware::{
        large_context::maybe_transform_unified_api_chat_request,
        model_support::model_field_from_json_body,
    },
    router::{
        direct::{DirectProxies, DirectProxyService},
        unified_route_planner::{UnifiedRoutePlanner, UnifiedRouteRequest},
    },
    types::{
        extensions::{
            AuthContext, MasterKeyUnifiedModelPassthrough,
            UnifiedImplicitModelFallbackContext, UnifiedModelBodyPassthrough,
            UnifiedModelPolicyChecked, VkPolicy,
        },
        provider::InferenceProvider,
        request::Request,
        response::Response,
    },
    utils::debug_log::{self, DebugLogConfig},
};

#[derive(Debug, Clone)]
pub struct Service {
    direct_proxies: DirectProxies,
    app_state: AppState,
}

impl Service {
    pub async fn new(app_state: &AppState) -> Result<Self, InitError> {
        let direct_proxies = DirectProxies::new(app_state).await?;
        Ok(Self {
            direct_proxies,
            app_state: app_state.clone(),
        })
    }
}

impl tower::Service<Request> for Service {
    type Response = Response;
    type Error = ApiError;
    type Future = ResponseFuture;

    #[inline]
    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[tracing::instrument(name = "unified_api", skip_all)]
    fn call(&mut self, req: Request) -> Self::Future {
        if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
            tracing::info!("[unified_api] call: body collection started");
        }
        let (mut parts, body) = req.into_parts();
        let debug_log_config = parts
            .extensions
            .get::<DebugLogConfig>()
            .copied()
            .unwrap_or_else(|| {
                DebugLogConfig::from_headers(&mut parts.headers)
            });
        debug_log::maybe_log_headers(
            "unified_api",
            &parts.headers,
            debug_log_config,
        );
        let direct_proxies = self.direct_proxies.clone();
        let app_state = self.app_state.clone();
        let collect_future = body.collect();
        ResponseFuture::new(
            collect_future,
            parts,
            direct_proxies,
            app_state,
            debug_log_config,
        )
    }
}

pin_project! {
    #[project = StateProj]
    enum State {
        CollectBody {
            #[pin]
            collect_future: Collect<axum_core::body::Body>,
            parts: Option<http::request::Parts>,
        },
        /// `pre_transformed`: when `true`, body already passed `maybe_transform` (and optional default `model` injection).
        DetermineProvider {
            collected_body: Option<Bytes>,
            parts: Option<http::request::Parts>,
            pre_transformed: bool,
        },
        AwaitDefaultModel {
            #[pin]
            fut: BoxFuture<'static, Result<Bytes, ApiError>>,
            parts: Option<http::request::Parts>,
        },
        ListModels {
            #[pin]
            fut: BoxFuture<'static, Result<Response, ApiError>>,
        },
        InitProxy {
            request: Option<Request>,
            provider: InferenceProvider,
        },
        Proxy {
            #[pin]
            response_future: <DirectProxyService as tower::Service<Request>>::Future,
        },
    }
}

pin_project! {
    pub struct ResponseFuture {
        #[pin]
        state: State,
        direct_proxies: DirectProxies,
        app_state: AppState,
        debug_log_config: DebugLogConfig,
    }
}

impl ResponseFuture {
    pub fn new(
        collect_future: Collect<axum_core::body::Body>,
        parts: http::request::Parts,
        direct_proxies: DirectProxies,
        app_state: AppState,
        debug_log_config: DebugLogConfig,
    ) -> Self {
        Self {
            state: State::CollectBody {
                collect_future,
                parts: Some(parts),
            },
            direct_proxies,
            app_state,
            debug_log_config,
        }
    }
}

pub enum UnifiedApi {
    ChatCompletions,
    Completions,
    Embeddings,
    ImageGenerations,
    Responses,
    Messages,
    Models,
}

impl TryFrom<&str> for UnifiedApi {
    type Error = InvalidRequestError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chat/completions" => Ok(Self::ChatCompletions),
            "completions" => Ok(Self::Completions),
            "embeddings" => Ok(Self::Embeddings),
            "images/generations" => Ok(Self::ImageGenerations),
            "responses" => Ok(Self::Responses),
            "messages" => Ok(Self::Messages),
            "models" => Ok(Self::Models),
            _ => {
                Err(InvalidRequestError::UnsupportedEndpoint(value.to_string()))
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct ModelsListResponse {
    object: &'static str,
    data: Vec<ModelListItem>,
}

#[derive(Debug, Serialize)]
struct ModelListItem {
    id: String,
    object: &'static str,
    created: i64,
    owned_by: String,
}

async fn list_models_response(
    app_state: AppState,
) -> Result<Response, ApiError> {
    let mut models = models_from_provider_config(&app_state);

    if models.is_empty()
        && let Some(store) = app_state.router_store()
    {
        let providers = store
            .get_all_providers_for_gateway()
            .await
            .map_err(ApiError::Internal)?;
        let provider_codes = providers
            .into_iter()
            .map(|provider| (provider.id, provider.code))
            .collect::<std::collections::HashMap<_, _>>();
        models = store
            .get_all_provider_models_for_gateway()
            .await
            .map_err(ApiError::Internal)?
            .into_iter()
            .filter_map(|model| {
                let owned_by = provider_codes.get(&model.provider_id)?.clone();
                Some((model.model_id, owned_by))
            })
            .collect();
    }

    let data = models
        .into_iter()
        .map(|(id, owned_by)| ModelListItem {
            id,
            object: "model",
            created: 0,
            owned_by,
        })
        .collect();
    let body = serde_json::to_vec(&ModelsListResponse {
        object: "list",
        data,
    })
    .map_err(|_| ApiError::Internal(InternalError::Internal))?;

    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(axum_core::body::Body::from(body))
        .map_err(|_| ApiError::Internal(InternalError::Internal))
}

fn models_from_provider_config(
    app_state: &AppState,
) -> IndexSet<(String, String)> {
    app_state
        .get_providers_config()
        .iter()
        .flat_map(|(provider, config)| {
            let provider_code = provider.as_provider_code().to_string();
            config.models.iter().map(move |model| {
                (model.as_model_name().to_string(), provider_code.clone())
            })
        })
        .collect()
}

#[cfg(test)]
mod models_tests {
    use http_body_util::BodyExt as _;

    use super::*;
    use crate::{app::build_test_app, config::Config};

    #[test]
    fn models_path_is_supported_unified_api() {
        assert!(matches!(
            UnifiedApi::try_from("models").expect("models path"),
            UnifiedApi::Models
        ));
    }

    #[tokio::test]
    async fn list_models_response_uses_provider_config_snapshot() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let response = list_models_response(app.state)
            .await
            .expect("models response");
        assert_eq!(response.status(), http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["object"], "list");
        assert!(body["data"].as_array().expect("data array").iter().any(
            |model| model["id"] == "gpt-4o-mini"
                && model["object"] == "model"
                && model["owned_by"] == "openai"
        ));
    }
}

/// Inject `model` into the body; if a non-empty `model` already exists, return
/// unchanged.
async fn inject_default_model_into_body_unified(
    app: AppState,
    path: String,
    body: Bytes,
    auth: AuthContext,
    vk: VkPolicy,
) -> Result<Bytes, ApiError> {
    if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
        tracing::info!(
            "[unified_api] inject_default_model_into_body: entered path={path}"
        );
    }
    tracing::warn!(path = %path, "inject_default_model_into_body_unified: entered");
    if model_field_from_json_body(&body).is_some() {
        return Ok(body);
    }
    let chosen = choose_default_gateway_model(&app, &path, &auth, &vk).await?;
    let mut v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    v["model"] = serde_json::Value::String(chosen);
    let out = serde_json::to_vec(&v)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    Ok(Bytes::from(out))
}

fn implicit_default_model_fallback_context(
    path: &str,
    body: &Bytes,
) -> Option<UnifiedImplicitModelFallbackContext> {
    if path != "chat/completions" {
        return None;
    }
    let selected_model = model_field_from_json_body(body)?.to_string();
    Some(UnifiedImplicitModelFallbackContext { selected_model })
}

impl Future for ResponseFuture {
    type Output = Result<Response, ApiError>;

    #[allow(clippy::too_many_lines)]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        loop {
            match this.state.as_mut().project() {
                StateProj::CollectBody {
                    collect_future,
                    parts,
                } => {
                    let collected = match ready!(pin!(collect_future).poll(cx))
                    {
                        Ok(collected) => collected,
                        Err(e) => {
                            return Poll::Ready(Err(
                                InternalError::CollectBodyError(e).into(),
                            ));
                        }
                    };
                    let collected_bytes = collected.to_bytes();
                    if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
                        tracing::info!(
                            "[unified_api] body collected, len={}",
                            collected_bytes.len()
                        );
                    }
                    debug_log::maybe_log_body(
                        "unified_api",
                        &collected_bytes,
                        *this.debug_log_config,
                    );
                    let mut parts =
                        parts.take().expect("future polled after completion");
                    let Some(extracted_path_and_query) =
                        parts.extensions.get::<PathAndQuery>()
                    else {
                        return Poll::Ready(Err(
                            InternalError::ExtensionNotFound("PathAndQuery")
                                .into(),
                        ));
                    };

                    let unified =
                        UnifiedApi::try_from(extracted_path_and_query.path())?;
                    if matches!(unified, UnifiedApi::Models) {
                        this.state.set(State::ListModels {
                            fut: Box::pin(list_models_response(
                                this.app_state.clone(),
                            )),
                        });
                        continue;
                    }
                    let api = match unified {
                        UnifiedApi::ChatCompletions => {
                            ApiEndpoint::OpenAI(OpenAI::chat_completions())
                        }
                        UnifiedApi::Completions => {
                            ApiEndpoint::OpenAI(OpenAI::completions())
                        }
                        UnifiedApi::Embeddings => {
                            ApiEndpoint::OpenAI(OpenAI::embeddings())
                        }
                        UnifiedApi::ImageGenerations => {
                            ApiEndpoint::OpenAI(OpenAI::image_generations())
                        }
                        UnifiedApi::Responses => {
                            ApiEndpoint::OpenAI(OpenAI::responses())
                        }
                        UnifiedApi::Messages => {
                            ApiEndpoint::Anthropic(Anthropic::messages())
                        }
                        UnifiedApi::Models => unreachable!("handled above"),
                    };
                    parts.extensions.insert(api);

                    this.state.set(State::DetermineProvider {
                        collected_body: Some(collected_bytes),
                        parts: Some(parts),
                        pre_transformed: false,
                    });
                }
                StateProj::ListModels { mut fut } => {
                    let res = if let std::task::Poll::Ready(r) =
                        fut.as_mut().poll(cx)
                    {
                        r
                    } else {
                        return std::task::Poll::Pending;
                    };
                    return std::task::Poll::Ready(res);
                }
                StateProj::AwaitDefaultModel { mut fut, parts } => {
                    let res = if let std::task::Poll::Ready(r) =
                        fut.as_mut().poll(cx)
                    {
                        r
                    } else {
                        return std::task::Poll::Pending;
                    };
                    let body = match res {
                        Ok(b) => b,
                        Err(e) => {
                            return std::task::Poll::Ready(Err(e));
                        }
                    };
                    let mut parts =
                        parts.take().expect("future polled after completion");
                    let path = parts
                        .extensions
                        .get::<PathAndQuery>()
                        .map(|value| value.path().to_string())
                        .unwrap_or_default();
                    if let Some(ctx) =
                        implicit_default_model_fallback_context(&path, &body)
                    {
                        parts.extensions.insert(ctx);
                    }
                    this.state.set(State::DetermineProvider {
                        collected_body: Some(body),
                        parts: Some(parts),
                        pre_transformed: true,
                    });
                }
                StateProj::DetermineProvider {
                    collected_body,
                    parts,
                    pre_transformed,
                } => {
                    let original_body = collected_body
                        .take()
                        .expect("future polled after completion");
                    let mut parts =
                        parts.take().expect("future polled after completion");
                    let mut path = parts
                        .extensions
                        .get::<PathAndQuery>()
                        .ok_or(InternalError::ExtensionNotFound(
                            "PathAndQuery",
                        ))?
                        .path()
                        .to_string();
                    let body = if *pre_transformed {
                        original_body
                    } else {
                        maybe_transform_unified_api_chat_request(
                            this.app_state,
                            &mut parts,
                            original_body,
                        )?
                    };
                    if !*pre_transformed
                        && model_field_from_json_body(&body).is_none()
                    {
                        let Some(auth) =
                            parts.extensions.get::<AuthContext>().cloned()
                        else {
                            return Poll::Ready(Err(
                                InvalidRequestError::NoModelAvailable.into(),
                            ));
                        };
                        let Some(vk) =
                            parts.extensions.get::<VkPolicy>().cloned()
                        else {
                            return Poll::Ready(Err(
                                InvalidRequestError::NoModelAvailable.into(),
                            ));
                        };
                        let app = this.app_state.clone();
                        let path_c = path.clone();
                        this.state.set(State::AwaitDefaultModel {
                            fut: Box::pin(
                                inject_default_model_into_body_unified(
                                    app, path_c, body, auth, vk,
                                ),
                            ),
                            parts: Some(parts),
                        });
                        // Must `continue` to the next `match` round and
                        // immediately `poll` the inject
                        // future. Returning `Pending` here without having
                        // polled the child future can
                        // strand the task on some executor paths once the body
                        // is fully read and no I/O
                        // remains (when `model` is present we skip
                        // `AwaitDefaultModel`, so this path matters).
                        continue;
                    }
                    path = apply_chat_completions_body_redirect_if_needed(
                        &path, &body, &mut parts,
                    )?;
                    let explicit_client_model = !*pre_transformed;
                    let client_profile =
                        resolve_client_profile(&parts.headers).profile;
                    let planner =
                        UnifiedRoutePlanner::new(this.app_state.clone());
                    let decision = planner.plan(UnifiedRouteRequest {
                        path: path.clone(),
                        body,
                        extensions: parts.extensions.clone(),
                        explicit_client_model,
                        client_profile,
                    })?;

                    if decision.policy_checked {
                        parts.extensions.insert(UnifiedModelPolicyChecked);
                    }
                    if decision.model_body_passthrough {
                        parts.extensions.insert(UnifiedModelBodyPassthrough);
                    }

                    let provider = decision.selected_provider;
                    let out_body = decision.out_body;
                    parts.extensions.insert(provider.clone());
                    parts.extensions.insert(*this.debug_log_config);
                    let request = Request::from_parts(
                        parts,
                        axum_core::body::Body::from(out_body),
                    );
                    this.state.set(State::InitProxy {
                        request: Some(request),
                        provider,
                    });
                }
                StateProj::InitProxy { request, provider } => {
                    let mut request =
                        request.take().expect("future polled after completion");
                    let mut direct_proxy = if let Some(p) =
                        this.direct_proxies.get(provider).cloned()
                    {
                        p
                    } else {
                        let custom_fallback = request
                            .extensions()
                            .get::<AuthContext>()
                            .is_some_and(|auth| {
                                auth.is_custom_provider
                                    && auth.master_key_base_url.is_some()
                            });
                        if !custom_fallback {
                            tracing::warn!(
                                provider = %provider,
                                "requested provider is not configured for direct proxy"
                            );
                            return Poll::Ready(Err(
                                InvalidRequestError::UnsupportedProvider(
                                    provider.clone(),
                                )
                                .into(),
                            ));
                        }
                        let auth = request
                            .extensions()
                            .get::<AuthContext>()
                            .expect("custom_fallback implies AuthContext");
                        tracing::debug!(
                            parsed_provider = %provider,
                            master_key_id = ?auth.master_key_id,
                            vk_prefix = %auth.virtual_key_prefix,
                            "unified_api: direct proxy miss for parsed provider; \
                             falling back via master_key base_url",
                        );
                        request
                            .extensions_mut()
                            .insert(MasterKeyUnifiedModelPassthrough);
                        let carrier = this
                            .direct_proxies
                            .get(&InferenceProvider::Custom)
                            .cloned()
                            .map(|p| ("custom", p))
                            .or_else(|| {
                                this.direct_proxies
                                    .get(&InferenceProvider::OpenAI)
                                    .cloned()
                                    .map(|p| ("openai", p))
                            });
                        let Some((carrier_name, proxy)) = carrier else {
                            tracing::warn!(
                                parsed_provider = %provider,
                                "unified_api: custom master_key base_url set but \
                                 neither Custom nor OpenAI direct proxy stack exists"
                            );
                            return Poll::Ready(Err(
                                InvalidRequestError::UnsupportedProvider(
                                    provider.clone(),
                                )
                                .into(),
                            ));
                        };
                        tracing::debug!(fallback_carrier = carrier_name);
                        proxy
                    };
                    let response_future = direct_proxy.call(request);
                    this.state.set(State::Proxy { response_future });
                }
                StateProj::Proxy { response_future } => {
                    let response =
                        ready!(response_future.poll(cx)).map_err(|_| {
                            tracing::error!(
                                "encountered error from what should be \
                                 infallible service"
                            );
                            InternalError::Internal
                        })?;
                    return Poll::Ready(Ok(response));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::implicit_default_model_fallback_context;

    #[test]
    fn implicit_default_context_only_applies_to_chat_completions() {
        let body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let ctx =
            implicit_default_model_fallback_context("chat/completions", &body)
                .expect("chat completions should produce fallback context");
        assert_eq!(ctx.selected_model, "openai/gpt-5.4");

        assert!(
            implicit_default_model_fallback_context("responses", &body)
                .is_none()
        );
        assert!(
            implicit_default_model_fallback_context("embeddings", &body)
                .is_none()
        );
    }

    #[test]
    fn implicit_default_context_requires_model_in_body() {
        let body = Bytes::from(
            serde_json::json!({
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        assert!(
            implicit_default_model_fallback_context("chat/completions", &body)
                .is_none()
        );
    }
}
