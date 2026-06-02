use std::{
    str::FromStr,
    task::{Context, Poll},
};

use bytes::{BufMut, Bytes, BytesMut};
use futures::{StreamExt, TryStreamExt, future::BoxFuture};
use http::uri::PathAndQuery;
use tracing::{Instrument, info_span};

use crate::{
    app_state::AppState,
    endpoints::{
        ApiEndpoint,
        anthropic::Anthropic,
        openai::{OpenAI, OpenAICompatibleChatCompletionRequest},
    },
    error::{
        api::ApiError, internal::InternalError, invalid_req::InvalidRequestError,
        mapper::MapperError, stream::StreamError,
    },
    ide_adapation::mapper_service_hooks,
    middleware::mapper::{
        chat_completion_role_normalize::lenient_openai_chat_roles_for_target_endpoint,
        envelope::RequestEnvelope, profile_resolver::resolve_mapper_metadata,
        registry::EndpointConverterRegistry,
    },
    types::{
        extensions::{
            MapperContext, MapperProfileContext, MasterKeyUnifiedModelPassthrough, RequestContext,
            UnifiedChatCompletionsResponsesBridge, UnifiedModelBodyPassthrough,
            UnifiedModelPolicyChecked,
        },
        model_id::ModelId,
        provider::InferenceProvider,
        request::Request,
        response::Response,
    },
    utils::debug_log::{self, DebugLogConfig},
    virtual_key::enforce::check_model_access,
};

#[derive(Debug, Clone)]
pub struct Service<S> {
    inner: S,
    endpoint_converter_registry: EndpointConverterRegistry,
    app_state: AppState,
}

impl<S> Service<S> {
    pub fn new(
        inner: S,
        endpoint_converter_registry: EndpointConverterRegistry,
        app_state: AppState,
    ) -> Self {
        Self {
            inner,
            endpoint_converter_registry,
            app_state,
        }
    }
}

impl<S> tower::Service<Request> for Service<S>
where
    S: tower::Service<
            Request,
            Response = http::Response<crate::types::body::Body>,
            Error = ApiError,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[tracing::instrument(name = "mapper", skip_all)]
    fn call(&mut self, mut req: Request) -> Self::Future {
        // see: https://docs.rs/tower/latest/tower/trait.Service.html#be-careful-when-cloning-inner-services
        let mut inner = self.inner.clone();
        let converter_registry = self.endpoint_converter_registry.clone();
        let app_state = self.app_state.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        Box::pin(async move {
            let mut target_provider = req
                .extensions()
                .get::<InferenceProvider>()
                .cloned()
                .ok_or_else(|| {
                    ApiError::Internal(InternalError::ExtensionNotFound("InferenceProvider"))
                })?;
            if req
                .extensions()
                .get::<MasterKeyUnifiedModelPassthrough>()
                .is_some()
            {
                target_provider = InferenceProvider::Custom;
            }
            let extracted_path_and_query =
                req.extensions_mut()
                    .remove::<PathAndQuery>()
                    .ok_or(ApiError::Internal(InternalError::ExtensionNotFound(
                        "PathAndQuery",
                    )))?;
            let source_endpoint = req.extensions().get::<ApiEndpoint>().cloned();
            let source_endpoint = source_endpoint.ok_or(ApiError::Internal(
                InternalError::ExtensionNotFound("ApiEndpoint"),
            ))?;
            let source_endpoint_cloned = source_endpoint.clone();
            let target_endpoint = ApiEndpoint::mapped(source_endpoint, &target_provider)?;
            let target_endpoint_cloned = target_endpoint.clone();
            // serialization/deserialization should be done on a dedicated
            // thread
            let converter_registry_cloned = converter_registry.clone();
            let source_endpoint_for_req = source_endpoint_cloned.clone();
            let target_endpoint_for_req = target_endpoint_cloned.clone();
            let req = tokio::task::spawn_blocking(move || async move {
                map_request(
                    app_state,
                    converter_registry_cloned,
                    source_endpoint_for_req,
                    target_endpoint_for_req,
                    &extracted_path_and_query,
                    req,
                )
                .instrument(info_span!("map_request"))
                .await
            })
            .await
            .map_err(InternalError::MappingTaskError)?
            .await?;
            let response = inner.call(req).await?;
            let response = tokio::task::spawn_blocking(move || async move {
                map_response(
                    converter_registry,
                    target_endpoint_cloned,
                    source_endpoint_cloned,
                    response,
                )
                .await
            })
            .instrument(info_span!("map_response"))
            .await
            .map_err(InternalError::MappingTaskError)?
            .await?;
            Ok(response)
        })
    }
}

#[allow(clippy::too_many_lines)]
async fn map_request(
    app_state: AppState,
    converter_registry: EndpointConverterRegistry,
    source_endpoint: ApiEndpoint,
    target_endpoint: ApiEndpoint,
    target_path_and_query: &PathAndQuery,
    req: Request,
) -> Result<Request, ApiError> {
    use http_body_util::BodyExt;
    let (mut parts, body) = req.into_parts();
    let debug_log_config = parts
        .extensions
        .get::<DebugLogConfig>()
        .copied()
        .unwrap_or_else(|| DebugLogConfig::from_headers(&mut parts.headers));
    debug_log::maybe_log_headers("mapper", &parts.headers, debug_log_config);
    let client_profile_resolution =
        mapper_service_hooks::resolve_and_attach_client_profile(&mut parts);
    let body = body
        .collect()
        .await
        .map_err(InternalError::CollectBodyError)?
        .to_bytes();
    debug_log::maybe_log_body("mapper", &body, debug_log_config);

    let workspace_id = parts
        .extensions
        .get::<std::sync::Arc<RequestContext>>()
        .and_then(|ctx| ctx.auth_context.as_ref())
        .map(|auth| auth.org_id.to_string())
        .unwrap_or_default();

    let (b, header_log) =
        crate::content_filter::prompt_cache::merge_prompt_cache_messages_into_body(
            app_state.redis(),
            &parts.headers,
            &workspace_id,
            body,
            &app_state.0.metrics.vk,
        )
        .await?;
    let body = b;
    if let Some(h) = header_log {
        parts.extensions.insert(h);
    }

    let filter_result = match crate::content_filter::evaluate::evaluate_for_vk_request(
        &app_state,
        &parts.headers,
        &parts.extensions,
        &body,
    )
    .await
    {
        Ok(r) => r,
        Err(ApiError::InvalidRequest(InvalidRequestError::ContentPolicyDenied { message })) => {
            emit_mapper_policy_deny_log(&app_state, &parts, &body, &message, target_path_and_query);
            return Err(ApiError::InvalidRequest(
                InvalidRequestError::ContentPolicyDenied { message },
            ));
        }
        Err(e) => return Err(e),
    };
    let mut body = match filter_result.forward_body {
        crate::content_filter::ContentFilterForwardBody::UseOriginal => body,
        crate::content_filter::ContentFilterForwardBody::UseReplaced(b) => b,
    };
    if let Some(ref new_model) = filter_result.change_model {
        let (new_body, original) =
            crate::content_filter::evaluate::apply_model_downgrade(body, new_model);
        body = new_body;
        let original_model = original.unwrap_or_default();
        tracing::info!(
            original_model = %original_model,
            downgraded_model = %new_model,
            "content_filter: policy model downgrade applied (router/mapper)"
        );
        parts
            .extensions
            .insert(crate::content_filter::PolicyModelOverride {
                original_model,
                downgraded_model: new_model.clone(),
            });
    }

    body = crate::ide_adapation::responses_strategy::normalize_responses_for_target(
        &source_endpoint,
        &target_endpoint,
        body,
        client_profile_resolution.profile,
    )?;

    // Pre-convert Responses → ChatCompletions for cross-protocol targets
    let (mut body, source_endpoint) =
        crate::ide_adapation::responses_strategy::maybe_preconvert_responses_to_chat(
            source_endpoint,
            &target_endpoint,
            body,
        )?;

    if matches!(
        source_endpoint,
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_))
    ) {
        let provider = target_endpoint.provider();
        body = crate::middleware::prompt_compression::apply_chat_completions(
            &mut parts, body, &provider,
        )?;
        body =
            crate::ide_adapation::ide_ingress_adjust::apply_global_chat_completions_wire_normalize(
                body,
            )?;
    }

    let master_key_model_passthrough = parts
        .extensions
        .get::<MasterKeyUnifiedModelPassthrough>()
        .is_some();
    let unified_model_body_passthrough = parts
        .extensions
        .get::<UnifiedModelBodyPassthrough>()
        .is_some();

    if should_enforce_mapper_model_policy(&parts.extensions) {
        enforce_vk_model_policy_for_source_endpoint(
            &app_state,
            &parts.extensions,
            &source_endpoint,
            &body,
        )?;
    }

    let (body, ide_ingress_adjust_meta) =
        mapper_service_hooks::apply_ide_ingress_adjust_record_metrics(
            &app_state,
            client_profile_resolution.profile,
            &source_endpoint,
            &parts.extensions,
            body,
        )?;

    let passthrough = mapper_service_hooks::record_client_profile_passthrough_metrics_and_extension(
        &app_state,
        &mut parts,
        &client_profile_resolution,
        &source_endpoint,
        &target_endpoint,
    );

    let skip_semantic_envelope = should_skip_semantic_envelope(&parts.extensions, passthrough);
    let request_envelope = if skip_semantic_envelope {
        None
    } else {
        RequestEnvelope::from_source_request_bytes(
            &source_endpoint,
            target_endpoint.provider(),
            &body,
        )?
        .map(|request_envelope| {
            let resolved = resolve_mapper_metadata(
                &request_envelope.target_provider,
                Some(request_envelope.raw_model.as_str()),
            )
            .expect("request mapper metadata should resolve");

            request_envelope
                .with_target_capabilities(resolved.capabilities.clone())
                .with_target_rules(resolved.rules.clone())
                .with_resolved_metadata(resolved)
        })
    };

    let (body, request_envelope) = if let Some(request_envelope) = request_envelope {
        let request_envelope =
            crate::middleware::mapper::request_rule_engine::prepare_request_envelope(
                request_envelope,
            )
            .map_err(InternalError::MapperError)?;
        let body = Bytes::from(
            serde_json::to_vec(&request_envelope.openai_request).map_err(|error| {
                InternalError::Serialize {
                    ty: std::any::type_name::<async_openai::types::CreateChatCompletionRequest>(),
                    error,
                }
            })?,
        );

        (body, Some(request_envelope))
    } else {
        (body, None)
    };
    let body = if unified_model_body_passthrough {
        normalize_unified_passthrough_body_model(&body, target_endpoint.provider())?
    } else {
        body
    };

    let unified_responses_bridge_chat_completions_sse = parts
        .extensions
        .remove::<UnifiedChatCompletionsResponsesBridge>()
        .is_some();

    let (body, mut mapper_ctx, upstream_endpoint) = if let Some(triple) =
        mapper_service_hooks::try_cursor_responses_compatible_chat_mapping(
            &converter_registry,
            client_profile_resolution.profile,
            &source_endpoint,
            &target_endpoint,
            &body,
            unified_model_body_passthrough,
        )? {
        triple
    } else {
        let converter = converter_registry
            .get_converter(&source_endpoint, &target_endpoint)
            .ok_or_else(|| {
                InternalError::InvalidConverter(source_endpoint.clone(), target_endpoint.clone())
            })?;
        let (body, mapper_ctx) = if master_key_model_passthrough {
            match (&source_endpoint, &target_endpoint) {
                (
                    ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)),
                    ApiEndpoint::OpenAICompatible {
                        provider,
                        openai_endpoint,
                    },
                ) if *openai_endpoint == OpenAI::chat_completions() => {
                    master_key_unified_passthrough_chat_completions(body, provider.clone())?
                }
                _ => converter.convert_req_body(body)?,
            }
        } else if passthrough {
            let ctx = mapper_service_hooks::mapper_context_native_semantic_passthrough(
                &source_endpoint,
                &target_endpoint,
                &body,
            )?;
            (body, ctx)
        } else if unified_model_body_passthrough {
            if supports_unified_model_body_passthrough(&source_endpoint, &target_endpoint) {
                tracing::trace!(
                    source_endpoint = ?source_endpoint,
                    target_endpoint = ?target_endpoint,
                    "unified model passthrough: skipped model catalog mapping"
                );
                converter.convert_req_body_model_passthrough(body)?
            } else {
                tracing::debug!(
                    source_endpoint = ?source_endpoint,
                    target_endpoint = ?target_endpoint,
                    "unified model passthrough: semantic target still uses normal converter until 6B"
                );
                converter.convert_req_body(body)?
            }
        } else {
            converter.convert_req_body(body)?
        };
        (body, mapper_ctx, target_endpoint.clone())
    };
    mapper_ctx.unified_responses_bridge_chat_completions_sse =
        unified_responses_bridge_chat_completions_sse;
    let base_path = upstream_endpoint.path(mapper_ctx.model.as_ref(), mapper_ctx.is_stream)?;

    let target_path_and_query = if let Some(query_params) = target_path_and_query.query() {
        format!("{base_path}?{query_params}")
    } else {
        base_path
    };
    let target_path_and_query =
        PathAndQuery::from_str(&target_path_and_query).map_err(InternalError::InvalidUri)?;

    let mut req = Request::from_parts(parts, axum_core::body::Body::from(body));
    if client_profile_resolution.profile
        == crate::ide_adapation::client_profile::ClientProfile::CodexCli
    {
        tracing::info!(
            passthrough,
            stream = mapper_ctx.is_stream,
            responses_bridge = mapper_ctx.cursor_responses_via_chat_completions,
            client_expects_responses_wire = mapper_ctx.client_expects_responses_wire,
            unified_chat_redirect = mapper_ctx.unified_responses_bridge_chat_completions_sse,
            source_endpoint = ?source_endpoint,
            target_endpoint = ?upstream_endpoint,
            target_path = %target_path_and_query,
            "codex: mapped request"
        );
    }
    tracing::trace!(
        client_profile = ?client_profile_resolution.profile,
        passthrough,
        ide_ingress_adjust_applied = ide_ingress_adjust_meta.applied,
        source_endpoint = ?source_endpoint,
        target_endpoint = ?upstream_endpoint,
        target_path_and_query = ?target_path_and_query,
        mapper_ctx = ?mapper_ctx,
        "mapped request"
    );
    if let Some(request_envelope) = request_envelope {
        if let Some(resolved) = request_envelope.resolved_metadata.as_ref() {
            req.extensions_mut().insert(MapperProfileContext {
                provider: request_envelope.target_provider.clone(),
                raw_model: request_envelope.raw_model.clone(),
                non_stream_profile: resolved.non_stream_profile.clone(),
            });
        }
        req.extensions_mut().insert(request_envelope);
    }
    req.extensions_mut().insert(target_path_and_query);
    req.extensions_mut().insert(mapper_ctx);
    req.extensions_mut().insert(upstream_endpoint);
    req.extensions_mut().insert(debug_log_config);
    Ok(req)
}

fn master_key_unified_passthrough_chat_completions(
    body: Bytes,
    provider: InferenceProvider,
) -> Result<(Bytes, MapperContext), ApiError> {
    use async_openai::types::CreateChatCompletionRequest;

    #[allow(deprecated)]
    let mut req = serde_json::from_slice::<CreateChatCompletionRequest>(&body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    #[allow(deprecated)]
    if req.max_completion_tokens.is_none()
        && let Some(v) = req.max_tokens.take()
    {
        req.max_completion_tokens = Some(v);
    }
    let is_stream = req.stream.unwrap_or(false);
    let model = ModelId::from_str_and_provider(provider.clone(), &req.model)
        .map_err(InternalError::MapperError)?;
    let wrapped = OpenAICompatibleChatCompletionRequest {
        provider,
        inner: req,
    };
    let target_bytes =
        Bytes::from(
            serde_json::to_vec(&wrapped).map_err(|e| InternalError::Serialize {
                ty: std::any::type_name::<OpenAICompatibleChatCompletionRequest>(),
                error: e,
            })?,
        );
    let anthropic_openai_usage = is_stream.then(|| {
        std::sync::Arc::new(std::sync::Mutex::new(
            crate::types::extensions::AnthropicStreamOpenAiUsageState::default(),
        ))
    });
    Ok((
        target_bytes,
        MapperContext {
            is_stream,
            model: Some(model),
            anthropic_openai_usage,
            unified_responses_bridge_chat_completions_sse: false,
            native_semantic_passthrough: false,
            cursor_responses_via_chat_completions: false,
            cursor_responses_origin: None,
            client_expects_responses_wire: false,
        },
    ))
}

pub fn enforce_vk_model_policy_for_source_endpoint(
    app_state: &AppState,
    extensions: &http::Extensions,
    source_endpoint: &ApiEndpoint,
    body: &bytes::Bytes,
) -> Result<(), ApiError> {
    if extensions
        .get::<crate::content_filter::PolicyModelOverride>()
        .is_some()
    {
        return Ok(());
    }
    use anthropic_ai_sdk::types::message::CreateMessageParams;
    use async_openai::types::{
        CreateChatCompletionRequest, CreateCompletionRequest, CreateEmbeddingRequest,
        CreateImageRequest, ImageModel,
    };
    const EP: &str = "router/mapper";
    let deny = |model: &str| {
        if let Err(e) = check_model_access(extensions, model) {
            app_state.0.metrics.vk.model_denied.add(1, &[]);
            Err(e)
        } else {
            Ok(())
        }
    };
    match source_endpoint {
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)) => {
            let req = serde_json::from_slice::<CreateChatCompletionRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&req.model) {
                tracing::warn!(
                    model = %req.model,
                    endpoint = "router/openai/chat_completions",
                    "virtual key model policy denied router request"
                );
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::Completions(_)) => {
            let r = serde_json::from_slice::<CreateCompletionRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::Embeddings(_)) => {
            let r = serde_json::from_slice::<CreateEmbeddingRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::ImageGenerations(_)) => {
            let r = serde_json::from_slice::<CreateImageRequest>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            let m = r
                .model
                .as_ref()
                .ok_or(InvalidRequestError::MissingModelId)?;
            let name = match m {
                ImageModel::DallE2 => "dall-e-2",
                ImageModel::DallE3 => "dall-e-3",
                ImageModel::Other(s) => s.as_str(),
            };
            if let Err(e) = deny(name) {
                tracing::warn!(model = %name, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::OpenAI(OpenAI::Responses(_)) => {
            let fields =
                crate::ide_adapation::responses_ingress_normalize::responses_request_routing_fields(
                    body,
                )?;
            if let Err(e) = deny(&fields.model) {
                tracing::warn!(model = %fields.model, endpoint = EP);
                return Err(e);
            }
        }
        ApiEndpoint::Anthropic(Anthropic::Messages(_)) => {
            let r = serde_json::from_slice::<CreateMessageParams>(body)
                .map_err(InvalidRequestError::InvalidRequestBody)?;
            if let Err(e) = deny(&r.model) {
                tracing::warn!(model = %r.model, endpoint = EP);
                return Err(e);
            }
        }
        _ => {}
    }
    Ok(())
}

fn should_enforce_mapper_model_policy(extensions: &http::Extensions) -> bool {
    extensions
        .get::<MasterKeyUnifiedModelPassthrough>()
        .is_none()
        && extensions.get::<UnifiedModelPolicyChecked>().is_none()
}

fn should_skip_semantic_envelope(
    extensions: &http::Extensions,
    native_semantic_passthrough: bool,
) -> bool {
    extensions
        .get::<MasterKeyUnifiedModelPassthrough>()
        .is_some()
        || native_semantic_passthrough
}

fn normalize_unified_passthrough_body_model(
    body: &Bytes,
    target_provider: InferenceProvider,
) -> Result<Bytes, ApiError> {
    let Some(normalized) = normalize_top_level_model_for_target_provider(body, &target_provider)?
    else {
        return Ok(body.clone());
    };
    Ok(normalized)
}

fn normalize_top_level_model_for_target_provider(
    body: &Bytes,
    target_provider: &InferenceProvider,
) -> Result<Option<Bytes>, ApiError> {
    let mut value = serde_json::from_slice::<serde_json::Value>(body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    let Some(model) = value.get("model").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let Some((prefix, raw_model)) = model.split_once('/') else {
        return Ok(None);
    };
    if raw_model.is_empty() {
        return Ok(None);
    }
    let matches_target = prefix.eq_ignore_ascii_case(target_provider.as_ref())
        || prefix.eq_ignore_ascii_case(target_provider.as_provider_code());
    if !matches_target {
        return Ok(None);
    }
    value["model"] = serde_json::Value::String(raw_model.to_string());
    let body = serde_json::to_vec(&value).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: "serde_json::Value",
            error,
        })
    })?;
    Ok(Some(Bytes::from(body)))
}

fn supports_unified_model_body_passthrough(
    source_endpoint: &ApiEndpoint,
    target_endpoint: &ApiEndpoint,
) -> bool {
    match (source_endpoint, target_endpoint) {
        (ApiEndpoint::OpenAI(source), ApiEndpoint::OpenAI(target)) => {
            matches!(
                (source, target),
                (OpenAI::ChatCompletions(_), OpenAI::ChatCompletions(_))
                    | (OpenAI::Completions(_), OpenAI::Completions(_))
                    | (OpenAI::Embeddings(_), OpenAI::Embeddings(_))
                    | (OpenAI::ImageGenerations(_), OpenAI::ImageGenerations(_))
                    | (OpenAI::Responses(_), OpenAI::Responses(_))
            )
        }
        (
            ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_)),
            ApiEndpoint::OpenAICompatible {
                openai_endpoint: OpenAI::ChatCompletions(_),
                ..
            }
            | ApiEndpoint::Anthropic(crate::endpoints::anthropic::Anthropic::Messages(_))
            | ApiEndpoint::Google(crate::endpoints::google::Google::GenerateContents(_))
            | ApiEndpoint::Bedrock(crate::endpoints::bedrock::Bedrock::Converse(_))
            | ApiEndpoint::Ollama(crate::endpoints::ollama::Ollama::ChatCompletions(_)),
        )
        | (
            ApiEndpoint::OpenAI(OpenAI::Responses(_)),
            ApiEndpoint::OpenAICompatible {
                openai_endpoint: OpenAI::Responses(_),
                ..
            },
        ) => true,
        _ => false,
    }
}

fn emit_mapper_policy_deny_log(
    app_state: &AppState,
    parts: &http::request::Parts,
    body: &bytes::Bytes,
    deny_message: &str,
    target_path_and_query: &http::uri::PathAndQuery,
) {
    use chrono::Utc;
    use tokio::time::Instant;
    use tracing::Instrument;
    use uuid::Uuid;

    use crate::{
        logger::service::LoggerService,
        session_headers::parse_session_headers,
        types::{
            body::{BodyReader, TfftTrigger},
            extensions::{MapperContext, PromptHeaderForRequestLog, RequestContext},
            provider::InferenceProvider,
            router::RouterId,
        },
    };

    if !app_state.config().alephant.is_observability_enabled() {
        return;
    }
    let req_ctx = match parts.extensions.get::<std::sync::Arc<RequestContext>>() {
        Some(ctx) => ctx.clone(),
        None => return,
    };
    let Some(auth_ctx) = req_ctx.auth_context.clone() else {
        return;
    };

    let target_provider = match parts.extensions.get::<InferenceProvider>() {
        Some(p) => p.clone(),
        None => return,
    };

    let target_url = {
        let providers_config = app_state.get_providers_config();
        let Some(provider_config) = providers_config.get(&target_provider) else {
            tracing::warn!("policy deny log (mapper): provider not configured, skipping");
            return;
        };
        match provider_config
            .base_url
            .join(target_path_and_query.as_str())
        {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "policy deny log (mapper): failed to build target_url, \
                     skipping"
                );
                return;
            }
        }
    };

    let start_instant = parts
        .extensions
        .get::<Instant>()
        .copied()
        .unwrap_or_else(Instant::now);
    let start_time = parts
        .extensions
        .get::<chrono::DateTime<Utc>>()
        .copied()
        .unwrap_or_else(Utc::now);
    let router_id = parts.extensions.get::<RouterId>().cloned();
    let prompt_header = parts.extensions.get::<PromptHeaderForRequestLog>().cloned();
    let prompt_ctx = parts
        .extensions
        .get::<crate::types::extensions::PromptContext>()
        .cloned();
    let session_ctx = parse_session_headers(&parts.headers).ok().flatten();

    let mapper_ctx = MapperContext {
        is_stream: false,
        model: None,
        anthropic_openai_usage: None,
        unified_responses_bridge_chat_completions_sse: false,
        native_semantic_passthrough: false,
        cursor_responses_via_chat_completions: false,
        cursor_responses_origin: None,
        client_expects_responses_wire: false,
    };

    let response_body_bytes =
        crate::content_filter::evaluate::policy_denied_error_response_json(deny_message);

    let response_status = http::StatusCode::OK;
    let request_log_id = parts
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(Uuid::new_v4);
    let response_log_id = Uuid::new_v4();
    let response_received_at = Utc::now();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(bytes::Bytes::from(response_body_bytes));
    drop(tx);

    let (tfft_tx_for_body, _unused_rx) = tokio::sync::oneshot::channel();
    let body_reader = BodyReader::new(
        rx,
        tfft_tx_for_body,
        hyper::body::SizeHint::default(),
        false,
        TfftTrigger::Never,
    );
    let (tfft_tx_for_log, tfft_rx) = tokio::sync::oneshot::channel();
    let _ = tfft_tx_for_log.send(());

    let deployment_target = app_state.config().deployment_target.clone();
    let response_logger = LoggerService::builder()
        .app_state(app_state.clone())
        .auth_ctx(auth_ctx)
        .start_time(start_time)
        .start_instant(start_instant)
        .target_url(target_url)
        .request_headers(parts.headers.clone())
        .request_body(body.clone())
        .response_status(response_status)
        .response_body(body_reader)
        .provider(target_provider)
        .tfft_rx(tfft_rx)
        .mapper_ctx(mapper_ctx)
        .router_id(router_id)
        .deployment_target(deployment_target)
        .request_id(request_log_id)
        .response_id(response_log_id)
        .response_created_at(response_received_at)
        .prompt_ctx(prompt_ctx)
        .prompt_header_for_request_log(prompt_header)
        .session_ctx(session_ctx)
        .agent_ctx(req_ctx.agent_context.clone())
        .build();

    if let Some(marker) = parts
        .extensions
        .get::<crate::types::extensions::RequestLogEmitted>()
    {
        marker.mark();
    }

    let app_state = app_state.clone();
    tokio::spawn(
        async move {
            if let Err(e) = response_logger.log().await {
                let error_str = e.as_ref().to_string();
                app_state
                    .0
                    .metrics
                    .error_count
                    .add(1, &[opentelemetry::KeyValue::new("type", error_str)]);
            }
        }
        .instrument(tracing::Span::current()),
    );
}

async fn map_response(
    converter_registry: EndpointConverterRegistry,
    source_endpoint: ApiEndpoint,
    target_endpoint: ApiEndpoint,
    resp: http::Response<crate::types::body::Body>,
) -> Result<Response, ApiError> {
    let mapper_ctx = resp
        .extensions()
        .get::<MapperContext>()
        .cloned()
        .ok_or(InternalError::ExtensionNotFound("MapperContext"))?;
    let is_stream = mapper_ctx.is_stream;
    let anthropic_openai_usage = mapper_ctx.anthropic_openai_usage.clone();
    let bridge_chat_completions = mapper_ctx.unified_responses_bridge_chat_completions_sse;
    let (parts, body) = resp.into_parts();
    let (parts, body) = match mapper_service_hooks::map_response_cursor_responses_branch(
        &mapper_ctx,
        bridge_chat_completions,
        parts,
        body,
    )
    .await?
    {
        mapper_service_hooks::CursorResponsesMapOutcome::Done(resp) => {
            return Ok(resp);
        }
        mapper_service_hooks::CursorResponsesMapOutcome::Continue { parts, body } => (parts, body),
    };

    let (parts, body) = match mapper_service_hooks::map_stream_unified_responses_chat_bridge(
        bridge_chat_completions,
        is_stream,
        parts,
        body,
    )? {
        mapper_service_hooks::UnifiedResponsesChatBridgeMapOutcome::Done(resp) => {
            return Ok(resp);
        }
        mapper_service_hooks::UnifiedResponsesChatBridgeMapOutcome::Continue { parts, body } => {
            (parts, body)
        }
    };

    let is_responses_api = matches!(
        &source_endpoint,
        ApiEndpoint::OpenAI(OpenAI::Responses(_))
            | ApiEndpoint::OpenAICompatible {
                openai_endpoint: OpenAI::Responses(_),
                ..
            }
    );
    if is_responses_api && is_stream {
        tracing::trace!("responses API stream passthrough (direct v1/responses)");
        let mapped_stream = body
            .into_data_stream()
            .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
            .map_ok(|bytes| {
                let mut new_bytes = BytesMut::new();
                new_bytes.put("data: ".as_bytes());
                new_bytes.put(bytes);
                new_bytes.put("\n\n".as_bytes());
                new_bytes.freeze()
            });
        let final_body = axum_core::body::Body::new(reqwest::Body::wrap_stream(mapped_stream));
        let new_resp = Response::from_parts(parts, final_body);
        return Ok(new_resp);
    }

    if mapper_ctx.native_semantic_passthrough && is_stream {
        tracing::trace!("native semantic passthrough (streaming SSE framing)");
        let mapped_stream = body
            .into_data_stream()
            .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
            .map_ok(|bytes| {
                let mut new_bytes = BytesMut::new();
                new_bytes.put("data: ".as_bytes());
                new_bytes.put(bytes);
                new_bytes.put("\n\n".as_bytes());
                new_bytes.freeze()
            });
        let final_body = axum_core::body::Body::new(reqwest::Body::wrap_stream(mapped_stream));
        let new_resp = Response::from_parts(parts, final_body);
        return Ok(new_resp);
    }

    let converter = converter_registry
        .get_converter(&target_endpoint, &source_endpoint)
        .ok_or_else(|| {
            InternalError::InvalidConverter(target_endpoint.clone(), source_endpoint.clone())
        })?;

    let lenient_roles = lenient_openai_chat_roles_for_target_endpoint(&target_endpoint);

    if is_stream {
        tracing::trace!(
            source_endpoint = ?target_endpoint,
            target_endpoint = ?source_endpoint,
            "mapped streaming response"
        );
        let append_done_marker = should_append_openai_sse_done_marker(&target_endpoint);
        // because we are using our custom body type, and we know it was
        // constructed in the dispatcher from either an SSE stream or a
        // stream of bytes, we can safely assume each frame is a single
        // SSE event in this branch
        let mapped_stream = body
            .into_data_stream()
            .map_err(|e| ApiError::StreamError(StreamError::BodyError(e)))
            .try_filter_map({
                let captured_registry = converter_registry.clone();
                let resp_parts = parts.clone();
                let target_endpoint_cloned = target_endpoint.clone();
                let source_endpoint_cloned = source_endpoint.clone();
                let anthropic_openai_usage = anthropic_openai_usage.clone();
                let lenient_roles_stream = lenient_roles;
                move |bytes| {
                    let registry_for_future = captured_registry.clone();
                    let resp_parts = resp_parts.clone();
                    let target_endpoint = target_endpoint_cloned.clone();
                    let source_endpoint = source_endpoint_cloned.clone();
                    let anthropic_usage_for_chunk = anthropic_openai_usage.clone();
                    async move {
                        let converter = registry_for_future
                            .get_converter(&target_endpoint, &source_endpoint)
                            .ok_or_else(|| {
                                InternalError::InvalidConverter(
                                    target_endpoint.clone(),
                                    source_endpoint.clone(),
                                )
                            })?;

                        let converted_data = converter.convert_resp_body(
                            resp_parts,
                            bytes,
                            is_stream,
                            anthropic_usage_for_chunk.as_ref(),
                            lenient_roles_stream,
                        )?;

                        // add the `data: ` prefix expected by the OpenAI SDK
                        if let Some(converted_data) = converted_data {
                            let mut new_bytes = BytesMut::new();
                            new_bytes.put("data: ".as_bytes());
                            new_bytes.put(converted_data);
                            new_bytes.put("\n\n".as_bytes());
                            let data = new_bytes.freeze();
                            Ok(Some(data))
                        } else {
                            Ok(converted_data)
                        }
                    }
                }
            })
            .chain(futures::stream::iter(append_done_marker.then(|| {
                Ok::<Bytes, ApiError>(Bytes::from_static(b"data: [DONE]\n\n"))
            })));
        let final_body = axum_core::body::Body::new(reqwest::Body::wrap_stream(mapped_stream));
        let new_resp = Response::from_parts(parts, final_body);
        Ok(new_resp)
    } else {
        use http_body_util::BodyExt;
        let body_bytes = body
            .collect()
            .await
            .map_err(InternalError::CollectBodyError)?
            .to_bytes();

        let mapped_body_bytes = if bridge_chat_completions {
            mapper_service_hooks::map_non_stream_unified_responses_chat_bridge(&body_bytes)?
        } else if mapper_ctx.native_semantic_passthrough {
            body_bytes
        } else {
            converter
                .convert_resp_body(parts.clone(), body_bytes, is_stream, None, lenient_roles)?
                .ok_or(MapperError::EmptyResponseBody)
                .map_err(InternalError::MapperError)?
        };

        let final_body = axum_core::body::Body::from(mapped_body_bytes);
        let new_resp = Response::from_parts(parts, final_body);
        tracing::trace!(
            source_endpoint = ?target_endpoint,
            target_endpoint = ?source_endpoint,
            "mapped non-streaming response"
        );
        Ok(new_resp)
    }
}

fn should_append_openai_sse_done_marker(endpoint: &ApiEndpoint) -> bool {
    matches!(
        endpoint,
        ApiEndpoint::OpenAI(OpenAI::ChatCompletions(_) | OpenAI::Completions(_))
            | ApiEndpoint::OpenAICompatible {
                openai_endpoint: OpenAI::ChatCompletions(_) | OpenAI::Completions(_),
                ..
            }
    )
}

#[derive(Debug, Clone)]
pub struct Layer {
    endpoint_converter_registry: EndpointConverterRegistry,
    app_state: AppState,
}

impl Layer {
    #[must_use]
    pub fn new(
        endpoint_converter_registry: EndpointConverterRegistry,
        app_state: AppState,
    ) -> Self {
        Self {
            endpoint_converter_registry,
            app_state,
        }
    }
}

impl<S> tower::Layer<S> for Layer {
    type Service = Service<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Service::new(
            inner,
            self.endpoint_converter_registry.clone(),
            self.app_state.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::uri::PathAndQuery;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};

    use crate::{
        app::build_test_app,
        config::Config,
        endpoints::{
            ApiEndpoint, anthropic::Anthropic, bedrock::Bedrock, google::Google, ollama::Ollama,
            openai::OpenAI,
        },
        middleware::mapper::{
            envelope::RequestEnvelope, model::ModelMapper, registry::EndpointConverterRegistry,
        },
        types::{
            extensions::{
                MapperContext, MapperProfileContext, MasterKeyUnifiedModelPassthrough,
                PromptCompressionTokenPair, UnifiedModelBodyPassthrough, UnifiedModelPolicyChecked,
            },
            provider::InferenceProvider,
        },
    };

    #[test]
    fn mapper_model_policy_is_enforced_without_passthrough_markers() {
        let extensions = http::Extensions::new();

        assert!(super::should_enforce_mapper_model_policy(&extensions));
    }

    #[test]
    fn mapper_model_policy_is_skipped_for_master_key_model_passthrough() {
        let mut extensions = http::Extensions::new();
        extensions.insert(MasterKeyUnifiedModelPassthrough);

        assert!(!super::should_enforce_mapper_model_policy(&extensions));
    }

    #[test]
    fn mapper_model_policy_is_skipped_for_unified_policy_checked() {
        let mut extensions = http::Extensions::new();
        extensions.insert(UnifiedModelPolicyChecked);

        assert!(!super::should_enforce_mapper_model_policy(&extensions));
    }

    #[test]
    fn mapper_model_policy_is_skipped_when_both_passthrough_markers_exist() {
        let mut extensions = http::Extensions::new();
        extensions.insert(MasterKeyUnifiedModelPassthrough);
        extensions.insert(UnifiedModelPolicyChecked);

        assert!(!super::should_enforce_mapper_model_policy(&extensions));
    }

    #[test]
    fn should_skip_semantic_envelope_false_without_passthrough_markers() {
        let extensions = http::Extensions::new();

        assert!(!super::should_skip_semantic_envelope(&extensions, false));
    }

    #[test]
    fn should_skip_semantic_envelope_true_for_native_semantic_passthrough() {
        let extensions = http::Extensions::new();

        assert!(super::should_skip_semantic_envelope(&extensions, true));
    }

    #[test]
    fn should_skip_semantic_envelope_true_for_master_key_model_passthrough() {
        let mut extensions = http::Extensions::new();
        extensions.insert(MasterKeyUnifiedModelPassthrough);

        assert!(super::should_skip_semantic_envelope(&extensions, false));
    }

    #[test]
    fn should_skip_semantic_envelope_false_for_unified_body_passthrough() {
        let mut extensions = http::Extensions::new();
        extensions.insert(UnifiedModelBodyPassthrough);

        assert!(!super::should_skip_semantic_envelope(&extensions, false));
    }

    #[test]
    fn should_skip_semantic_envelope_true_when_master_key_and_body_passthrough_markers_exist() {
        let mut extensions = http::Extensions::new();
        extensions.insert(MasterKeyUnifiedModelPassthrough);
        extensions.insert(UnifiedModelBodyPassthrough);

        assert!(super::should_skip_semantic_envelope(&extensions, false));
    }

    #[test]
    fn unified_passthrough_body_model_strips_matching_provider_prefix() {
        let body = Bytes::from_static(br#"{"model":"deepseek/deepseek-reasoner","messages":[]}"#);

        let normalized = super::normalize_unified_passthrough_body_model(
            &body,
            InferenceProvider::Named("deepseek".into()),
        )
        .expect("body should normalize");
        let value: Value = serde_json::from_slice(&normalized).expect("json body");

        assert_eq!(value["model"], "deepseek-reasoner");
    }

    #[test]
    fn unified_passthrough_body_model_keeps_cross_provider_openrouter_style_model() {
        let body = Bytes::from_static(br#"{"model":"anthropic/claude-sonnet-4.6","messages":[]}"#);

        let normalized = super::normalize_unified_passthrough_body_model(
            &body,
            InferenceProvider::Named("openrouter".into()),
        )
        .expect("body should normalize");
        let value: Value = serde_json::from_slice(&normalized).expect("json body");

        assert_eq!(value["model"], "anthropic/claude-sonnet-4.6");
    }

    #[test]
    fn unified_model_body_passthrough_supported_for_openai_family() {
        assert!(super::supports_unified_model_body_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
        ));
        assert!(super::supports_unified_model_body_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::responses()),
            &ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named("openrouter".into()),
                openai_endpoint: OpenAI::responses(),
            },
        ));
    }

    #[test]
    fn unified_model_body_passthrough_supported_for_semantic_targets() {
        assert!(super::supports_unified_model_body_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::Anthropic(Anthropic::messages()),
        ));
        assert!(super::supports_unified_model_body_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::Google(Google::generate_contents()),
        ));
        assert!(super::supports_unified_model_body_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::Bedrock(Bedrock::converse()),
        ));
        assert!(super::supports_unified_model_body_passthrough(
            &ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            &ApiEndpoint::Ollama(Ollama::chat_completions()),
        ));
    }

    async fn map_unified_body_passthrough(
        source_endpoint: ApiEndpoint,
        target_endpoint: ApiEndpoint,
        path: &'static str,
        body: Value,
    ) -> Value {
        map_unified_body_passthrough_with_path(source_endpoint, target_endpoint, path, body)
            .await
            .0
    }

    async fn map_unified_body_passthrough_with_path(
        source_endpoint: ApiEndpoint,
        target_endpoint: ApiEndpoint,
        path: &'static str,
        body: Value,
    ) -> (Value, String) {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let request_body = Bytes::from(serde_json::to_vec(&body).expect("request body"));
        let mut request = http::Request::builder()
            .method(http::Method::POST)
            .uri(format!("http://router.alephant.test{path}"))
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");
        request.extensions_mut().insert(UnifiedModelPolicyChecked);
        request.extensions_mut().insert(UnifiedModelBodyPassthrough);

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static(path),
            request,
        )
        .await
        .expect("map request should succeed");

        let target_path = mapped
            .extensions()
            .get::<PathAndQuery>()
            .expect("mapped request should include target path")
            .to_string();
        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        (
            serde_json::from_slice(&upstream_bytes).expect("mapped body should be valid json"),
            target_path,
        )
    }

    #[tokio::test]
    async fn openai_compatible_streaming_response_appends_done_marker() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let upstream_endpoint = ApiEndpoint::OpenAICompatible {
            provider: InferenceProvider::Named("openrouter".into()),
            openai_endpoint: OpenAI::chat_completions(),
        };
        let client_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let upstream_chunk = Bytes::from_static(
            br#"{"id":"gen","choices":[{"index":0,"delta":{"content":"hi","function_call":null,"tool_calls":null,"role":"assistant","refusal":null},"finish_reason":null,"logprobs":null}],"created":1,"model":"openai/gpt-4o-mini","service_tier":null,"system_fingerprint":null,"object":"chat.completion.chunk","usage":null}"#,
        );

        let mut resp = http::Response::builder()
            .status(http::StatusCode::OK)
            .body(axum_core::body::Body::from(upstream_chunk))
            .expect("response should build");
        resp.extensions_mut().insert(MapperContext {
            is_stream: true,
            model: None,
            anthropic_openai_usage: None,
            unified_responses_bridge_chat_completions_sse: false,
            native_semantic_passthrough: false,
            cursor_responses_via_chat_completions: false,
            cursor_responses_origin: None,
            client_expects_responses_wire: false,
        });

        let mapped = super::map_response(registry, upstream_endpoint, client_endpoint, resp)
            .await
            .expect("streaming response should map");
        let (_, body) = mapped.into_parts();
        let bytes = body
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let body_text = std::str::from_utf8(&bytes).expect("mapped SSE should be utf8");

        assert!(body_text.contains("data: {"));
        assert!(body_text.ends_with("data: [DONE]\n\n"));
        assert_eq!(body_text.matches("[DONE]").count(), 1);
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_preserves_model_and_stream_usage() {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            "/v1/chat/completions",
            json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": true
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "gpt-4o");
        assert_eq!(upstream_body["stream_options"]["include_usage"], true);
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_family_preserves_models() {
        let completions = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::completions()),
            ApiEndpoint::OpenAI(OpenAI::completions()),
            "/v1/completions",
            json!({
                "model": "future-completions-model",
                "prompt": "hello"
            }),
        )
        .await;
        assert_eq!(completions["model"], "future-completions-model");

        let embeddings = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::embeddings()),
            ApiEndpoint::OpenAI(OpenAI::embeddings()),
            "/v1/embeddings",
            json!({
                "model": "future-embedding-model",
                "input": "hello"
            }),
        )
        .await;
        assert_eq!(embeddings["model"], "future-embedding-model");

        let responses = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::responses()),
            ApiEndpoint::OpenAI(OpenAI::responses()),
            "/v1/responses",
            json!({
                "model": "future-responses-model",
                "input": "hello",
                "stream_options": {"include_usage": true}
            }),
        )
        .await;
        assert_eq!(responses["model"], "future-responses-model");
        assert!(responses.get("stream_options").is_none());
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_compatible_chat_preserves_unknown_model_and_max_tokens()
     {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named("openrouter".into()),
                openai_endpoint: OpenAI::chat_completions(),
            },
            "/v1/chat/completions",
            json!({
                "model": "future-openrouter-model",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "future-openrouter-model");
        assert_eq!(upstream_body["max_completion_tokens"], 128);
        assert!(upstream_body["max_tokens"].is_null());
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_compatible_chat_strips_matching_provider_prefix()
    {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named("deepseek".into()),
                openai_endpoint: OpenAI::chat_completions(),
            },
            "/v1/chat/completions",
            json!({
                "model": "deepseek/deepseek-chat",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "deepseek-chat");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_compatible_responses_preserves_unknown_model() {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::responses()),
            ApiEndpoint::OpenAICompatible {
                provider: InferenceProvider::Named("openrouter".into()),
                openai_endpoint: OpenAI::responses(),
            },
            "/v1/responses",
            json!({
                "model": "future-openrouter-responses-model",
                "input": "hello"
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "future-openrouter-responses-model");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_anthropic_preserves_model_and_converts_messages()
     {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Anthropic(Anthropic::messages()),
            "/v1/chat/completions",
            json!({
                "model": "future-anthropic-model",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "future-anthropic-model");
        assert_eq!(upstream_body["max_tokens"], 128);
        assert_eq!(upstream_body["messages"][0]["role"], "user");
        assert_eq!(upstream_body["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_google_preserves_model_and_messages() {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Google(Google::generate_contents()),
            "/v1/chat/completions",
            json!({
                "model": "future-google-model",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "future-google-model");
        assert_eq!(upstream_body["messages"][0]["role"], "user");
        assert_eq!(upstream_body["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_bedrock_preserves_model_id_and_messages()
    {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Bedrock(Bedrock::converse()),
            "/v1/chat/completions",
            json!({
                "model": "anthropic.future-bedrock-model-v1:0",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;

        assert_eq!(
            upstream_body["modelId"],
            "anthropic.future-bedrock-model-v1:0"
        );
        assert_eq!(upstream_body["messages"][0]["role"], "user");
        assert_eq!(upstream_body["messages"][0]["content"][0]["text"], "hello");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_bedrock_allows_raw_model_id() {
        let (upstream_body, target_path) = map_unified_body_passthrough_with_path(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Bedrock(Bedrock::converse()),
            "/v1/chat/completions",
            json!({
                "model": "claude-sonnet-4.6",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;

        assert_eq!(upstream_body["modelId"], "claude-sonnet-4.6");
        assert_eq!(target_path, "/model/claude-sonnet-4.6/converse");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_bedrock_strips_provider_prefix_and_keeps_thinking()
     {
        let (upstream_body, target_path) = map_unified_body_passthrough_with_path(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Bedrock(Bedrock::converse()),
            "/v1/chat/completions",
            json!({
                "model": "bedrock/anthropic.claude-3-5-sonnet-20240620-v1:0",
                "messages": [{"role": "user", "content": "hello"}],
                "max_completion_tokens": 2048,
                "reasoning_effort": "high"
            }),
        )
        .await;

        assert_eq!(
            upstream_body["modelId"],
            "anthropic.claude-3-5-sonnet-20240620-v1:0"
        );
        assert_eq!(
            target_path,
            "/model/anthropic.claude-3-5-sonnet-20240620-v1:0/converse"
        );
        assert_eq!(
            upstream_body["additionalModelRequestFields"]["object"]["thinking"]["object"]["type"]["string"],
            "enabled"
        );
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_bedrock_encodes_slash_model_path() {
        let (upstream_body, target_path) = map_unified_body_passthrough_with_path(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Bedrock(Bedrock::converse()),
            "/v1/chat/completions",
            json!({
                "model": "anthropic/claude-sonnet-4.6",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;

        assert_eq!(upstream_body["modelId"], "anthropic/claude-sonnet-4.6");
        assert_eq!(target_path, "/model/anthropic%2Fclaude-sonnet-4.6/converse");
    }

    #[tokio::test]
    async fn unified_model_body_passthrough_openai_chat_to_ollama_preserves_model_and_messages() {
        let upstream_body = map_unified_body_passthrough(
            ApiEndpoint::OpenAI(OpenAI::chat_completions()),
            ApiEndpoint::Ollama(Ollama::chat_completions()),
            "/v1/chat/completions",
            json!({
                "model": "future-ollama-model",
                "messages": [{"role": "user", "content": "hello"}]
            }),
        )
        .await;

        assert_eq!(upstream_body["model"], "future-ollama-model");
        assert_eq!(upstream_body["messages"][0]["role"], "user");
        assert_eq!(upstream_body["messages"][0]["content"], "hello");
    }

    #[tokio::test]
    async fn map_request_runs_post_policy_prompt_compression_on_chat_completions() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("qwen".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider).expect("mapped endpoint");

        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "qwen/qwen3-32b",
                "messages": [
                    { "role": "user", "content": "  a   b  " },
                ],
            }))
            .expect("request body should serialize"),
        );

        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/v1/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/v1/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        assert!(
            mapped
                .extensions()
                .get::<PromptCompressionTokenPair>()
                .is_some(),
            "post-policy compression should set PromptCompressionTokenPair"
        );

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value =
            serde_json::from_slice(&upstream_bytes).expect("mapped body should be valid json");
        assert_eq!(upstream_body["messages"][0]["content"], "a b");
    }

    #[tokio::test]
    async fn unified_body_passthrough_keeps_envelope_and_converter() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("openrouter".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider).expect("mapped endpoint");

        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "claude-sonnet-4.6",
                "messages": [
                    { "role": "user", "content": "hello" },
                ],
                "max_tokens": 256
            }))
            .expect("request body should serialize"),
        );

        let mut request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/v1/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");
        request.extensions_mut().insert(UnifiedModelPolicyChecked);
        request.extensions_mut().insert(UnifiedModelBodyPassthrough);

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/v1/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        assert!(
            mapped.extensions().get::<RequestEnvelope>().is_some(),
            "unified body passthrough should keep semantic envelope for rules",
        );
        let mapper_ctx = mapped
            .extensions()
            .get::<MapperContext>()
            .expect("mapper context should still be attached");
        assert_eq!(
            mapper_ctx.model.as_ref().map(ToString::to_string),
            Some("claude-sonnet-4.6".to_string())
        );

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value =
            serde_json::from_slice(&upstream_bytes).expect("mapped body should be valid json");

        assert_eq!(upstream_body["model"], "claude-sonnet-4.6");
        assert_eq!(upstream_body["max_completion_tokens"], 256);
        assert!(upstream_body["max_tokens"].is_null());
    }

    #[tokio::test]
    async fn map_request_applies_shared_request_rules_before_converter() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("qwen".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider).expect("mapped endpoint");
        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "qwen/qwen3-32b",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request body should serialize"),
        );
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/v1/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/v1/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        let envelope = mapped
            .extensions()
            .get::<RequestEnvelope>()
            .expect("request envelope should be attached");
        assert!(envelope.request_rule_context.is_some());
        assert!(envelope.openai_request.reasoning_effort.is_none());

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value =
            serde_json::from_slice(&upstream_bytes).expect("mapped body should be valid json");

        assert_eq!(upstream_body["model"], "qwen3-32b");
        assert!(upstream_body["reasoning_effort"].is_null());
    }

    #[tokio::test]
    async fn map_request_preserves_reasoning_effort_for_deepseek_reasoner() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("deepseek".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider).expect("mapped endpoint");
        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "deepseek/deepseek-reasoner",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ],
                "reasoning_effort": "high"
            }))
            .expect("request body should serialize"),
        );
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/v1/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/v1/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        let envelope = mapped
            .extensions()
            .get::<RequestEnvelope>()
            .expect("request envelope should be attached");
        assert_eq!(
            envelope.openai_request.reasoning_effort,
            Some(async_openai::types::ReasoningEffort::High)
        );

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value =
            serde_json::from_slice(&upstream_bytes).expect("mapped body should be valid json");

        assert_eq!(upstream_body["model"], "deepseek-reasoner");
        assert_eq!(upstream_body["reasoning_effort"], json!("high"));
    }

    #[tokio::test]
    async fn map_request_attaches_mapper_profile_context() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::OpenAI(OpenAI::chat_completions());
        let provider = InferenceProvider::Named("deepseek".into());
        let target_endpoint =
            ApiEndpoint::mapped(source_endpoint.clone(), &provider).expect("mapped endpoint");
        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "deepseek/deepseek-reasoner",
                "messages": [
                    {
                        "role": "user",
                        "content": "hello"
                    }
                ]
            }))
            .expect("request body should serialize"),
        );
        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/v1/chat/completions")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/v1/chat/completions"),
            request,
        )
        .await
        .expect("map request should succeed");

        let profile_context = mapped
            .extensions()
            .get::<MapperProfileContext>()
            .expect("mapper profile context should be attached");

        assert_eq!(profile_context.provider, provider);
        assert_eq!(profile_context.raw_model, "deepseek/deepseek-reasoner");
    }

    #[tokio::test]
    async fn map_request_native_semantic_passthrough_skips_envelope_for_claude_cli() {
        use crate::{
            endpoints::anthropic::Anthropic,
            ide_adapation::client_profile::{ClientProfile, ClientProfileResolution},
            types::extensions::{MapperContext, NativeSemanticPassthrough},
        };

        let app = build_test_app(Config::default()).await.expect("build app");
        let model_mapper = ModelMapper::new(app.state.clone());
        let registry = EndpointConverterRegistry::new(&model_mapper);
        let source_endpoint = ApiEndpoint::Anthropic(Anthropic::messages());
        let target_endpoint = ApiEndpoint::Anthropic(Anthropic::messages());

        let request_body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "claude-3-5-haiku-20241022",
                "max_tokens": 256,
                "messages": [{"role": "user", "content": "Hello"}],
                "stream": false
            }))
            .expect("request body should serialize"),
        );

        let request = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://router.alephant.test/v1/messages")
            .header("User-Agent", "claude-cli/1.0")
            .body(axum_core::body::Body::from(request_body))
            .expect("request should build");

        let mapped = super::map_request(
            app.state.clone(),
            registry,
            source_endpoint,
            target_endpoint,
            &PathAndQuery::from_static("/v1/messages"),
            request,
        )
        .await
        .expect("map request should succeed");

        assert!(
            mapped.extensions().get::<RequestEnvelope>().is_none(),
            "envelope should be skipped on native semantic passthrough",
        );
        let resolution = mapped
            .extensions()
            .get::<ClientProfileResolution>()
            .expect("client profile resolution");
        assert_eq!(resolution.profile, ClientProfile::ClaudeCode);
        assert!(!resolution.from_explicit_header);
        assert!(
            mapped
                .extensions()
                .get::<NativeSemanticPassthrough>()
                .is_some()
        );
        let mapper_ctx = mapped
            .extensions()
            .get::<MapperContext>()
            .expect("mapper ctx");
        assert!(mapper_ctx.native_semantic_passthrough);

        let (_, body) = mapped.into_parts();
        let upstream_bytes = body
            .collect()
            .await
            .expect("mapped body should collect")
            .to_bytes();
        let upstream_body: Value =
            serde_json::from_slice(&upstream_bytes).expect("mapped body should be valid json");
        assert_eq!(upstream_body["model"], json!("claude-3-5-haiku-20241022"));
    }
}
