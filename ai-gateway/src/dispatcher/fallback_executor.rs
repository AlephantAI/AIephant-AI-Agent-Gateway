use std::str::FromStr;

use bytes::Bytes;
use http::HeaderMap;
use tokio::sync::{mpsc, oneshot};

use crate::{
    app_state::AppState,
    default_model::choose_default_gateway_model_excluding_provider,
    dispatcher::{
        client::Client,
        provider_allowlist::enforce_workspace_provider_allowlist,
        request_builder::request_builder_with_effective_host,
        sync_dispatch,
        target_endpoint::{TargetEndpointRequest, TargetEndpointResolver},
        upstream_auth::{UpstreamAuthApplier, UpstreamAuthRequest},
    },
    error::{
        api::ApiError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    middleware::model_support::split_provider_model,
    types::{
        body::{Body, BodyReader},
        extensions::{
            RequestContext, UnifiedImplicitModelFallbackContext, VkPolicy,
        },
        model_id::ModelId,
        provider::InferenceProvider,
    },
};

pub(super) struct FallbackExecutor<'a> {
    app_state: &'a AppState,
}

pub(super) struct CrossProviderFallbackRequest<'a> {
    pub(super) req_ctx: &'a RequestContext,
    pub(super) method: &'a http::Method,
    pub(super) headers: &'a HeaderMap,
    pub(super) extracted_path_and_query: &'a str,
    pub(super) vk_policy: Option<&'a VkPolicy>,
    pub(super) implicit_model_fallback_ctx:
        Option<&'a UnifiedImplicitModelFallbackContext>,
    pub(super) req_body_bytes: &'a Bytes,
    pub(super) cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
}

pub(super) struct CrossProviderFallbackOutcome {
    pub(super) response: http::Response<Body>,
    pub(super) response_body_for_logger: BodyReader,
    pub(super) tfft_rx: oneshot::Receiver<()>,
    pub(super) effective_provider: InferenceProvider,
    pub(super) effective_target_url: url::Url,
    pub(super) effective_request_body: Bytes,
}

impl<'a> FallbackExecutor<'a> {
    #[must_use]
    pub(super) fn new(app_state: &'a AppState) -> Self {
        Self { app_state }
    }

    pub(super) async fn try_cross_provider_default_model_fallback(
        &self,
        request: CrossProviderFallbackRequest<'_>,
    ) -> Result<Option<CrossProviderFallbackOutcome>, ApiError> {
        let Some(auth_ctx) = request.req_ctx.auth_context.as_ref() else {
            return Ok(None);
        };
        let Some(vk_policy) = request.vk_policy else {
            return Ok(None);
        };
        let Some(implicit_ctx) = request.implicit_model_fallback_ctx else {
            return Ok(None);
        };
        let Ok(parsed_current) =
            split_provider_model(&implicit_ctx.selected_model)
        else {
            return Ok(None);
        };
        let fallback_model =
            match choose_default_gateway_model_excluding_provider(
                self.app_state,
                "chat/completions",
                auth_ctx,
                vk_policy,
                Some(parsed_current.provider_raw),
            )
            .await
            {
                Ok(model) => model,
                Err(ApiError::InvalidRequest(
                    InvalidRequestError::NoModelAvailable,
                )) => return Ok(None),
                Err(err) => return Err(err),
            };
        if fallback_model.eq_ignore_ascii_case(&implicit_ctx.selected_model) {
            return Ok(None);
        }
        enforce_workspace_provider_allowlist(
            self.app_state,
            request.req_ctx.auth_context.as_ref(),
            &inference_provider_from_gateway_model(&fallback_model)?,
        )?;
        let (fallback_provider, fallback_target_url, fallback_body) = self
            .cross_provider_fallback_request_details(
                request.req_ctx,
                request.extracted_path_and_query,
                request.req_body_bytes,
                &fallback_model,
            )
            .await?;
        let fallback_client =
            Client::new(self.app_state, fallback_provider.clone())
                .await
                .map_err(|err| {
                    tracing::error!(
                        error = %err,
                        provider = %fallback_provider,
                        "failed to build fallback client"
                    );
                    ApiError::Internal(InternalError::Internal)
                })?;
        let fallback_request_builder = fallback_client
            .as_ref()
            .request(request.method.clone(), fallback_target_url.clone())
            .headers(request.headers.clone());
        let fallback_request_builder = request_builder_with_effective_host(
            fallback_request_builder,
            &fallback_target_url,
        );
        let fallback_request_builder = UpstreamAuthApplier::new(self.app_state)
            .apply(UpstreamAuthRequest {
                client: &fallback_client,
                request_builder: fallback_request_builder,
                req_body_bytes: &fallback_body,
                auth_context: request.req_ctx.auth_context.as_ref(),
                provider: fallback_provider.clone(),
            })
            .await?;
        crate::fallback::observability::log_decision(
            &self.app_state.config().fallback_policy,
            crate::fallback::observability::DecisionKind::CrossProviderFallback,
            None,
            &fallback_provider,
        );
        let (response, response_body_for_logger, tfft_rx) =
            sync_dispatch::dispatch_sync(
                &fallback_request_builder,
                fallback_body.clone(),
                request.cache_tap,
            )
            .await?;
        Ok(Some(CrossProviderFallbackOutcome {
            response,
            response_body_for_logger,
            tfft_rx,
            effective_provider: fallback_provider,
            effective_target_url: fallback_target_url,
            effective_request_body: fallback_body,
        }))
    }

    pub(super) async fn cross_provider_fallback_request_details(
        &self,
        req_ctx: &RequestContext,
        extracted_path_and_query: &str,
        req_body_bytes: &Bytes,
        fallback_model: &str,
    ) -> Result<(InferenceProvider, url::Url, Bytes), ApiError> {
        let fallback_provider =
            inference_provider_from_gateway_model(fallback_model)?;
        let fallback_target_url =
            TargetEndpointResolver::new(self.app_state.clone())
                .resolve(TargetEndpointRequest {
                    request_context: req_ctx,
                    target_provider: &fallback_provider,
                    path_and_query: extracted_path_and_query,
                    allow_learned_region: true,
                })
                .await?
                .url;
        let fallback_body =
            rewrite_chat_completion_model(req_body_bytes, fallback_model)?;
        Ok((fallback_provider, fallback_target_url, fallback_body))
    }
}

fn rewrite_chat_completion_model(
    body: &Bytes,
    new_model: &str,
) -> Result<Bytes, ApiError> {
    let mut value: serde_json::Value = serde_json::from_slice(body)
        .map_err(InvalidRequestError::InvalidRequestBody)?;
    value["model"] = serde_json::Value::String(new_model.to_string());
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|err| InvalidRequestError::InvalidRequestBody(err).into())
}

fn inference_provider_from_gateway_model(
    model: &str,
) -> Result<InferenceProvider, ApiError> {
    let source_model =
        ModelId::from_str(model).map_err(InternalError::MapperError)?;
    match source_model {
        ModelId::ModelIdWithVersion { provider, .. } => Ok(provider),
        ModelId::Bedrock(_) => Ok(InferenceProvider::Bedrock),
        ModelId::Ollama(_) => Ok(InferenceProvider::Ollama),
        ModelId::Unknown(_) => {
            Err(InvalidRequestError::UnsupportedEndpoint(format!(
                "provider for the given model: '{source_model}' not supported"
            ))
            .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_chat_completion_model_replaces_model_field() {
        let body = Bytes::from(
            serde_json::json!({
                "model": "openai/gpt-5.4",
                "messages": [{"role": "user", "content": "hi"}]
            })
            .to_string(),
        );

        let rewritten =
            rewrite_chat_completion_model(&body, "google/gemini-2.5-pro")
                .expect("rewrite body");
        let value: serde_json::Value =
            serde_json::from_slice(&rewritten).expect("json body");
        assert_eq!(
            value.get("model").and_then(serde_json::Value::as_str),
            Some("google/gemini-2.5-pro")
        );
    }
}
