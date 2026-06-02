use bytes::Bytes;
use chrono::{DateTime, Utc};
use http::HeaderMap;
use http_body_util::BodyExt;
use opentelemetry::KeyValue;
use tokio::{sync::oneshot, time::Instant};
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    logger::service::LoggerService,
    metrics::tfft::TFFTFuture,
    session_headers::SessionHeaders,
    types::{
        body::BodyReader,
        extensions::{
            LargeContextDecision, MapperContext, PromptCompressionTokenPair, PromptContext,
            PromptHeaderForRequestLog, RequestContext, RequestLogEmitted,
        },
        provider::InferenceProvider,
        router::RouterId,
    },
    utils::debug_log::{self, DebugLogConfig},
};

#[derive(Clone)]
pub(crate) struct DispatchLogger {
    app_state: AppState,
}

pub(crate) struct DispatchLogRequest<'a> {
    pub(crate) req_ctx: &'a RequestContext,
    pub(crate) start_time: DateTime<Utc>,
    pub(crate) start_instant: Instant,
    pub(crate) target_url: url::Url,
    pub(crate) headers: HeaderMap,
    pub(crate) req_body_bytes: Bytes,
    pub(crate) client_response: &'a http::Response<crate::types::body::Body>,
    pub(crate) response_body_for_logger: BodyReader,
    pub(crate) tfft_rx: oneshot::Receiver<()>,
    pub(crate) mapper_ctx: &'a MapperContext,
    pub(crate) router_id: Option<RouterId>,
    pub(crate) request_log_id: Uuid,
    pub(crate) response_log_id: Uuid,
    pub(crate) response_received_at: DateTime<Utc>,
    pub(crate) prompt_ctx: Option<PromptContext>,
    pub(crate) prompt_header_for_request_log: Option<PromptHeaderForRequestLog>,
    pub(crate) large_context_decision: Option<LargeContextDecision>,
    pub(crate) prompt_compression_tokens: Option<PromptCompressionTokenPair>,
    pub(crate) session_ctx: Option<SessionHeaders>,
    pub(crate) ai_gateway_body_mapping: Option<String>,
    pub(crate) cache_reference_id: Option<String>,
    pub(crate) llm_kv_cache_read_enabled: bool,
    pub(crate) effective_provider: &'a InferenceProvider,
    pub(crate) log_emitted: Option<&'a RequestLogEmitted>,
    pub(crate) debug_log_config: DebugLogConfig,
}

fn emit_dispatcher_debug_request(
    target_url: &url::Url,
    headers: &HeaderMap,
    response_headers: &HeaderMap,
    req_body_bytes: &Bytes,
    debug_log_config: DebugLogConfig,
) {
    if debug_log_config.headers {
        let request_headers = debug_log::debug_header_lines(headers);
        tracing::info!(
            target_url = %target_url,
            headers = %request_headers,
            "dispatcher request headers (debug headers enabled)"
        );
        let response_headers = debug_log::debug_header_lines(response_headers);
        tracing::info!(
            target_url = %target_url,
            headers = %response_headers,
            "dispatcher response headers (debug headers enabled)"
        );
    }

    debug_log::maybe_log_body_with_target(
        "dispatcher request",
        target_url,
        req_body_bytes,
        debug_log_config,
    );
}

struct ResponseMetricsDebugTask {
    app_state: AppState,
    response_body_for_logger: BodyReader,
    start_instant: Instant,
    tfft_rx: oneshot::Receiver<()>,
    forward_url: String,
    provider: String,
    model: String,
    path: String,
    debug_log_config: DebugLogConfig,
}

fn spawn_response_body_metrics_and_debug_log(task: ResponseMetricsDebugTask) {
    tokio::spawn(
        async move {
            let tfft_future = TFFTFuture::new(task.start_instant, task.tfft_rx);
            let collect_future = task.response_body_for_logger.collect();
            let (collected, tfft_duration) = tokio::join!(collect_future, tfft_future);
            let response_body = collected.expect("infallible never errors").to_bytes();
            debug_log::maybe_log_body_with_target(
                "dispatcher response",
                &task.forward_url,
                &response_body,
                task.debug_log_config,
            );
            if let Ok(tfft_duration) = tfft_duration {
                tracing::trace!(
                    tfft_duration = ?tfft_duration,
                    "tfft_duration"
                );
                let attributes = [
                    KeyValue::new("provider", task.provider),
                    KeyValue::new("model", task.model),
                    KeyValue::new("path", task.path),
                ];
                #[allow(clippy::cast_precision_loss)]
                task.app_state
                    .0
                    .metrics
                    .tfft_duration
                    .record(tfft_duration.as_millis() as f64, &attributes);
            } else {
                tracing::error!("Failed to get TFFT signal");
            }
        }
        .instrument(tracing::Span::current()),
    );
}

impl DispatchLogger {
    #[must_use]
    pub(crate) fn new(app_state: AppState) -> Self {
        Self { app_state }
    }

    /// Handles logging logic for both observability and metrics
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle(&self, request: DispatchLogRequest<'_>) {
        emit_dispatcher_debug_request(
            &request.target_url,
            &request.headers,
            request.client_response.headers(),
            &request.req_body_bytes,
            request.debug_log_config,
        );

        let deployment_target = self.app_state.config().deployment_target.clone();
        if self.app_state.config().alephant.is_observability_enabled() {
            if let Some(auth_ctx) = request.req_ctx.auth_context.clone() {
                let cache_enabled_for_log = if request.llm_kv_cache_read_enabled {
                    Some(request.cache_reference_id.is_some())
                } else {
                    None
                };
                let response_logger = LoggerService::builder()
                    .app_state(self.app_state.clone())
                    .auth_ctx(auth_ctx)
                    .start_time(request.start_time)
                    .start_instant(request.start_instant)
                    .target_url(request.target_url)
                    .request_headers(request.headers)
                    .request_body(request.req_body_bytes)
                    .response_status(request.client_response.status())
                    .response_body(request.response_body_for_logger)
                    .provider(request.effective_provider.clone())
                    .tfft_rx(request.tfft_rx)
                    .mapper_ctx(request.mapper_ctx.clone())
                    .router_id(request.router_id)
                    .deployment_target(deployment_target)
                    .request_id(request.request_log_id)
                    .response_id(request.response_log_id)
                    .response_created_at(request.response_received_at)
                    .prompt_ctx(request.prompt_ctx)
                    .prompt_header_for_request_log(request.prompt_header_for_request_log)
                    .large_context_decision(request.large_context_decision)
                    .prompt_compression_tokens(request.prompt_compression_tokens)
                    .session_ctx(request.session_ctx)
                    .agent_ctx(request.req_ctx.agent_context.clone())
                    .ai_gateway_body_mapping(request.ai_gateway_body_mapping)
                    .cache_enabled(cache_enabled_for_log)
                    .cache_reference_id(request.cache_reference_id)
                    .debug_log_config(request.debug_log_config)
                    .build();

                if let Some(marker) = request.log_emitted {
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
        } else {
            if let Some(marker) = request.log_emitted {
                marker.mark();
            }
            let app_state = self.app_state.clone();
            let model = request
                .mapper_ctx
                .model
                .as_ref()
                .map_or_else(|| "unknown".to_string(), std::string::ToString::to_string);
            let forward_url = request.target_url.to_string();
            let path = request.target_url.path().to_string();
            let provider_string = request.effective_provider.to_string();
            spawn_response_body_metrics_and_debug_log(ResponseMetricsDebugTask {
                app_state,
                response_body_for_logger: request.response_body_for_logger,
                start_instant: request.start_instant,
                tfft_rx: request.tfft_rx,
                forward_url,
                provider: provider_string,
                model,
                path,
                debug_log_config: request.debug_log_config,
            });
        }
    }
}
