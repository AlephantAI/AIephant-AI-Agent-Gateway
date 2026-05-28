use bytes::Bytes;
use http::HeaderMap;
use tokio::sync::mpsc;

use super::sync_dispatch::SyncDispatchResponse;
use crate::{
    app_state::AppState,
    dispatcher::{
        client::Client,
        regional_endpoint::{self, EndpointRegion},
        request_builder::request_builder_with_effective_host,
        sync_dispatch,
        target_endpoint::TargetEndpoint,
        upstream_auth::{UpstreamAuthApplier, UpstreamAuthRequest},
    },
    error::api::ApiError,
    types::{extensions::RequestContext, provider::InferenceProvider},
};

pub(super) struct RegionalRetryExecutor<'a> {
    app_state: &'a AppState,
    client: &'a Client,
    provider: &'a InferenceProvider,
}

pub(super) struct RegionalRetryRequest<'a> {
    pub(super) req_body_bytes: Bytes,
    pub(super) req_ctx: &'a RequestContext,
    pub(super) cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    pub(super) method: &'a http::Method,
    pub(super) headers: &'a HeaderMap,
    pub(super) target_endpoint: &'a TargetEndpoint,
}

impl<'a> RegionalRetryExecutor<'a> {
    #[must_use]
    pub(super) fn new(
        app_state: &'a AppState,
        client: &'a Client,
        provider: &'a InferenceProvider,
    ) -> Self {
        Self {
            app_state,
            client,
            provider,
        }
    }

    pub(super) async fn retry_once(
        &self,
        request: RegionalRetryRequest<'_>,
    ) -> Result<Option<SyncDispatchResponse>, ApiError> {
        let Some(cn_retry_url) = request.target_endpoint.cn_retry_url.clone() else {
            return Ok(None);
        };
        let auth_ctx = request.req_ctx.auth_context.as_ref();
        let master_key_id = auth_ctx.and_then(|auth| auth.master_key_id);

        tracing::info!(
            provider = %self.provider,
            master_key_id = ?master_key_id,
            target_url = %cn_retry_url,
            "regional_endpoint_retry: attempt"
        );

        let request_builder = self
            .client
            .as_ref()
            .request(request.method.clone(), cn_retry_url.clone())
            .headers(request.headers.clone());
        let request_builder = request_builder_with_effective_host(request_builder, &cn_retry_url);
        let request_builder = UpstreamAuthApplier::new(self.app_state)
            .apply(UpstreamAuthRequest {
                client: self.client,
                request_builder,
                req_body_bytes: &request.req_body_bytes,
                auth_context: auth_ctx,
                provider: self.provider.clone(),
            })
            .await?;

        let response = sync_dispatch::dispatch_sync(
            &request_builder,
            request.req_body_bytes,
            request.cache_tap,
        )
        .await?;
        let status = response.0.status();

        if status.is_success() {
            regional_endpoint::remember_region(self.app_state, master_key_id, EndpointRegion::Cn)
                .await;
            tracing::info!(
                provider = %self.provider,
                master_key_id = ?master_key_id,
                target_url = %cn_retry_url,
                status = %status,
                "regional_endpoint_retry: success"
            );
        } else {
            tracing::warn!(
                provider = %self.provider,
                master_key_id = ?master_key_id,
                target_url = %cn_retry_url,
                status = %status,
                "regional_endpoint_retry: failure"
            );
        }

        Ok(Some(response))
    }
}
