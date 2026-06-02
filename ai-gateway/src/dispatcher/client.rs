use bytes::Bytes;
use futures::StreamExt;
use http_body_util::BodyExt;
use reqwest::RequestBuilder;
use reqwest_eventsource::{Event, EventSource, RequestBuilderExt};
use tracing::{Instrument, info_span};

use crate::{
    app_state::AppState,
    config::deployment_target::MasterKeyResolution,
    discover::monitor::metrics::EndpointMetricsRegistry,
    dispatcher::{
        SSEStream, anthropic_client::Client as AnthropicClient,
        bedrock_client::Client as BedrockClient,
        ollama_client::Client as OllamaClient,
        openai_compatible_client::Client as OpenAICompatibleClient,
    },
    endpoints::ApiEndpoint,
    error::{
        api::ApiError, auth::AuthError, init::InitError,
        internal::InternalError, stream::StreamError,
    },
    types::{
        extensions::AuthContext,
        provider::{InferenceProvider, ProviderKey},
        secret::Secret,
    },
};

pub trait ProviderClient {
    async fn authenticate(
        &self,
        app_state: &AppState,
        request_builder: reqwest::RequestBuilder,
        req_body_bytes: &bytes::Bytes,
        auth_ctx: Option<&AuthContext>,
        provider: InferenceProvider,
    ) -> Result<reqwest::RequestBuilder, ApiError>;
}

impl ProviderClient for Client {
    async fn authenticate(
        &self,
        app_state: &AppState,
        request_builder: reqwest::RequestBuilder,
        req_body_bytes: &bytes::Bytes,
        auth_ctx: Option<&AuthContext>,
        provider: InferenceProvider,
    ) -> Result<reqwest::RequestBuilder, ApiError> {
        match self {
            Client::Bedrock(inner) => {
                let provider_key =
                    resolve_bedrock_provider_key(app_state, auth_ctx).await?;
                inner.extract_and_sign_aws_headers(
                    request_builder,
                    req_body_bytes,
                    &provider_key,
                )
            }
            Client::OpenAICompatible(_) | Client::Anthropic(_) => {
                self.authenticate_inner(
                    app_state,
                    request_builder,
                    auth_ctx,
                    provider,
                )
                .await
            }
            Client::Ollama(_) => Ok(request_builder),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Client {
    OpenAICompatible(OpenAICompatibleClient),
    Anthropic(AnthropicClient),
    Ollama(OllamaClient),
    Bedrock(BedrockClient),
}

impl Client {
    /// `compat_mode` with Alephant auth off: no `AuthContext`; use provider API
    /// keys from the environment (same names as typical SDKs).
    fn authenticate_compat_env(
        &self,
        request_builder: reqwest::RequestBuilder,
        provider: &InferenceProvider,
    ) -> Result<reqwest::RequestBuilder, ApiError> {
        let Some(key) = compat_provider_api_key_from_env(provider) else {
            return Err(ApiError::Authentication(
                AuthError::ProviderKeyNotFound,
            ));
        };

        match self {
            Client::OpenAICompatible(c) => {
                Ok(OpenAICompatibleClient::set_auth_header(
                    request_builder,
                    &key,
                    c.upstream_auth,
                ))
            }
            Client::Anthropic(_) => {
                Ok(AnthropicClient::set_auth_header(request_builder, &key))
            }
            Client::Ollama(_) => Ok(request_builder),
            Client::Bedrock(_) => {
                Err(ApiError::Authentication(AuthError::ProviderKeyNotFound))
            }
        }
    }

    async fn authenticate_inner(
        &self,
        app_state: &AppState,
        request_builder: reqwest::RequestBuilder,
        auth_ctx: Option<&AuthContext>,
        provider: InferenceProvider,
    ) -> Result<reqwest::RequestBuilder, ApiError> {
        if auth_ctx.is_none() && app_state.config().compat_mode {
            return self.authenticate_compat_env(request_builder, &provider);
        }

        let Some(auth_ctx) = auth_ctx else {
            return Err(ApiError::Authentication(
                AuthError::ProviderKeyNotFound,
            ));
        };

        let Some(master_key_id) = auth_ctx.master_key_id else {
            return Err(ApiError::Authentication(
                AuthError::ProviderKeyNotFound,
            ));
        };

        let cache = app_state
            .0
            .master_key_cache
            .as_ref()
            .ok_or(ApiError::Internal(InternalError::Internal))?;

        let workspace_id = *auth_ctx.org_id.as_ref();
        let master_key_resolution =
            app_state.0.config.deployment_target.master_key_resolution;
        let fallback_enabled =
            fallback_enabled_for_master_key_resolution(master_key_resolution);

        let decrypted = cache
            .get_primary_or_fallback(
                master_key_id,
                workspace_id,
                provider.clone(),
                fallback_enabled,
            )
            .await
            .map_err(|e| {
                tracing::warn!(
                    error = %e,
                    %master_key_id,
                    "dispatcher: failed to resolve master_key"
                );
                ApiError::Authentication(AuthError::ProviderKeyNotFound)
            })?;

        if !master_key_provider_matches(&decrypted.provider, &provider) {
            tracing::warn!(
                master_provider = ?decrypted.provider,
                target_provider = ?provider,
                %master_key_id,
                "dispatcher: master_key provider mismatch"
            );
            return Err(ApiError::Authentication(
                AuthError::ProviderKeyNotFound,
            ));
        }

        let key = Secret::from(decrypted.plaintext.as_ref().clone());
        let request_builder = match self {
            Client::OpenAICompatible(c) => {
                OpenAICompatibleClient::set_auth_header(
                    request_builder,
                    &key,
                    c.upstream_auth,
                )
            }
            Client::Anthropic(_) => {
                AnthropicClient::set_auth_header(request_builder, &key)
            }
            _ => request_builder,
        };
        Ok(request_builder)
    }

    pub(crate) async fn sse_stream<B>(
        request_builder: RequestBuilder,
        body: B,
        api_endpoint: Option<ApiEndpoint>,
        metrics_registry: &EndpointMetricsRegistry,
    ) -> Result<SSEStream, ApiError>
    where
        B: Into<reqwest::Body>,
    {
        let event_source = request_builder
            .body(body)
            .eventsource()
            .map_err(|_e| InternalError::Internal)?;
        let stream =
            sse_stream(event_source, api_endpoint, metrics_registry.clone())
                .await?;
        Ok(stream)
    }

    pub(crate) async fn new(
        app_state: &AppState,
        inference_provider: InferenceProvider,
    ) -> Result<Self, InitError> {
        if inference_provider == InferenceProvider::Ollama {
            return Self::new_inner(app_state, inference_provider, None);
        }
        let api_key = &app_state
            .0
            .provider_keys
            .get_provider_key(&inference_provider, None)
            .await;

        Self::new_inner(app_state, inference_provider, api_key.as_ref())
    }

    fn new_inner(
        app_state: &AppState,
        inference_provider: InferenceProvider,
        api_key: Option<&ProviderKey>,
    ) -> Result<Self, InitError> {
        // connection timeout, timeout, etc.
        let base_client = reqwest::Client::builder()
            .connect_timeout(app_state.0.config.dispatcher.connection_timeout)
            .timeout(app_state.0.config.dispatcher.timeout)
            .tcp_nodelay(true);

        match inference_provider {
            InferenceProvider::OpenAI
            | InferenceProvider::GoogleGemini
            | InferenceProvider::Custom
            | InferenceProvider::Named(_) => {
                let openai_compatible_client = OpenAICompatibleClient::new(
                    app_state,
                    base_client,
                    inference_provider,
                    api_key,
                )?;
                Ok(Self::OpenAICompatible(openai_compatible_client))
            }
            InferenceProvider::Anthropic => Ok(Self::Anthropic(
                AnthropicClient::new(app_state, base_client, api_key)?,
            )),
            InferenceProvider::Bedrock => Ok(Self::Bedrock(
                BedrockClient::new(app_state, base_client, api_key)?,
            )),
            InferenceProvider::Ollama => {
                Ok(Self::Ollama(OllamaClient::new(app_state, base_client)?))
            }
        }
    }
}

fn compat_provider_api_key_from_env(
    provider: &InferenceProvider,
) -> Option<Secret<String>> {
    let raw = match provider {
        InferenceProvider::OpenAI
        | InferenceProvider::Custom
        | InferenceProvider::Named(_) => std::env::var("OPENAI_API_KEY").ok(),
        InferenceProvider::GoogleGemini => std::env::var("GEMINI_API_KEY")
            .ok()
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok()),
        InferenceProvider::Anthropic => std::env::var("ANTHROPIC_API_KEY").ok(),
        InferenceProvider::Bedrock | InferenceProvider::Ollama => None,
    }?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(Secret::from(trimmed.to_string()))
    }
}

fn bedrock_provider_key_from_env() -> Option<ProviderKey> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID").ok()?;
    let secret_key = std::env::var("AWS_SECRET_ACCESS_KEY").ok()?;
    let access_key = access_key.trim();
    let secret_key = secret_key.trim();

    if access_key.is_empty() || secret_key.is_empty() {
        return None;
    }

    Some(ProviderKey::AwsCredentials {
        access_key: Secret::from(access_key.to_string()),
        secret_key: Secret::from(secret_key.to_string()),
    })
}

async fn resolve_bedrock_provider_key(
    app_state: &AppState,
    auth_ctx: Option<&AuthContext>,
) -> Result<ProviderKey, ApiError> {
    if app_state.config().compat_mode && auth_ctx.is_none() {
        return bedrock_provider_key_from_env()
            .ok_or(ApiError::Authentication(AuthError::ProviderKeyNotFound));
    }

    if let Some(auth_ctx) = auth_ctx
        && let Some(key) = app_state
            .0
            .provider_keys
            .get_provider_key(
                &InferenceProvider::Bedrock,
                Some(&auth_ctx.org_id),
            )
            .await
    {
        return Ok(key);
    }

    bedrock_provider_key_from_env()
        .ok_or(ApiError::Authentication(AuthError::ProviderKeyNotFound))
}

fn master_key_provider_matches(
    master_provider: &InferenceProvider,
    target_provider: &InferenceProvider,
) -> bool {
    matches!(master_provider, InferenceProvider::Custom)
        || master_provider == target_provider
}

fn fallback_enabled_for_master_key_resolution(
    resolution: MasterKeyResolution,
) -> bool {
    resolution.fallback_enabled()
}

impl AsRef<reqwest::Client> for Client {
    fn as_ref(&self) -> &reqwest::Client {
        match self {
            Client::OpenAICompatible(client) => &client.inner,
            Client::Anthropic(client) => &client.0,
            Client::Ollama(client) => &client.0,
            Client::Bedrock(client) => &client.inner,
        }
    }
}

/// Request which responds with SSE.
/// [server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events#event_stream_format)
pub(super) async fn sse_stream(
    mut event_source: EventSource,
    api_endpoint: Option<ApiEndpoint>,
    metrics_registry: EndpointMetricsRegistry,
) -> Result<SSEStream, StreamError> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // we want to await the first event so that we can propagate errors
    match event_source.next().await {
        Some(Ok(event)) => match event {
            Event::Message(message) if message.data != "[DONE]" => {
                let data = Bytes::from(message.data);

                if let Err(_e) = tx.send(Ok(data)) {
                    tracing::trace!("rx dropped before stream ended");
                }
            }
            _ => {}
        },
        Some(Err(e)) => {
            handle_stream_error(e, api_endpoint.clone(), &metrics_registry)
                .await?;
        }
        None => {}
    }

    tokio::spawn(
        async move {
            while let Some(ev) = event_source.next().await {
                match ev {
                    Err(e) => {
                        if matches!(e, reqwest_eventsource::Error::StreamEnded) {
                            // `StreamEnded` is returned for valid stream end cases
                            // so we don't send the error in the channel
                            tracing::trace!("stream ended");
                            break;
                        }

                        if let Err(e) = handle_stream_error_with_tx(e, tx.clone(), api_endpoint.clone(), &metrics_registry).await {
                            tracing::error!(error = %e, "failed to handle stream error");
                            break;
                        }
                    }
                    Ok(event) => match event {
                        Event::Message(message) => {
                            if message.data == "[DONE]" {
                                break;
                            }

                            let data = Bytes::from(message.data);

                            if let Err(_e) = tx.send(Ok(data)) {
                                tracing::trace!(
                                    "rx dropped before stream ended"
                                );
                                break;
                            }
                        }
                        Event::Open => {}
                    },
                }
            }

            event_source.close();
        }
        .instrument(info_span!("sse_stream")),
    );

    Ok(Box::pin(
        tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
    ))
}

async fn handle_stream_error_with_tx(
    error: reqwest_eventsource::Error,
    tx: tokio::sync::mpsc::UnboundedSender<Result<Bytes, ApiError>>,
    api_endpoint: Option<ApiEndpoint>,
    metrics_registry: &EndpointMetricsRegistry,
) -> Result<(), InternalError> {
    record_stream_err_metrics(&error, api_endpoint.clone(), metrics_registry);
    match error {
        reqwest_eventsource::Error::InvalidStatusCode(
            status_code,
            response,
        ) => {
            let http_resp = http::Response::from(response);
            let (_parts, body) = http_resp.into_parts();
            let body = body.collect().await?.to_bytes();

            cfg_if::cfg_if! {
                // this is compiled out in release builds
                if #[cfg(debug_assertions)] {
                    tracing::debug!(
                        status_code = %status_code,
                        body_len = body.len(),
                        "received error response in stream"
                    );
                } else {
                    if status_code.is_server_error() {
                        tracing::error!(status_code = %status_code, "received server error in stream");
                    } else if status_code.is_client_error() {
                        tracing::debug!(status_code = %status_code, "received client error in stream");
                    }
                }
            }

            if let Err(e) = tx.send(Ok(body)) {
                tracing::error!(error = %e, "rx dropped before stream ended");
            }
            Ok(())
        }
        e => {
            if let Err(e) = tx.send(Err(ApiError::StreamError(
                StreamError::StreamError(Box::new(e)),
            ))) {
                tracing::error!(error = %e, "rx dropped before stream ended");
            }
            Ok(())
        }
    }
}

async fn handle_stream_error(
    error: reqwest_eventsource::Error,
    api_endpoint: Option<ApiEndpoint>,
    metrics_registry: &EndpointMetricsRegistry,
) -> Result<(), StreamError> {
    record_stream_err_metrics(&error, api_endpoint.clone(), metrics_registry);
    match error {
        reqwest_eventsource::Error::InvalidStatusCode(
            status_code,
            response,
        ) => {
            let http_resp = http::Response::from(response);
            let (_, body) = http_resp.into_parts();
            let body = match body.collect().await {
                Err(e) => {
                    let error =
                        axum_core::Error::new(InternalError::ReqwestError(e));
                    return Err(StreamError::BodyError(error));
                }
                Ok(body) => body.to_bytes(),
            };
            let text = String::from_utf8_lossy(&body).into_owned();
            tracing::warn!(
                status_code = %status_code,
                body_len = text.len(),
                "received error response in stream"
            );
            Err(StreamError::UpstreamError {
                status_code,
                body: text,
            })
        }
        e => {
            tracing::error!(error = %e, "received error in stream");
            Err(StreamError::StreamError(Box::new(e)))
        }
    }
}

fn record_stream_err_metrics(
    stream_error: &reqwest_eventsource::Error,
    api_endpoint: Option<ApiEndpoint>,
    metrics_registry: &EndpointMetricsRegistry,
) {
    if let Some(api_endpoint) = api_endpoint {
        metrics_registry.health_metrics(api_endpoint).map(|metrics| {
            metrics.incr_for_stream_error(stream_error);
        }).inspect_err(|e| {
            tracing::error!(error = %e, "failed to increment stream error metrics");
        }).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_key_provider_matches_returns_true_for_same_provider() {
        assert!(master_key_provider_matches(
            &InferenceProvider::OpenAI,
            &InferenceProvider::OpenAI
        ));
    }

    #[test]
    fn master_key_provider_matches_returns_false_for_different_provider() {
        assert!(!master_key_provider_matches(
            &InferenceProvider::OpenAI,
            &InferenceProvider::Anthropic
        ));
    }

    #[test]
    fn master_key_provider_matches_custom_matches_any_target() {
        assert!(master_key_provider_matches(
            &InferenceProvider::Custom,
            &InferenceProvider::OpenAI
        ));
        assert!(master_key_provider_matches(
            &InferenceProvider::Custom,
            &InferenceProvider::Anthropic
        ));
    }

    #[test]
    fn fallback_enabled_honors_master_key_resolution() {
        assert!(!fallback_enabled_for_master_key_resolution(
            MasterKeyResolution::PrimaryOnly
        ));
        assert!(fallback_enabled_for_master_key_resolution(
            MasterKeyResolution::PrimaryThenWorkspaceFallback
        ));
    }
}
