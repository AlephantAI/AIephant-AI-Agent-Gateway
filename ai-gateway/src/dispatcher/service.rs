use std::{
    collections::HashMap,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use backon::{BackoffBuilder, ConstantBuilder, ExponentialBuilder, Retryable};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, uri::PathAndQuery};
use http_body_util::BodyExt;
use opentelemetry::KeyValue;
use reqwest::RequestBuilder;
use rust_decimal::prelude::ToPrimitive;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use tower::{Service, ServiceBuilder};
use tracing::{Instrument, info_span};
use uuid::Uuid;

#[cfg(test)]
use crate::dispatcher::provider_allowlist::allowlist_workspace_id_for_request;
use crate::{
    app_state::AppState,
    config::{fallback_bridge, retry::RetryConfig, router::RouterConfig},
    discover::monitor::metrics::EndpointMetricsRegistry,
    dispatcher::{
        cache_coordinator::{
            build_llm_cache_hit_response, llm_kv_slot_keys,
            llm_kv_write_slot_keys, semantic_write_body_bytes,
        },
        client::Client,
        dispatch_logger::{DispatchLogRequest, DispatchLogger},
        extensions::ExtensionsCopier,
        fallback_executor::{CrossProviderFallbackRequest, FallbackExecutor},
        provider_allowlist::enforce_workspace_provider_allowlist,
        regional_retry_executor::{
            RegionalRetryExecutor, RegionalRetryRequest,
        },
        request_builder::request_builder_with_effective_host,
        sync_dispatch::{self, SyncDispatchResponse},
        target_endpoint::{
            TargetEndpoint, TargetEndpointRequest, TargetEndpointResolver,
            TargetEndpointSource,
        },
        upstream_auth::{UpstreamAuthApplier, UpstreamAuthRequest},
    },
    endpoints::ApiEndpoint,
    error::{
        api::ApiError, init::InitError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    logger::service::LoggerService,
    middleware::{
        add_extension::{AddExtensions, AddExtensionsLayer},
        mapper::{model::ModelMapper, registry::EndpointConverterRegistry},
        prompt_compression,
    },
    session_headers::{SessionHeaders, parse_session_headers},
    types::{
        body::{BodyReader, TfftTrigger},
        extensions::{
            LargeContextDecision, MapperContext, MapperProfileContext,
            PromptCompressionTokenPair, PromptContext,
            PromptHeaderForRequestLog, RequestContext, RequestKind,
            RequestLogEmitted, UnifiedImplicitModelFallbackContext, VkPolicy,
        },
        model_id::ModelId,
        provider::InferenceProvider,
        rate_limit::RateLimitEvent,
        request::Request,
        router::RouterId,
    },
    utils::{
        debug_log::DebugLogConfig,
        handle_error::{ErrorHandler, ErrorHandlerLayer},
    },
    virtual_key::enforce::check_model_access,
};

pub type DispatcherFuture = BoxFuture<
    'static,
    Result<http::Response<crate::types::body::Body>, ApiError>,
>;
pub type DispatcherService =
    AddExtensions<ErrorHandler<crate::middleware::mapper::Service<Dispatcher>>>;

/// Leaf service that dispatches requests to the correct provider.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    client: Client,
    app_state: AppState,
    provider: InferenceProvider,
    /// Is `Some` for load balanced routers, `None` for direct proxies.
    rate_limit_tx: Option<mpsc::Sender<RateLimitEvent>>,
}

struct SyncDispatchOutcome {
    response: http::Response<crate::types::body::Body>,
    response_body_for_logger: crate::types::body::BodyReader,
    tfft_rx: oneshot::Receiver<()>,
    effective_provider: InferenceProvider,
    effective_target_url: url::Url,
    effective_request_body: Bytes,
}

struct GatewayRequestContext {
    mapper_ctx: MapperContext,
    req_ctx: Arc<RequestContext>,
    api_endpoint: Option<ApiEndpoint>,
    extracted_path_and_query: PathAndQuery,
    inference_provider: InferenceProvider,
    router_id: Option<RouterId>,
    mapper_profile_context: Option<MapperProfileContext>,
    start_instant: Instant,
    start_time: DateTime<Utc>,
    request_kind: RequestKind,
    prompt_ctx: Option<PromptContext>,
    prompt_header_from_mapper: Option<PromptHeaderForRequestLog>,
    large_context_decision: Option<LargeContextDecision>,
    prompt_compression_tokens: Option<PromptCompressionTokenPair>,
}

impl Dispatcher {
    async fn new_inner(
        app_state: AppState,
        router_id: &RouterId,
        provider: InferenceProvider,
        model_mapper: ModelMapper,
    ) -> Result<DispatcherService, InitError> {
        let client = Client::new(&app_state, provider.clone()).await?;
        let rate_limit_tx = app_state.get_rate_limit_tx(router_id).await?;

        let dispatcher = Self {
            client,
            app_state: app_state.clone(),
            provider: provider.clone(),
            rate_limit_tx: Some(rate_limit_tx),
        };
        let converter_registry = EndpointConverterRegistry::new(&model_mapper);

        let extensions_layer = AddExtensionsLayer::builder()
            .inference_provider(provider.clone())
            .router_id(Some(router_id.clone()))
            .build();

        Ok(ServiceBuilder::new()
            .layer(extensions_layer)
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(crate::middleware::mapper::Layer::new(
                converter_registry,
                app_state.clone(),
            ))
            // other middleware: rate limiting, logging, etc, etc
            // will be added here as well
            .service(dispatcher))
    }

    pub async fn new(
        app_state: AppState,
        router_id: &RouterId,
        router_config: &Arc<RouterConfig>,
        provider: InferenceProvider,
    ) -> Result<DispatcherService, InitError> {
        let model_mapper = ModelMapper::new_for_router(
            app_state.clone(),
            router_config.clone(),
        );
        Self::new_inner(app_state, router_id, provider, model_mapper).await
    }

    pub async fn new_with_model_id(
        app_state: AppState,
        router_id: &RouterId,
        router_config: &Arc<RouterConfig>,
        provider: InferenceProvider,
        model_id: ModelId,
    ) -> Result<DispatcherService, InitError> {
        let model_mapper = ModelMapper::new_with_model_id(
            app_state.clone(),
            router_config.clone(),
            model_id,
        );
        Self::new_inner(app_state, router_id, provider, model_mapper).await
    }

    pub async fn new_direct_proxy(
        app_state: AppState,
        provider: &InferenceProvider,
    ) -> Result<DispatcherService, InitError> {
        let client = Client::new(&app_state, provider.clone()).await?;

        let dispatcher = Self {
            client,
            app_state: app_state.clone(),
            provider: provider.clone(),
            rate_limit_tx: None,
        };
        let model_mapper = ModelMapper::new(app_state.clone());
        let converter_registry = EndpointConverterRegistry::new(&model_mapper);

        let extensions_layer = AddExtensionsLayer::builder()
            .inference_provider(provider.clone())
            .router_id(None)
            .build();

        Ok(ServiceBuilder::new()
            .layer(extensions_layer)
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(crate::middleware::mapper::Layer::new(
                converter_registry,
                app_state.clone(),
            ))
            // other middleware: rate limiting, logging, etc, etc
            // will be added here as well
            .service(dispatcher))
    }
}

impl Service<Request> for Dispatcher {
    type Response = http::Response<crate::types::body::Body>;
    type Error = ApiError;
    type Future = DispatcherFuture;

    fn poll_ready(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[tracing::instrument(name = "dispatcher", skip_all)]
    fn call(&mut self, req: Request) -> Self::Future {
        // see: https://docs.rs/tower/latest/tower/trait.Service.html#be-careful-when-cloning-inner-services
        let this = self.clone();
        let this = std::mem::replace(self, this);
        tracing::trace!(provider = ?this.provider, "dispatcher received request");
        Box::pin(async move { this.dispatch(req).await })
    }
}

impl Dispatcher {
    #[allow(clippy::too_many_lines)]
    async fn dispatch(
        &self,
        mut req: Request,
    ) -> Result<http::Response<crate::types::body::Body>, ApiError> {
        // Extract request context and extensions
        let GatewayRequestContext {
            mapper_ctx,
            req_ctx,
            api_endpoint,
            extracted_path_and_query,
            inference_provider,
            router_id,
            mapper_profile_context,
            start_instant,
            start_time,
            request_kind,
            prompt_ctx,
            prompt_header_from_mapper,
            large_context_decision,
            mut prompt_compression_tokens,
        } = Self::extract_request_context(&mut req)?;

        let debug_log_config = req
            .extensions_mut()
            .remove::<DebugLogConfig>()
            .unwrap_or_else(|| DebugLogConfig::from_headers(req.headers_mut()));
        let auth_ctx = req_ctx.auth_context.as_ref();
        let target_provider = &self.provider;

        let provider_for_allowlist = if auth_ctx.is_some_and(|a| {
            a.is_custom_provider && a.master_key_base_url.is_some()
        }) {
            InferenceProvider::Custom
        } else {
            target_provider.clone()
        };
        enforce_workspace_provider_allowlist(
            &self.app_state,
            auth_ctx,
            &provider_for_allowlist,
        )?;

        let headers_for_llm_cache = req.headers().clone();
        let session_ctx = parse_session_headers(req.headers())
            .map_err(ApiError::InvalidRequest)?;

        UpstreamAuthApplier::sanitize_headers(req.headers_mut());
        let method = req.method().clone();
        let headers = req.headers().clone();
        let mut extensions_snapshot = req.extensions().clone();
        let log_emitted_marker =
            extensions_snapshot.get::<RequestLogEmitted>().cloned();
        let vk_policy = req.extensions().get::<VkPolicy>().cloned();
        let implicit_model_fallback_ctx = req
            .extensions()
            .get::<UnifiedImplicitModelFallbackContext>()
            .cloned();
        let target_endpoint =
            TargetEndpointResolver::new(self.app_state.clone())
                .resolve(TargetEndpointRequest {
                    request_context: &req_ctx,
                    target_provider,
                    path_and_query: extracted_path_and_query.as_str(),
                    allow_learned_region: !mapper_ctx.is_stream,
                })
                .await?;
        let target_url = target_endpoint.url.clone();
        // TODO: could change request type of dispatcher to
        // http::Request<reqwest::Body>
        // to avoid collecting the body twice
        let mut req_body_bytes = req
            .into_body()
            .collect()
            .await
            .map_err(|e| InternalError::RequestBodyError(Box::new(e)))?
            .to_bytes();

        let direct_proxy_prompt_log: Option<PromptHeaderForRequestLog> = if matches!(
            request_kind,
            RequestKind::DirectProxy | RequestKind::CustomProvider
        ) {
            let workspace_id =
                auth_ctx.map(|a| a.org_id.to_string()).unwrap_or_default();
            let (b, pl) =
                    crate::content_filter::prompt_cache::merge_prompt_cache_messages_into_body(
                        self.app_state.redis(),
                        &headers,
                        &workspace_id,
                        req_body_bytes,
                        &self.app_state.0.metrics.vk,
                    )
                    .await?;
            req_body_bytes = b;
            let prompt_log = pl;

            let filter_result =
                match crate::content_filter::evaluate::evaluate_for_vk_request(
                    &self.app_state,
                    &headers,
                    &extensions_snapshot,
                    &req_body_bytes,
                )
                .await
                {
                    Ok(r) => r,
                    Err(ApiError::InvalidRequest(
                        InvalidRequestError::ContentPolicyDenied {
                            ref message,
                        },
                    )) => {
                        self.emit_policy_deny_request_log(
                            &req_ctx,
                            start_time,
                            start_instant,
                            &mapper_ctx,
                            router_id.clone(),
                            &headers,
                            &req_body_bytes,
                            message,
                            prompt_ctx.clone(),
                            prompt_log.clone(),
                            session_ctx.clone(),
                            target_provider,
                            extracted_path_and_query.as_str(),
                            log_emitted_marker.as_ref(),
                        )
                        .await;
                        return Err(ApiError::InvalidRequest(
                            InvalidRequestError::ContentPolicyDenied {
                                message: message.clone(),
                            },
                        ));
                    }
                    Err(e) => return Err(e),
                };
            if let crate::content_filter::ContentFilterForwardBody::UseReplaced(
                    b,
                ) = filter_result.forward_body
                {
                    req_body_bytes = b;
                }
            if let Some(ref new_model) = filter_result.change_model {
                let (new_body, original) =
                    crate::content_filter::evaluate::apply_model_downgrade(
                        req_body_bytes,
                        new_model,
                    );
                req_body_bytes = new_body;
                let original_model = original.unwrap_or_default();
                tracing::info!(
                    original_model = %original_model,
                    downgraded_model = %new_model,
                    "content_filter: policy model downgrade applied (direct proxy)"
                );
                extensions_snapshot.insert(
                    crate::content_filter::PolicyModelOverride {
                        original_model,
                        downgraded_model: new_model.clone(),
                    },
                );
            }

            if extracted_path_and_query
                .path()
                .ends_with("chat/completions")
            {
                let mut fake_parts = http::Request::new(()).into_parts().0;
                fake_parts.headers = headers.clone();
                req_body_bytes = prompt_compression::apply_chat_completions(
                    &mut fake_parts,
                    req_body_bytes,
                    target_provider,
                )?;
                if let Some(pair) =
                    fake_parts.extensions.remove::<PromptCompressionTokenPair>()
                {
                    prompt_compression_tokens = Some(pair);
                }
            }
            prompt_log
        } else {
            None
        };

        let prompt_for_request_log: Option<PromptHeaderForRequestLog> = if matches!(
            request_kind,
            RequestKind::DirectProxy | RequestKind::CustomProvider
        ) {
            direct_proxy_prompt_log
        } else {
            prompt_header_from_mapper
        };

        enforce_direct_proxy_vk_model_policy(
            &self.app_state,
            vk_policy.as_ref(),
            request_kind,
            target_provider,
            extracted_path_and_query.as_str(),
            &req_body_bytes,
            &extensions_snapshot,
            auth_ctx,
        )?;

        let llm_cache_settings =
            match alephant_llm_kv_cache::CacheSettings::parse(
                &headers_for_llm_cache,
            ) {
                Err(msg) => {
                    tracing::error!(%msg, "llm kv: invalid Alephant-Cache-* headers");
                    return Err(ApiError::Internal(InternalError::Internal));
                }
                Ok(s) => s,
            };

        let llm_kv_read_ok = llm_cache_settings.should_read
            && auth_ctx.is_some()
            && req_ctx.llm_kv_cache_read_allowed;

        let cache_read_keys =
            if llm_kv_read_ok || llm_cache_settings.should_write {
                Some(llm_kv_slot_keys(
                    &llm_cache_settings,
                    &target_url,
                    &req_body_bytes,
                ))
            } else {
                None
            };

        if llm_kv_read_ok
            && let Some(ref keys) = cache_read_keys
            && let Some((entry, bidx)) = alephant_llm_kv_cache::read_bucket(
                self.app_state.llm_kv().as_ref(),
                keys,
            )
            .await
        {
            let (mut hit_resp, body_reader, tfft_rx) =
                build_llm_cache_hit_response(&entry, bidx, &mapper_ctx)?;
            let response_received_at = Utc::now();
            tracing::info!(
                method = %method,
                target_url = %target_url,
                is_stream = %mapper_ctx.is_stream,
                response_status = %hit_resp.status(),
                "llm kv cache hit"
            );
            let response_log_id = Uuid::new_v4();
            let provider_request_id = {
                let h = hit_resp.headers_mut();
                h.insert(
                    "alephant-id",
                    HeaderValue::from_str(&response_log_id.to_string())
                        .expect("a uuid is always a valid header value"),
                );
                h.remove(http::header::CONTENT_LENGTH);
                h.remove("x-request-id")
            };
            let extensions_copier = ExtensionsCopier::builder()
                .inference_provider(inference_provider.clone())
                .router_id(router_id.clone())
                .auth_context(auth_ctx.cloned())
                .provider_request_id(provider_request_id)
                .mapper_ctx(mapper_ctx.clone())
                .mapper_profile_context(mapper_profile_context.clone())
                .build();
            extensions_copier.copy_extensions(hit_resp.extensions_mut());
            hit_resp.extensions_mut().insert(mapper_ctx.clone());
            if let Some(ref ep) = api_endpoint {
                hit_resp.extensions_mut().insert(ep.clone());
            }
            hit_resp
                .extensions_mut()
                .insert(extracted_path_and_query.clone());
            let llm_kv_cache_key = keys.get(bidx).cloned();
            DispatchLogger::new(self.app_state.clone()).handle(
                DispatchLogRequest {
                    req_ctx: &req_ctx,
                    start_time,
                    start_instant,
                    target_url: target_url.clone(),
                    headers: headers.clone(),
                    req_body_bytes: req_body_bytes.clone(),
                    client_response: &hit_resp,
                    response_body_for_logger: body_reader,
                    tfft_rx,
                    mapper_ctx: &mapper_ctx,
                    router_id: router_id.clone(),
                    request_log_id: request_log_id_from_headers(&headers),
                    response_log_id,
                    response_received_at,
                    prompt_ctx: prompt_ctx.clone(),
                    prompt_header_for_request_log: prompt_for_request_log
                        .clone(),
                    large_context_decision: large_context_decision.clone(),
                    prompt_compression_tokens,
                    session_ctx: session_ctx.clone(),
                    ai_gateway_body_mapping: None,
                    cache_reference_id: llm_kv_cache_key,
                    llm_kv_cache_read_enabled: true,
                    effective_provider: target_provider,
                    log_emitted: log_emitted_marker.as_ref(),
                    debug_log_config,
                },
            );
            return Ok(hit_resp);
        }

        let semantic_prepared = if llm_kv_read_ok {
            if let Some(semantic_cache) = self.app_state.semantic_cache() {
                match semantic_cache.prepare_request(
                    extracted_path_and_query.as_str(),
                    &headers_for_llm_cache,
                    &req_body_bytes,
                ) {
                    Ok(prepared) => prepared,
                    Err(err) => {
                        tracing::warn!(%err, "semantic cache bypassed");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut semantic_write_context = None;
        if llm_kv_read_ok
            && let Some(semantic_cache) = self.app_state.semantic_cache()
            && let Some(prepared) = semantic_prepared.as_ref()
        {
            match semantic_cache.try_hit_prepared(prepared).await {
                Ok(outcome) => {
                    semantic_write_context = outcome.write;
                    if let Some(hit) = outcome.hit {
                        let entry = alephant_llm_kv_cache::LlmCacheEntry {
                            headers: std::collections::HashMap::new(),
                            latency: 0,
                            body: vec![
                                String::from_utf8_lossy(&hit.response_bytes)
                                    .to_string(),
                            ],
                        };
                        let (mut hit_resp, body_reader, tfft_rx) =
                            build_llm_cache_hit_response(
                                &entry,
                                0,
                                &mapper_ctx,
                            )?;
                        let response_received_at = Utc::now();
                        tracing::info!(
                            method = %method,
                            target_url = %target_url,
                            is_stream = %mapper_ctx.is_stream,
                            response_status = %hit_resp.status(),
                            cache_reference_id = %hit.cache_reference_id,
                            "semantic cache hit"
                        );
                        let response_log_id = Uuid::new_v4();
                        let provider_request_id = {
                            let h = hit_resp.headers_mut();
                            h.insert(
                                "alephant-id",
                                HeaderValue::from_str(
                                    &response_log_id.to_string(),
                                )
                                .expect(
                                    "a uuid is always a valid header value",
                                ),
                            );
                            h.remove(http::header::CONTENT_LENGTH);
                            h.remove("x-request-id")
                        };
                        let extensions_copier = ExtensionsCopier::builder()
                            .inference_provider(inference_provider.clone())
                            .router_id(router_id.clone())
                            .auth_context(auth_ctx.cloned())
                            .provider_request_id(provider_request_id)
                            .mapper_ctx(mapper_ctx.clone())
                            .mapper_profile_context(
                                mapper_profile_context.clone(),
                            )
                            .build();
                        extensions_copier
                            .copy_extensions(hit_resp.extensions_mut());
                        hit_resp.extensions_mut().insert(mapper_ctx.clone());
                        if let Some(ref ep) = api_endpoint {
                            hit_resp.extensions_mut().insert(ep.clone());
                        }
                        hit_resp
                            .extensions_mut()
                            .insert(extracted_path_and_query.clone());
                        DispatchLogger::new(self.app_state.clone()).handle(
                            DispatchLogRequest {
                                req_ctx: &req_ctx,
                                start_time,
                                start_instant,
                                target_url: target_url.clone(),
                                headers: headers.clone(),
                                req_body_bytes: req_body_bytes.clone(),
                                client_response: &hit_resp,
                                response_body_for_logger: body_reader,
                                tfft_rx,
                                mapper_ctx: &mapper_ctx,
                                router_id: router_id.clone(),
                                request_log_id: request_log_id_from_headers(
                                    &headers,
                                ),
                                response_log_id,
                                response_received_at,
                                prompt_ctx: prompt_ctx.clone(),
                                prompt_header_for_request_log:
                                    prompt_for_request_log.clone(),
                                large_context_decision: large_context_decision
                                    .clone(),
                                prompt_compression_tokens,
                                session_ctx: session_ctx.clone(),
                                ai_gateway_body_mapping: None,
                                cache_reference_id: Some(
                                    hit.cache_reference_id,
                                ),
                                llm_kv_cache_read_enabled: true,
                                effective_provider: target_provider,
                                log_emitted: log_emitted_marker.as_ref(),
                                debug_log_config,
                            },
                        );
                        return Ok(hit_resp);
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "semantic cache bypassed");
                }
            }
        }

        let llm_kv_write_enabled = llm_cache_settings.should_write
            && auth_ctx.is_some()
            && req_ctx.llm_kv_cache_write_allowed;
        let semantic_write_enabled = self.app_state.semantic_cache().is_some();
        let (cache_tap_tx, cache_save_rx) =
            if llm_kv_write_enabled || semantic_write_enabled {
                let (tx, rx) = mpsc::unbounded_channel();
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };

        let request_builder = self
            .client
            .as_ref()
            .request(method.clone(), target_url.clone())
            .headers(headers.clone());
        let request_builder =
            request_builder_with_effective_host(request_builder, &target_url);

        let request_builder = UpstreamAuthApplier::new(&self.app_state)
            .apply(UpstreamAuthRequest {
                client: &self.client,
                request_builder,
                req_body_bytes: &req_body_bytes,
                auth_context: auth_ctx,
                provider: self.provider.clone(),
            })
            .await?;

        let request_log_id = request_log_id_from_headers(&headers);

        tracing::info!(
            target_url = %target_url,
            body_len = req_body_bytes.len(),
            "dispatcher forward"
        );

        let metrics_for_stream = self.app_state.0.endpoint_metrics.clone();
        if let Some(ref api_endpoint) = api_endpoint {
            let endpoint_metrics = self
                .app_state
                .0
                .endpoint_metrics
                .health_metrics(api_endpoint.clone())?;
            endpoint_metrics.incr_req_count();
        }

        let (
            mut client_response,
            response_body_for_logger,
            tfft_rx,
            effective_provider,
            effective_target_url,
            effective_request_body,
        ) = if mapper_ctx.is_stream {
            if target_endpoint.cn_retry_url.is_some()
                && !regional_endpoint_retry_enabled_for_streaming()
            {
                tracing::debug!(
                    provider = %self.provider,
                    "regional_endpoint_retry: streaming path skipped in phase one"
                );
            }
            let (response, body_reader, tfft_rx) = dispatch_stream_with_retry(
                &self.app_state,
                request_builder,
                req_body_bytes.clone(),
                api_endpoint.clone(),
                metrics_for_stream,
                &req_ctx,
                request_kind,
                cache_tap_tx.clone(),
            )
            .await?;
            (
                response,
                body_reader,
                tfft_rx,
                self.provider.clone(),
                target_url.clone(),
                req_body_bytes.clone(),
            )
        } else {
            let outcome = self
                .dispatch_sync_with_retry(
                    request_builder,
                    req_body_bytes.clone(),
                    &req_ctx,
                    request_kind,
                    cache_tap_tx,
                    &method,
                    &headers,
                    target_endpoint.clone(),
                    extracted_path_and_query.as_str(),
                    vk_policy.as_ref(),
                    implicit_model_fallback_ctx.as_ref(),
                )
                .instrument(info_span!("dispatch_sync"))
                .await?;
            (
                outcome.response,
                outcome.response_body_for_logger,
                outcome.tfft_rx,
                outcome.effective_provider,
                outcome.effective_target_url,
                outcome.effective_request_body,
            )
        };
        if llm_kv_read_ok {
            let h = client_response.headers_mut();
            let _ = h.insert(
                HeaderName::from_static("alephant-cache"),
                HeaderValue::from_static("MISS"),
            );
        }

        let cache_write_keys = if cache_save_rx.is_some()
            && llm_cache_settings.should_write
            && auth_ctx.is_some()
            && req_ctx.llm_kv_cache_write_allowed
        {
            Some(llm_kv_write_slot_keys(
                &llm_cache_settings,
                &effective_target_url,
                &effective_request_body,
            ))
        } else {
            None
        };

        if let Some(mut rx) = cache_save_rx {
            let backend = self.app_state.llm_kv().clone();
            let ttl = llm_cache_settings.expiration_ttl_secs();
            let start = start_instant;
            let status = client_response.status();
            let resp_hdrs = client_response.headers().clone();
            let llm_kv_keys = cache_write_keys;
            let semantic_cache = self.app_state.semantic_cache().cloned();
            let semantic_prepared_for_write = semantic_prepared.clone();
            let semantic_write_context_for_write =
                semantic_write_context.clone();
            let semantic_path = extracted_path_and_query.to_string();
            let semantic_headers = headers_for_llm_cache.clone();
            let semantic_body = semantic_write_body_bytes(
                &req_body_bytes,
                &effective_request_body,
            );
            tokio::spawn(async move {
                if !status.is_success() {
                    return;
                }
                let mut body_chunks = Vec::new();
                let mut body_bytes = Vec::new();
                while let Some(b) = rx.recv().await {
                    body_bytes.extend_from_slice(&b);
                    body_chunks.push(String::from_utf8_lossy(&b).into_owned());
                }
                if llm_kv_write_enabled && let Some(keys) = llm_kv_keys {
                    let mut headers_json = HashMap::new();
                    for (name, val) in &resp_hdrs {
                        if let Ok(vs) = val.to_str() {
                            headers_json
                                .insert(name.to_string(), vs.to_string());
                        }
                    }
                    let entry = alephant_llm_kv_cache::LlmCacheEntry {
                        headers: headers_json,
                        latency: u64::try_from(start.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                        body: body_chunks,
                    };
                    let _ = alephant_llm_kv_cache::try_save_to_first_free_slot(
                        backend.as_ref(),
                        &keys,
                        &entry,
                        ttl,
                    )
                    .await;
                }
                if let Some(svc) = semantic_cache {
                    let store_result = if let Some(write) =
                        semantic_write_context_for_write
                    {
                        svc.store_response_with_context(write, &body_bytes)
                            .await
                    } else if let Some(prepared) =
                        semantic_prepared_for_write.as_ref()
                    {
                        svc.store_response_prepared(prepared, &body_bytes).await
                    } else {
                        svc.store_response(
                            &semantic_path,
                            &semantic_headers,
                            &semantic_body,
                            &body_bytes,
                        )
                        .await
                    };
                    if let Err(err) = store_result {
                        tracing::warn!(%err, "semantic cache store bypassed");
                    }
                }
            });
        }

        let response_received_at = Utc::now();
        tracing::info!(
            method = %method,
            target_url = %effective_target_url,
            is_stream = %mapper_ctx.is_stream,
            response_status = %client_response.status(),
            "proxied request"
        );
        let response_log_id = Uuid::new_v4();
        let provider_request_id = {
            let headers = client_response.headers_mut();
            headers.insert(
                "alephant-id",
                HeaderValue::from_str(&response_log_id.to_string())
                    .expect("a uuid is always a valid header value"),
            );
            headers.remove(http::header::CONTENT_LENGTH);
            headers.remove("x-request-id")
        };
        tracing::debug!(provider_req_id = ?provider_request_id, status = %client_response.status(), "received response");
        let extensions_copier = ExtensionsCopier::builder()
            .inference_provider(effective_provider.clone())
            .router_id(router_id.clone())
            .auth_context(auth_ctx.cloned())
            .provider_request_id(provider_request_id)
            .mapper_ctx(mapper_ctx.clone())
            .mapper_profile_context(mapper_profile_context)
            .build();
        extensions_copier.copy_extensions(client_response.extensions_mut());
        client_response.extensions_mut().insert(mapper_ctx.clone());
        if let Some(api_endpoint) = api_endpoint.clone() {
            client_response.extensions_mut().insert(api_endpoint);
        }
        client_response
            .extensions_mut()
            .insert(extracted_path_and_query);

        let response_status = client_response.status();
        let response_headers = client_response.headers();
        self.handle_error_and_rate_limiting(
            response_status,
            response_headers,
            api_endpoint.clone(),
            &effective_provider,
        )
        .await?;

        // Handle logging
        DispatchLogger::new(self.app_state.clone()).handle(
            DispatchLogRequest {
                req_ctx: &req_ctx,
                start_time,
                start_instant,
                target_url: effective_target_url,
                headers,
                req_body_bytes: effective_request_body,
                client_response: &client_response,
                response_body_for_logger,
                tfft_rx,
                mapper_ctx: &mapper_ctx,
                router_id,
                request_log_id,
                response_log_id,
                response_received_at,
                prompt_ctx,
                prompt_header_for_request_log: prompt_for_request_log,
                large_context_decision,
                prompt_compression_tokens,
                session_ctx,
                ai_gateway_body_mapping: None,
                cache_reference_id: None,
                llm_kv_cache_read_enabled: llm_kv_read_ok,
                effective_provider: &effective_provider,
                log_emitted: log_emitted_marker.as_ref(),
                debug_log_config,
            },
        );

        Ok(client_response)
    }

    // ... existing methods ...

    /// Extracts request context and extensions from the request
    fn extract_request_context(
        req: &mut Request,
    ) -> Result<GatewayRequestContext, ApiError> {
        let mapper_ctx = req
            .extensions_mut()
            .remove::<MapperContext>()
            .ok_or(InternalError::ExtensionNotFound("MapperContext"))?;
        let req_ctx = req
            .extensions_mut()
            .remove::<Arc<RequestContext>>()
            .ok_or(InternalError::ExtensionNotFound("RequestContext"))?;
        let api_endpoint = req.extensions().get::<ApiEndpoint>().cloned();
        let extracted_path_and_query = req
            .extensions_mut()
            .remove::<PathAndQuery>()
            .ok_or(ApiError::Internal(InternalError::ExtensionNotFound(
                "PathAndQuery",
            )))?;
        let inference_provider = req
            .extensions()
            .get::<InferenceProvider>()
            .cloned()
            .ok_or(InternalError::ExtensionNotFound("InferenceProvider"))?;
        let router_id = req.extensions().get::<RouterId>().cloned();
        let mapper_profile_context =
            req.extensions_mut().remove::<MapperProfileContext>();
        let start_instant = req
            .extensions()
            .get::<Instant>()
            .copied()
            .unwrap_or_else(|| {
                tracing::warn!(
                    "did not find expected Instant in req extensions"
                );
                Instant::now()
            });
        let start_time = req
            .extensions()
            .get::<DateTime<Utc>>()
            .copied()
            .unwrap_or_else(|| {
                tracing::warn!(
                    "did not find expected DateTime<Utc> in req extensions"
                );
                Utc::now()
            });
        let request_kind = req
            .extensions()
            .get::<RequestKind>()
            .copied()
            .ok_or(InternalError::ExtensionNotFound("RequestKind"))?;
        let prompt_ctx = req.extensions_mut().remove::<PromptContext>();
        let prompt_header_from_mapper =
            req.extensions_mut().remove::<PromptHeaderForRequestLog>();
        let large_context_decision =
            req.extensions_mut().remove::<LargeContextDecision>();
        let prompt_compression_tokens =
            req.extensions_mut().remove::<PromptCompressionTokenPair>();

        Ok(GatewayRequestContext {
            mapper_ctx,
            req_ctx,
            api_endpoint,
            extracted_path_and_query,
            inference_provider,
            router_id,
            mapper_profile_context,
            start_instant,
            start_time,
            request_kind,
            prompt_ctx,
            prompt_header_from_mapper,
            large_context_decision,
            prompt_compression_tokens,
        })
    }

    /// Handles error responses and rate limiting
    async fn handle_error_and_rate_limiting(
        &self,
        response_status: StatusCode,
        response_headers: &HeaderMap,
        api_endpoint: Option<ApiEndpoint>,
        effective_provider: &InferenceProvider,
    ) -> Result<(), ApiError> {
        if response_status.is_server_error() {
            if let Some(api_endpoint) = api_endpoint {
                let endpoint_metrics = self
                    .app_state
                    .0
                    .endpoint_metrics
                    .health_metrics(api_endpoint)?;
                endpoint_metrics.incr_remote_internal_error_count();
            }
        } else if response_status == StatusCode::TOO_MANY_REQUESTS
            && let Some(ref api_endpoint) = api_endpoint
        {
            let retry_after = extract_retry_after(response_headers);
            tracing::info!(
                provider = ?effective_provider,
                api_endpoint = ?api_endpoint,
                retry_after = ?retry_after,
                "Provider rate limited, signaling monitor"
            );

            if let Some(rate_limit_tx) = &self.rate_limit_tx
                && let Err(e) = rate_limit_tx
                    .send(RateLimitEvent::new(
                        api_endpoint.clone(),
                        retry_after,
                    ))
                    .await
            {
                tracing::error!(error = %e, "failed to send rate limit event");
            }
        }
        Ok(())
    }
}

impl Dispatcher {
    #[allow(clippy::too_many_arguments)]
    async fn emit_policy_deny_request_log(
        &self,
        req_ctx: &RequestContext,
        start_time: DateTime<Utc>,
        start_instant: Instant,
        mapper_ctx: &MapperContext,
        router_id: Option<RouterId>,
        headers: &HeaderMap,
        req_body_bytes: &Bytes,
        deny_message: &str,
        prompt_ctx: Option<PromptContext>,
        prompt_header_for_request_log: Option<PromptHeaderForRequestLog>,
        session_ctx: Option<SessionHeaders>,
        target_provider: &InferenceProvider,
        extracted_path_and_query: &str,
        log_emitted: Option<&RequestLogEmitted>,
    ) {
        let deployment_target =
            self.app_state.config().deployment_target.clone();
        if !self.app_state.config().alephant.is_observability_enabled() {
            return;
        }
        let Some(auth_ctx) = req_ctx.auth_context.clone() else {
            return;
        };

        let target_url =
            match TargetEndpointResolver::new(self.app_state.clone())
                .resolve(TargetEndpointRequest {
                    request_context: req_ctx,
                    target_provider,
                    path_and_query: extracted_path_and_query,
                    allow_learned_region: false,
                })
                .await
            {
                Ok(endpoint) => endpoint.url,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "policy deny log: failed to build target_url, skipping"
                    );
                    return;
                }
            };

        let response_body_bytes =
            crate::content_filter::evaluate::policy_denied_error_response_json(
                deny_message,
            );

        let response_status = http::StatusCode::OK;
        let request_log_id = request_log_id_from_headers(headers);
        let response_log_id = Uuid::new_v4();
        let response_received_at = Utc::now();

        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(Bytes::from(response_body_bytes));
        drop(tx);

        let (tfft_tx_for_body, _unused_rx) = oneshot::channel();
        let body_reader = BodyReader::new(
            rx,
            tfft_tx_for_body,
            hyper::body::SizeHint::default(),
            false,
            TfftTrigger::Never,
        );
        let (tfft_tx_for_log, tfft_rx) = oneshot::channel();
        let _ = tfft_tx_for_log.send(());

        let response_logger = LoggerService::builder()
            .app_state(self.app_state.clone())
            .auth_ctx(auth_ctx)
            .start_time(start_time)
            .start_instant(start_instant)
            .target_url(target_url)
            .request_headers(headers.clone())
            .request_body(req_body_bytes.clone())
            .response_status(response_status)
            .response_body(body_reader)
            .provider(target_provider.clone())
            .tfft_rx(tfft_rx)
            .mapper_ctx(mapper_ctx.clone())
            .router_id(router_id)
            .deployment_target(deployment_target)
            .request_id(request_log_id)
            .response_id(response_log_id)
            .response_created_at(response_received_at)
            .prompt_ctx(prompt_ctx)
            .prompt_header_for_request_log(prompt_header_for_request_log)
            .session_ctx(session_ctx)
            .build();

        if let Some(marker) = log_emitted {
            marker.mark();
        }

        let app_state = self.app_state.clone();
        tokio::spawn(
            async move {
                if let Err(e) = response_logger.log().await {
                    let error_str = e.as_ref().to_string();
                    app_state
                        .0
                        .metrics
                        .error_count
                        .add(1, &[KeyValue::new("type", error_str)]);
                }
            }
            .instrument(tracing::Span::current()),
        );
    }

    /// We take a `&RequestBuilder` so that `dispatch_stream` implements `FnMut`
    /// so we can use the [`backon`] crate for retries.
    async fn dispatch_stream(
        request_builder: &RequestBuilder,
        req_body_bytes: Bytes,
        api_endpoint: Option<ApiEndpoint>,
        metrics_registry: EndpointMetricsRegistry,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> Result<
        (
            http::Response<crate::types::body::Body>,
            crate::types::body::BodyReader,
            oneshot::Receiver<()>,
        ),
        ApiError,
    > {
        let request_builder = request_builder.try_clone().ok_or_else(|| {
            // in theory, this should never happen, as we'll have already
            // collected the request body
            tracing::error!(
                "failed to clone request builder, cannot dispatch stream"
            );
            ApiError::Internal(InternalError::Internal)
        })?;
        let response_stream = Client::sse_stream(
            request_builder,
            req_body_bytes,
            api_endpoint,
            &metrics_registry,
        )
        .await?;
        let mut resp_builder = http::Response::builder();
        *resp_builder.headers_mut().unwrap() = stream_response_headers();
        resp_builder = resp_builder.status(StatusCode::OK);

        let (user_resp_body, body_reader, tfft_rx) = BodyReader::wrap_stream(
            response_stream,
            true,
            TfftTrigger::FirstModelToken,
            cache_tap,
        );

        let response = resp_builder
            .body(user_resp_body)
            .map_err(InternalError::HttpError)?;
        Ok((response, body_reader, tfft_rx))
    }

    #[allow(clippy::too_many_lines)]
    async fn dispatch_sync_with_retry(
        &self,
        request_builder: RequestBuilder,
        req_body_bytes: Bytes,
        req_ctx: &RequestContext,
        request_kind: RequestKind,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
        method: &http::Method,
        headers: &HeaderMap,
        target_endpoint: TargetEndpoint,
        extracted_path_and_query: &str,
        vk_policy: Option<&VkPolicy>,
        implicit_model_fallback_ctx: Option<
            &UnifiedImplicitModelFallbackContext,
        >,
    ) -> Result<SyncDispatchOutcome, ApiError> {
        let target_url = target_endpoint.url.clone();
        let mut effective_target_url = target_url.clone();
        let retry_config =
            get_retry_config(&self.app_state, request_kind, req_ctx);
        let fallback_policy_for_log =
            self.app_state.config().fallback_policy.clone();
        let provider_for_log = self.provider.clone();
        let fallback_cache_tap = cache_tap.clone();
        let retry_exhausted_before_fallback =
            retry_config.as_ref().is_some_and(|config| {
                retry_config_allows_retry_attempts(config.as_ref())
            });
        let mut result: Result<SyncDispatchResponse, ApiError> = if let Some(
            retry_config,
        ) =
            retry_config
        {
            match retry_config.as_ref() {
                RetryConfig::Exponential {
                    min_delay,
                    max_delay,
                    max_retries,
                    factor,
                } => {
                    let retry_strategy = ExponentialBuilder::default()
                        .with_max_delay(*max_delay)
                        .with_min_delay(*min_delay)
                        .with_max_times(usize::from(*max_retries))
                        .with_factor(factor.to_f32().unwrap_or(
                            crate::config::retry::DEFAULT_RETRY_FACTOR,
                        ))
                        .with_jitter()
                        .build();
                    let future_fn = || async {
                        let result = sync_dispatch::dispatch_sync(
                            &request_builder,
                            req_body_bytes.clone(),
                            cache_tap.clone(),
                        )
                        .await?;

                        Ok(result)
                    };

                    crate::utils::retry::RetryWithResult::new(
                        future_fn,
                        retry_strategy,
                    )
                    .when(|result: &Result<_, _>| match result {
                        Ok(response) => response.0.status().is_server_error(),
                        Err(e) => match e {
                            ApiError::Internal(InternalError::ReqwestError(
                                reqwest_error,
                            )) => reqwest_error.is_connect() || reqwest_error.status().is_some_and(|s| s.is_server_error()),
                            _ => false,
                        },
                    })
                    .notify(|result: &Result<_, _>, dur: Duration| match result {
                        Ok(result) if result.0.status().is_server_error() => {
                                tracing::warn!(
                                    error = %result.0.status(),
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                        }
                        Err(ApiError::Internal(InternalError::ReqwestError(
                            reqwest_error,
                        ))) if reqwest_error.is_connect() || reqwest_error.status().is_some_and(|s| s.is_server_error()) => {
                                tracing::warn!(
                                    error = %reqwest_error,
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                            }
                        _ => {}
                    })
                    .await
                }
                RetryConfig::Constant { delay, max_retries } => {
                    let retry_strategy = ConstantBuilder::default()
                        .with_delay(*delay)
                        .with_max_times(usize::from(*max_retries))
                        .with_jitter()
                        .build();
                    let future_fn = || async {
                        sync_dispatch::dispatch_sync(
                            &request_builder,
                            req_body_bytes.clone(),
                            cache_tap.clone(),
                        )
                        .await
                    };

                    crate::utils::retry::RetryWithResult::new(future_fn, retry_strategy)
                    .when(|result: &Result<_, _>| match result {
                        Ok(response) => response.0.status().is_server_error(),
                        Err(e) => match e {
                            ApiError::Internal(InternalError::ReqwestError(
                                reqwest_error,
                            )) => reqwest_error.is_connect() || reqwest_error.status().is_some_and(|s| s.is_server_error()),
                            _ => false,
                        },
                    })
                    .notify(|result: &Result<_, _>, dur: Duration| match result {
                        Ok(result) if result.0.status().is_server_error() => {
                                tracing::warn!(
                                    error = %result.0.status(),
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                        }
                        Err(ApiError::Internal(InternalError::ReqwestError(
                            reqwest_error,
                        ))) if reqwest_error.is_connect() || reqwest_error.status().is_some_and(|s| s.is_server_error()) => {
                                tracing::warn!(
                                    error = %reqwest_error,
                                    retry_in = ?dur,
                                    "got error dispatching sync request, retrying...",
                                );
                                crate::fallback::observability::log_decision(
                                    &fallback_policy_for_log,
                                    crate::fallback::observability::DecisionKind::Retry,
                                    None,
                                    &provider_for_log,
                                );
                            }
                        _ => {}
                    })
                    .await
                }
            }
        } else {
            sync_dispatch::dispatch_sync(
                &request_builder,
                req_body_bytes.clone(),
                cache_tap.clone(),
            )
            .await
        };

        let mut regional_retry_produced_response =
            target_endpoint_response_is_final(&target_endpoint, &result);
        if let Ok(response) = &result
            && should_attempt_regional_endpoint_retry(
                &target_endpoint,
                response.0.status(),
            )
        {
            let regional_retry_result = RegionalRetryExecutor::new(
                &self.app_state,
                &self.client,
                &self.provider,
            )
            .retry_once(RegionalRetryRequest {
                req_body_bytes: req_body_bytes.clone(),
                req_ctx,
                cache_tap,
                method,
                headers,
                target_endpoint: &target_endpoint,
            })
            .await;
            regional_retry_produced_response = apply_regional_retry_result(
                &mut result,
                &mut effective_target_url,
                target_endpoint.cn_retry_url.clone(),
                regional_retry_result,
                &self.provider,
                req_ctx
                    .auth_context
                    .as_ref()
                    .and_then(|auth| auth.master_key_id),
            );
        }

        if should_attempt_cross_provider_default_model_fallback(
            retry_exhausted_before_fallback,
            regional_retry_produced_response,
            request_kind,
            extracted_path_and_query,
            implicit_model_fallback_ctx,
            &result,
        ) {
            match FallbackExecutor::new(&self.app_state)
                .try_cross_provider_default_model_fallback(
                    CrossProviderFallbackRequest {
                        req_ctx,
                        method,
                        headers,
                        extracted_path_and_query,
                        vk_policy,
                        implicit_model_fallback_ctx,
                        req_body_bytes: &req_body_bytes,
                        cache_tap: fallback_cache_tap,
                    },
                )
                .await
            {
                Ok(Some(fallback_result)) => {
                    return Ok(SyncDispatchOutcome {
                        response: fallback_result.response,
                        response_body_for_logger: fallback_result
                            .response_body_for_logger,
                        tfft_rx: fallback_result.tfft_rx,
                        effective_provider: fallback_result.effective_provider,
                        effective_target_url: fallback_result
                            .effective_target_url,
                        effective_request_body: fallback_result
                            .effective_request_body,
                    });
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        path = %extracted_path_and_query,
                        provider = %self.provider,
                        "cross-provider fallback failed; returning original result"
                    );
                }
            }
        }

        let (response, response_body_for_logger, tfft_rx) = result?;
        Ok(SyncDispatchOutcome {
            response,
            response_body_for_logger,
            tfft_rx,
            effective_provider: self.provider.clone(),
            effective_target_url,
            effective_request_body: req_body_bytes,
        })
    }
}

fn sync_dispatch_result_is_retryable(
    result: &Result<SyncDispatchResponse, ApiError>,
) -> bool {
    match result {
        Ok(response) => response.0.status().is_server_error(),
        Err(ApiError::Internal(InternalError::ReqwestError(reqwest_error))) => {
            reqwest_error.is_connect()
                || reqwest_error
                    .status()
                    .is_some_and(|status| status.is_server_error())
        }
        _ => false,
    }
}

fn should_attempt_regional_endpoint_retry(
    endpoint: &TargetEndpoint,
    status: http::StatusCode,
) -> bool {
    matches!(endpoint.source, TargetEndpointSource::GlobalProviderBaseUrl)
        && endpoint.cn_retry_url.is_some()
        && crate::dispatcher::regional_endpoint::regional_retry_eligible_status(
            status,
        )
}

fn regional_endpoint_retry_enabled_for_streaming() -> bool {
    false
}

fn target_endpoint_response_is_final(
    endpoint: &TargetEndpoint,
    result: &Result<SyncDispatchResponse, ApiError>,
) -> bool {
    matches!(endpoint.source, TargetEndpointSource::LearnedCn) && result.is_ok()
}

fn apply_regional_retry_result(
    result: &mut Result<SyncDispatchResponse, ApiError>,
    effective_target_url: &mut url::Url,
    cn_retry_url: Option<url::Url>,
    regional_retry_result: Result<Option<SyncDispatchResponse>, ApiError>,
    provider: &InferenceProvider,
    master_key_id: Option<Uuid>,
) -> bool {
    match regional_retry_result {
        Ok(Some(regional_response)) => {
            // A CN HTTP response is the final attempted endpoint, even when
            // non-success. Internal retry errors stay best-effort and keep
            // the original response below.
            if let Some(cn_retry_url) = cn_retry_url {
                *effective_target_url = cn_retry_url;
            }
            *result = Ok(regional_response);
            true
        }
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(
                provider = %provider,
                master_key_id = ?master_key_id,
                error = %err,
                "regional_endpoint_retry: best-effort retry failed; returning original response"
            );
            false
        }
    }
}

fn should_attempt_cross_provider_default_model_fallback(
    retry_exhausted_before_fallback: bool,
    regional_retry_produced_response: bool,
    request_kind: RequestKind,
    extracted_path_and_query: &str,
    implicit_model_fallback_ctx: Option<&UnifiedImplicitModelFallbackContext>,
    result: &Result<SyncDispatchResponse, ApiError>,
) -> bool {
    retry_exhausted_before_fallback
        && !regional_retry_produced_response
        && matches!(request_kind, RequestKind::UnifiedApi)
        && unified_chat_completions_path(extracted_path_and_query)
        && implicit_model_fallback_ctx.is_some()
        && sync_dispatch_result_is_retryable(result)
}

fn retry_config_allows_retry_attempts(retry_config: &RetryConfig) -> bool {
    match retry_config {
        RetryConfig::Exponential { max_retries, .. }
        | RetryConfig::Constant { max_retries, .. } => *max_retries > 0,
    }
}

fn unified_chat_completions_path(extracted_path_and_query: &str) -> bool {
    extracted_path_and_query
        .split('?')
        .next()
        .is_some_and(|path| path.ends_with("chat/completions"))
}

fn enforce_direct_proxy_vk_model_policy(
    app_state: &AppState,
    vk_policy: Option<&VkPolicy>,
    request_kind: RequestKind,
    provider: &InferenceProvider,
    path_and_query: &str,
    req_body_bytes: &Bytes,
    extensions: &http::Extensions,
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
) -> Result<(), ApiError> {
    if extensions
        .get::<crate::content_filter::PolicyModelOverride>()
        .is_some()
    {
        return Ok(());
    }
    if auth_ctx.is_some_and(|a| {
        a.is_custom_provider && a.master_key_base_url.is_some()
    }) {
        return Ok(());
    }
    if !matches!(
        request_kind,
        RequestKind::DirectProxy | RequestKind::CustomProvider
    ) {
        return Ok(());
    }

    if provider != &InferenceProvider::OpenAI {
        return Ok(());
    }

    // Only OpenAI chat/completions has a stable source request schema with
    // model in body for this path.
    if !path_and_query.ends_with("chat/completions") {
        return Ok(());
    }

    let req = serde_json::from_slice::<
        async_openai::types::CreateChatCompletionRequest,
    >(req_body_bytes)
    .map_err(
        crate::error::invalid_req::InvalidRequestError::InvalidRequestBody,
    )?;

    let mut ext = http::Extensions::new();
    if let Some(policy) = vk_policy.cloned() {
        ext.insert(policy);
    }

    if let Err(e) = check_model_access(&ext, &req.model) {
        app_state.0.metrics.vk.model_denied.add(1, &[]);
        tracing::warn!(
            provider = %provider,
            path = %path_and_query,
            model = %req.model,
            "virtual key model policy denied direct proxy request"
        );
        return Err(e);
    }
    Ok(())
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
async fn dispatch_stream_with_retry(
    app_state: &AppState,
    request_builder: RequestBuilder,
    req_body_bytes: Bytes,
    api_endpoint: Option<ApiEndpoint>,
    metrics_registry: EndpointMetricsRegistry,
    request_ctx: &RequestContext,
    request_kind: RequestKind,
    cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
) -> Result<
    (
        http::Response<crate::types::body::Body>,
        crate::types::body::BodyReader,
        oneshot::Receiver<()>,
    ),
    ApiError,
> {
    let retry_config = get_retry_config(app_state, request_kind, request_ctx);
    let fallback_policy_for_log = app_state.config().fallback_policy.clone();
    let provider_for_log = api_endpoint
        .as_ref()
        .map(|e| e.provider().to_string())
        .unwrap_or_default();

    if let Some(retry_config) = retry_config {
        match retry_config.as_ref() {
            RetryConfig::Exponential {
                min_delay,
                max_delay,
                max_retries,
                factor,
            } => {
                let retry_strategy =
                    ExponentialBuilder::default()
                        .with_max_delay(*max_delay)
                        .with_min_delay(*min_delay)
                        .with_max_times(usize::from(*max_retries))
                        .with_factor(factor.to_f32().unwrap_or(
                            crate::config::retry::DEFAULT_RETRY_FACTOR,
                        ))
                        .with_jitter()
                        .build();
                (|| async {
                    Dispatcher::dispatch_stream(
                        &request_builder,
                        req_body_bytes.clone(),
                        api_endpoint.clone(),
                        metrics_registry.clone(),
                        cache_tap.clone(),
                    )
                    .await
                })
                .retry(retry_strategy)
                .sleep(tokio::time::sleep)
                .when(|e: &ApiError| match e {
                    ApiError::StreamError(s) => s.is_retryable(),
                    _ => false,
                })
                .notify(|err: &ApiError, dur: Duration| {
                    if let ApiError::StreamError(_s) = err {
                        tracing::warn!(
                            error = %err,
                            retry_in = ?dur,
                            "upstream server error in stream, retrying...",
                        );
                        crate::fallback::observability::log_decision(
                            &fallback_policy_for_log,
                            crate::fallback::observability::DecisionKind::Retry,
                            None,
                            &provider_for_log,
                        );
                    }
                })
                .await
            }
            RetryConfig::Constant { delay, max_retries } => {
                let retry_strategy = ConstantBuilder::default()
                    .with_delay(*delay)
                    .with_max_times(usize::from(*max_retries))
                    .with_jitter()
                    .build();
                (|| async {
                    Dispatcher::dispatch_stream(
                        &request_builder,
                        req_body_bytes.clone(),
                        api_endpoint.clone(),
                        metrics_registry.clone(),
                        cache_tap.clone(),
                    )
                    .await
                })
                .retry(retry_strategy)
                .sleep(tokio::time::sleep)
                .when(|e: &ApiError| match e {
                    ApiError::StreamError(s) => s.is_retryable(),
                    _ => false,
                })
                .notify(|err: &ApiError, dur: Duration| {
                    if let ApiError::StreamError(_s) = err {
                        tracing::warn!(
                            error = %err,
                            retry_in = ?dur,
                            "upstream server error in stream, retrying...",
                        );
                        crate::fallback::observability::log_decision(
                            &fallback_policy_for_log,
                            crate::fallback::observability::DecisionKind::Retry,
                            None,
                            &provider_for_log,
                        );
                    }
                })
                .await
            }
        }
    } else {
        Dispatcher::dispatch_stream(
            &request_builder,
            req_body_bytes.clone(),
            api_endpoint,
            metrics_registry,
            cache_tap,
        )
        .await
    }
}

fn extract_retry_after(headers: &HeaderMap) -> Option<u64> {
    let retry_after_str = headers
        .get(http::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())?;

    // First try to parse as seconds (u64)
    if let Ok(seconds) = retry_after_str.parse::<u64>() {
        // The value is in seconds, return seconds from now
        return Some(seconds);
    }

    // If that fails, try to parse as HTTP date format
    if let Ok(datetime) =
        DateTime::parse_from_str(retry_after_str, "%a, %d %b %Y %H:%M:%S GMT")
    {
        // Convert to seconds from now
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch is always earlier than now")
            .as_secs();
        let target = u64::try_from(datetime.to_utc().timestamp()).unwrap_or(0);
        if target > now {
            return Some(target - now);
        }
    }

    None
}

fn stream_response_headers() -> HeaderMap {
    HeaderMap::from_iter([
        (
            http::header::CONTENT_TYPE,
            HeaderValue::from_str("text/event-stream; charset=utf-8").unwrap(),
        ),
        (
            http::header::CONNECTION,
            HeaderValue::from_str("keep-alive").unwrap(),
        ),
        (
            http::header::TRANSFER_ENCODING,
            HeaderValue::from_str("chunked").unwrap(),
        ),
    ])
}

fn request_log_id_from_headers(headers: &HeaderMap) -> Uuid {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(Uuid::new_v4)
}

fn get_retry_config<'a>(
    app_state: &'a AppState,
    request_kind: RequestKind,
    _req_ctx: &RequestContext,
) -> Option<std::borrow::Cow<'a, RetryConfig>> {
    if matches!(
        request_kind,
        RequestKind::DirectProxy | RequestKind::CustomProvider
    ) {
        return None;
    }
    fallback_bridge::resolved_global_retry(app_state.config())
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, sync::Arc};

    use http::HeaderValue;
    use indexmap::IndexSet;
    use uuid::Uuid;

    use super::*;
    use crate::{
        app::build_test_app,
        config::{
            Config, providers::GlobalProviderConfig, router::RouterConfig,
        },
        types::{
            extensions::{
                AuthContext, LargeContextAction, LargeContextDecision,
                PromptCompressionTokenPair, PromptContext,
                UnifiedImplicitModelFallbackContext, VkPolicy,
            },
            org::OrgId,
            secret::Secret,
            user::UserId,
        },
    };

    fn auth_ctx_with_base_url(base_url: Option<&str>) -> AuthContext {
        AuthContext {
            api_key: Secret::from("sk-test".to_string()),
            user_id: UserId::new(Uuid::new_v4()),
            org_id: OrgId::new(Uuid::new_v4()),
            virtual_key_id: Some(Uuid::new_v4()),
            virtual_key_prefix: String::new(),
            master_key_id: Some(Uuid::new_v4()),
            master_key_base_url: base_url.map(ToOwned::to_owned),
            department_id: Uuid::nil(),
            entity_type: String::new(),
            entity_id: Uuid::nil(),
            entity_name: String::new(),
            body_ttl_days: 90,
            is_custom_provider: false,
            master_key_allowed_providers: None,
        }
    }

    #[test]
    fn request_builder_effective_host_overrides_default_client_host() {
        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            http::header::HOST,
            HeaderValue::from_static("global.openai.test"),
        );
        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .build()
            .expect("client");
        let target_url: url::Url = "https://cn.openai.test/v1/chat/completions"
            .parse()
            .unwrap();

        let request = request_builder_with_effective_host(
            client.post(target_url.clone()),
            &target_url,
        )
        .build()
        .expect("request");

        assert_eq!(
            request.headers().get(http::header::HOST),
            Some(&HeaderValue::from_static("cn.openai.test"))
        );
    }

    fn direct_body_with_model(model: &str) -> Bytes {
        Bytes::from(
            serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        )
    }

    #[test]
    fn direct_proxy_policy_denies_blocked_openai_chat() {
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
        let err = enforce_direct_proxy_vk_model_policy(
            &app.state,
            ext.get::<VkPolicy>(),
            RequestKind::DirectProxy,
            &InferenceProvider::OpenAI,
            "/v1/chat/completions",
            &direct_body_with_model("gpt-4"),
            &ext,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ApiError::InvalidRequest(
                crate::error::invalid_req::InvalidRequestError::ModelAccessDenied(_)
            )
        ));
    }

    #[test]
    fn direct_proxy_policy_skips_non_openai_provider() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let ext = http::Extensions::new();
        assert!(
            enforce_direct_proxy_vk_model_policy(
                &app.state,
                ext.get::<VkPolicy>(),
                RequestKind::DirectProxy,
                &InferenceProvider::Anthropic,
                "/v1/messages",
                &Bytes::from("{}"),
                &ext,
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn direct_proxy_policy_skips_when_not_direct_proxy_kind() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let ext = http::Extensions::new();
        assert!(
            enforce_direct_proxy_vk_model_policy(
                &app.state,
                ext.get::<VkPolicy>(),
                RequestKind::UnifiedApi,
                &InferenceProvider::OpenAI,
                "/v1/chat/completions",
                &direct_body_with_model("gpt-4"),
                &ext,
                None,
            )
            .is_ok()
        );
    }

    fn empty_request_ctx() -> RequestContext {
        RequestContext {
            auth_context: None,
            router_config: None,
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
        }
    }

    fn request_ctx(
        auth_context: Option<AuthContext>,
        router_config: Option<RouterConfig>,
    ) -> RequestContext {
        RequestContext {
            auth_context,
            router_config: router_config.map(Arc::new),
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
        }
    }

    async fn build_test_dispatcher_async(
        config: Config,
        provider: InferenceProvider,
    ) -> Dispatcher {
        let app = build_test_app(config).await.expect("build app");
        let client = Client::new(&app.state, provider.clone())
            .await
            .expect("client");
        Dispatcher {
            client,
            app_state: app.state,
            provider,
            rate_limit_tx: None,
        }
    }

    fn sync_result_with_status(
        status: StatusCode,
    ) -> Result<
        (
            http::Response<crate::types::body::Body>,
            crate::types::body::BodyReader,
            oneshot::Receiver<()>,
        ),
        ApiError,
    > {
        let stream = futures::stream::once(futures::future::ok::<_, ApiError>(
            Bytes::new(),
        ));
        let (body, reader, rx) =
            BodyReader::wrap_stream(stream, false, TfftTrigger::Never, None);
        Ok((
            http::Response::builder()
                .status(status)
                .body(body)
                .expect("response"),
            reader,
            rx,
        ))
    }

    #[test]
    fn extract_request_context_reads_large_context_decision() {
        let mut req = http::Request::builder()
            .uri("https://example.com/v1/chat/completions")
            .body(crate::types::body::Body::empty())
            .expect("request");
        req.extensions_mut().insert(MapperContext {
            is_stream: false,
            model: None,
            anthropic_openai_usage: None,
            unified_responses_bridge_chat_completions_sse: false,
            native_semantic_passthrough: false,
            cursor_responses_via_chat_completions: false,
            cursor_responses_origin: None,
            client_expects_responses_wire: false,
        });
        req.extensions_mut().insert(Arc::new(empty_request_ctx()));
        req.extensions_mut().insert(
            "/chat/completions"
                .parse::<PathAndQuery>()
                .expect("path and query"),
        );
        req.extensions_mut().insert(InferenceProvider::OpenAI);
        req.extensions_mut().insert(RequestKind::UnifiedApi);
        req.extensions_mut().insert(PromptContext {
            prompt_id: "prompt-1".to_string(),
            prompt_version_id: Some("version-1".to_string()),
            inputs: None,
        });
        req.extensions_mut().insert(PromptCompressionTokenPair {
            origin_prompt_token: 4096,
            compression_prompt_token: 2048,
        });
        req.extensions_mut().insert(LargeContextDecision {
            handler: crate::middleware::large_context::headers::TokenLimitExceptionHandler::Fallback,
            action: LargeContextAction::FallbackApplied,
            original_model: Some("openai/gpt-4o-mini,openai/gpt-4o".to_string()),
            effective_model: Some("openai/gpt-4o".to_string()),
            estimated_input_tokens: Some(120_000),
            model_context_limit: Some(128_000),
            input_budget_tokens: Some(115_200),
        });

        let context = Dispatcher::extract_request_context(&mut req)
            .expect("extract context");
        let large_context_decision = context.large_context_decision;
        let prompt_compression_tokens = context.prompt_compression_tokens;

        assert_eq!(
            prompt_compression_tokens,
            Some(PromptCompressionTokenPair {
                origin_prompt_token: 4096,
                compression_prompt_token: 2048,
            })
        );
        assert!(
            req.extensions()
                .get::<PromptCompressionTokenPair>()
                .is_none(),
            "prompt compression tokens should be removed from request \
             extensions"
        );
        let large_context_decision =
            large_context_decision.expect("large context decision");
        assert_eq!(large_context_decision.handler.as_str(), "fallback");
        assert_eq!(large_context_decision.action.as_str(), "fallback-applied");
        assert_eq!(
            large_context_decision.effective_model.as_deref(),
            Some("openai/gpt-4o")
        );
    }

    #[test]
    fn get_retry_config_direct_proxy_is_none() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let req_ctx = empty_request_ctx();
        let retry =
            get_retry_config(&app.state, RequestKind::DirectProxy, &req_ctx);
        assert!(retry.is_none(), "direct proxy should skip global retry");
    }

    #[test]
    fn get_retry_config_uses_fallback_policy_when_enabled() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut config = Config::default();
        config.fallback_policy.enabled = true;
        config.fallback_policy.retry = RetryConfig::Constant {
            delay: Duration::from_millis(7),
            max_retries: 1,
        };
        config.global.retries = Some(RetryConfig::Constant {
            delay: Duration::from_secs(99),
            max_retries: 1,
        });
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let req_ctx = empty_request_ctx();
        let retry =
            get_retry_config(&app.state, RequestKind::UnifiedApi, &req_ctx)
                .expect("retry config should exist");
        assert_eq!(
            retry,
            Cow::Owned(RetryConfig::Constant {
                delay: Duration::from_millis(7),
                max_retries: 1,
            }),
            "fallback-policy retry should take precedence"
        );
    }

    #[test]
    fn get_retry_config_falls_back_to_global_when_policy_disabled() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut config = Config::default();
        config.fallback_policy.enabled = false;
        config.global.retries = Some(RetryConfig::Constant {
            delay: Duration::from_millis(11),
            max_retries: 2,
        });
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let req_ctx = empty_request_ctx();
        let retry = get_retry_config(&app.state, RequestKind::Router, &req_ctx)
            .expect("global retry should be used");
        assert_eq!(
            retry,
            Cow::Owned(RetryConfig::Constant {
                delay: Duration::from_millis(11),
                max_retries: 2,
            })
        );
    }

    #[test]
    fn get_retry_config_returns_none_when_policy_and_global_disabled() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let mut config = Config::default();
        config.fallback_policy.enabled = false;
        config.global.retries = None;
        let app = rt.block_on(build_test_app(config)).expect("build app");
        let req_ctx = empty_request_ctx();
        let retry = get_retry_config(&app.state, RequestKind::Router, &req_ctx);
        assert!(retry.is_none(), "no retry expected");
    }

    #[test]
    fn should_attempt_regional_retry_requires_default_source_and_cn_url() {
        let endpoint = TargetEndpoint {
            url: "https://global.openai.test/v1/chat/completions"
                .parse()
                .unwrap(),
            source: TargetEndpointSource::GlobalProviderBaseUrl,
            cn_retry_url: Some(
                "https://cn.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
            ),
        };

        assert!(should_attempt_regional_endpoint_retry(
            &endpoint,
            StatusCode::UNAUTHORIZED
        ));
        assert!(should_attempt_regional_endpoint_retry(
            &endpoint,
            StatusCode::FORBIDDEN
        ));
        assert!(!should_attempt_regional_endpoint_retry(
            &endpoint,
            StatusCode::TOO_MANY_REQUESTS
        ));

        let endpoint_without_cn = TargetEndpoint {
            cn_retry_url: None,
            ..endpoint
        };
        assert!(!should_attempt_regional_endpoint_retry(
            &endpoint_without_cn,
            StatusCode::UNAUTHORIZED
        ));
    }

    #[test]
    fn should_attempt_regional_retry_rejects_learned_or_custom_sources() {
        for source in [
            TargetEndpointSource::MasterKeyBaseUrl,
            TargetEndpointSource::RouterProviderBaseUrl,
            TargetEndpointSource::LearnedCn,
        ] {
            let endpoint = TargetEndpoint {
                url: "https://custom.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
                source,
                cn_retry_url: Some(
                    "https://cn.openai.test/v1/chat/completions"
                        .parse()
                        .unwrap(),
                ),
            };

            assert!(!should_attempt_regional_endpoint_retry(
                &endpoint,
                StatusCode::UNAUTHORIZED
            ));
        }
    }

    #[test]
    fn regional_retry_is_disabled_for_streaming_path() {
        assert!(
            !regional_endpoint_retry_enabled_for_streaming(),
            "phase one intentionally avoids transparent stream regional retry"
        );
    }

    #[test]
    fn regional_retry_result_error_keeps_original_response_and_url() {
        let mut result = sync_result_with_status(StatusCode::UNAUTHORIZED);
        let mut effective_target_url: url::Url =
            "https://global.openai.test/v1/chat/completions"
                .parse()
                .unwrap();

        apply_regional_retry_result(
            &mut result,
            &mut effective_target_url,
            Some(
                "https://cn.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
            ),
            Err(ApiError::Internal(InternalError::Internal)),
            &InferenceProvider::OpenAI,
            None,
        );

        assert_eq!(
            result.expect("original response").0.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            effective_target_url.as_str(),
            "https://global.openai.test/v1/chat/completions"
        );
    }

    #[test]
    fn regional_retry_result_success_replaces_response_and_url() {
        let mut result = sync_result_with_status(StatusCode::UNAUTHORIZED);
        let mut effective_target_url: url::Url =
            "https://global.openai.test/v1/chat/completions"
                .parse()
                .unwrap();

        let regional_retry_produced_response = apply_regional_retry_result(
            &mut result,
            &mut effective_target_url,
            Some(
                "https://cn.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
            ),
            Ok(Some(
                sync_result_with_status(StatusCode::OK)
                    .expect("regional response"),
            )),
            &InferenceProvider::OpenAI,
            None,
        );

        assert!(regional_retry_produced_response);
        assert_eq!(
            result.expect("regional response").0.status(),
            StatusCode::OK
        );
        assert_eq!(
            effective_target_url.as_str(),
            "https://cn.openai.test/v1/chat/completions"
        );
    }

    #[test]
    fn regional_retry_result_non_success_response_replaces_original_response_and_marks_final()
     {
        let mut result = sync_result_with_status(StatusCode::UNAUTHORIZED);
        let mut effective_target_url: url::Url =
            "https://global.openai.test/v1/chat/completions"
                .parse()
                .unwrap();

        let regional_retry_produced_response = apply_regional_retry_result(
            &mut result,
            &mut effective_target_url,
            Some(
                "https://cn.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
            ),
            Ok(Some(
                sync_result_with_status(StatusCode::FORBIDDEN)
                    .expect("regional response"),
            )),
            &InferenceProvider::OpenAI,
            None,
        );

        assert!(regional_retry_produced_response);
        assert_eq!(
            result.expect("regional response").0.status(),
            StatusCode::FORBIDDEN,
            "CN HTTP response is returned because it is the final attempted \
             endpoint"
        );
        assert_eq!(
            effective_target_url.as_str(),
            "https://cn.openai.test/v1/chat/completions"
        );
    }

    #[test]
    fn learned_cn_http_response_is_final_for_cross_provider_fallback() {
        let endpoint = TargetEndpoint {
            url: "https://cn.openai.test/v1/chat/completions"
                .parse()
                .unwrap(),
            source: TargetEndpointSource::LearnedCn,
            cn_retry_url: Some(
                "https://cn.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
            ),
        };
        let result = sync_result_with_status(StatusCode::BAD_GATEWAY);

        assert!(target_endpoint_response_is_final(&endpoint, &result));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            target_endpoint_response_is_final(&endpoint, &result),
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&UnifiedImplicitModelFallbackContext {
                selected_model: "openai/gpt-5.4".to_string(),
            }),
            &result,
        ));
    }

    #[test]
    fn learned_cn_internal_error_is_not_marked_final() {
        let endpoint = TargetEndpoint {
            url: "https://cn.openai.test/v1/chat/completions"
                .parse()
                .unwrap(),
            source: TargetEndpointSource::LearnedCn,
            cn_retry_url: Some(
                "https://cn.openai.test/v1/chat/completions"
                    .parse()
                    .unwrap(),
            ),
        };
        let result = Err(ApiError::Internal(InternalError::Internal));

        assert!(!target_endpoint_response_is_final(&endpoint, &result));
    }

    #[test]
    fn cross_provider_fallback_skips_regional_retry_http_response() {
        let ctx = UnifiedImplicitModelFallbackContext {
            selected_model: "openai/gpt-5.4".to_string(),
        };

        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            true,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
    }

    #[test]
    fn cross_provider_fallback_requires_implicit_chat_retryable_result() {
        let ctx = UnifiedImplicitModelFallbackContext {
            selected_model: "openai/gpt-5.4".to_string(),
        };
        assert!(should_attempt_cross_provider_default_model_fallback(
            true,
            false,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(should_attempt_cross_provider_default_model_fallback(
            true,
            false,
            RequestKind::UnifiedApi,
            "/v1/chat/completions?user=test",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            false,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_REQUEST),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            false,
            RequestKind::DirectProxy,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            false,
            RequestKind::UnifiedApi,
            "/v1/responses",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
        assert!(!should_attempt_cross_provider_default_model_fallback(
            true,
            false,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            None,
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
    }

    #[test]
    fn cross_provider_fallback_requires_retry_to_have_occurred() {
        let ctx = UnifiedImplicitModelFallbackContext {
            selected_model: "openai/gpt-5.4".to_string(),
        };
        assert!(!should_attempt_cross_provider_default_model_fallback(
            false,
            false,
            RequestKind::UnifiedApi,
            "/v1/chat/completions",
            Some(&ctx),
            &sync_result_with_status(StatusCode::BAD_GATEWAY),
        ));
    }

    #[tokio::test]
    async fn cross_provider_fallback_request_details_use_effective_provider_url_and_body()
     {
        let openai_provider = InferenceProvider::OpenAI;
        let groq_provider = InferenceProvider::Named("groq".into());
        let groq_model = "llama-3.1-8b";
        let mut config = Config::default();
        config
            .providers
            .get_mut(&openai_provider)
            .expect("openai config")
            .base_url = "https://openai.test/".parse().expect("openai url");
        config.providers.insert(
            groq_provider.clone(),
            GlobalProviderConfig {
                models: IndexSet::from([ModelId::from_str_and_provider(
                    groq_provider.clone(),
                    groq_model,
                )
                .expect("groq model")]),
                base_url: "https://groq.test/".parse().expect("groq url"),
                cn_base_url: Some(
                    "https://cn.groq.test/".parse().expect("groq cn url"),
                ),
                version: None,
                upstream_auth: Default::default(),
            },
        );
        let dispatcher =
            build_test_dispatcher_async(config, openai_provider.clone()).await;
        let auth = auth_ctx_with_base_url(None);
        let master_key_id = auth.master_key_id;
        let req_ctx = request_ctx(Some(auth), None);
        crate::dispatcher::regional_endpoint::remember_region(
            &dispatcher.app_state,
            master_key_id,
            crate::dispatcher::regional_endpoint::EndpointRegion::Cn,
        )
        .await;
        let req_body_bytes = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let executor =
            crate::dispatcher::fallback_executor::FallbackExecutor::new(
                &dispatcher.app_state,
            );
        let (effective_provider, effective_target_url, effective_request_body) =
            executor
                .cross_provider_fallback_request_details(
                    &req_ctx,
                    "/v1/chat/completions",
                    &req_body_bytes,
                    &format!("groq/{groq_model}"),
                )
                .await
                .expect("fallback request details");

        assert_eq!(effective_provider, groq_provider);
        assert_eq!(
            effective_target_url.as_str(),
            "https://cn.groq.test/v1/chat/completions"
        );
        let effective_body: serde_json::Value =
            serde_json::from_slice(&effective_request_body)
                .expect("effective body json");
        assert_eq!(
            effective_body
                .get("model")
                .and_then(serde_json::Value::as_str),
            Some("groq/llama-3.1-8b")
        );
    }

    #[test]
    fn allowlist_guard_allows_when_workspace_allowlist_empty() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let app = rt
            .block_on(build_test_app(Config::default()))
            .expect("build app");
        let auth = auth_ctx_with_base_url(None);
        assert!(
            enforce_workspace_provider_allowlist(
                &app.state,
                Some(&auth),
                &InferenceProvider::OpenAI,
            )
            .is_ok()
        );
    }

    #[test]
    fn allowlist_guard_skips_without_auth_context_in_cloud() {
        assert!(
            allowlist_workspace_id_for_request(None).is_none(),
            "cloud request without auth context must skip allowlist \
             enforcement"
        );
    }

    #[test]
    fn allowlist_guard_extracts_workspace_id_in_cloud_with_auth() {
        let auth = auth_ctx_with_base_url(None);
        assert_eq!(
            allowlist_workspace_id_for_request(Some(&auth)),
            Some(*auth.org_id.as_ref())
        );
    }
}
