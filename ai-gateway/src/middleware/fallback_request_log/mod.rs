use bytes::Bytes;
use chrono::Utc;
use futures::future::BoxFuture;
use opentelemetry::KeyValue;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use tower::Layer;
use tracing::Instrument;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    error::api::{ApiError, ErrorDetails, ErrorResponse},
    logger::service::LoggerService,
    types::{
        body::{BodyReader, TfftTrigger},
        extensions::{AuthContext, MapperContext, RequestLogEmitted},
        provider::InferenceProvider,
        request::Request,
        response::Response,
        router::RouterId,
    },
};

// ── Layer ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct FallbackRequestLogLayer {
    app_state: AppState,
}

impl FallbackRequestLogLayer {
    #[must_use]
    pub fn new(app_state: &AppState) -> Self {
        Self {
            app_state: app_state.clone(),
        }
    }
}

impl<S> Layer<S> for FallbackRequestLogLayer {
    type Service = FallbackRequestLogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        FallbackRequestLogService {
            inner,
            app_state: self.app_state.clone(),
        }
    }
}

// ── Service ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct FallbackRequestLogService<S> {
    inner: S,
    app_state: AppState,
}

impl<S> tower::Service<Request> for FallbackRequestLogService<S>
where
    S: tower::Service<Request, Response = Response, Error = ApiError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<'static, Result<Response, ApiError>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request) -> Self::Future {
        let app_state = self.app_state.clone();

        let marker = RequestLogEmitted::new();
        req.extensions_mut().insert(marker.clone());

        let headers = req.headers().clone();
        let auth_ctx = req.extensions().get::<AuthContext>().cloned();
        let router_id = req.extensions().get::<RouterId>().cloned();
        let provider = req.extensions().get::<InferenceProvider>().cloned();
        let start_instant = Instant::now();
        let start_time = Utc::now();

        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            match inner.call(req).await {
                Ok(resp) => Ok(resp),
                Err(api_err) => {
                    if !marker.is_emitted() {
                        emit_fallback_log(
                            &app_state,
                            auth_ctx,
                            &headers,
                            router_id,
                            provider,
                            start_time,
                            start_instant,
                            &api_err,
                        );
                    }
                    Err(api_err)
                }
            }
        })
    }
}

// ── Fallback log emit ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn emit_fallback_log(
    app_state: &AppState,
    auth_ctx: Option<AuthContext>,
    headers: &http::HeaderMap,
    router_id: Option<RouterId>,
    provider: Option<InferenceProvider>,
    start_time: chrono::DateTime<Utc>,
    start_instant: Instant,
    api_err: &ApiError,
) {
    if !app_state.config().alephant.is_observability_enabled() {
        return;
    }
    let Some(auth_ctx) = auth_ctx else {
        return;
    };

    let deployment_target = app_state.config().deployment_target.clone();
    let provider = provider.unwrap_or_default();

    let target_url: url::Url = "https://gateway-internal/fallback-log"
        .parse()
        .expect("static URL is valid");

    let error_json = build_error_response_json(api_err);
    let response_status = error_status_code(api_err);

    let request_log_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
        .unwrap_or_else(Uuid::new_v4);
    let response_log_id = Uuid::new_v4();
    let response_received_at = Utc::now();

    let (tx, rx) = mpsc::unbounded_channel();
    let _ = tx.send(Bytes::from(error_json));
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

    let response_logger = LoggerService::builder()
        .app_state(app_state.clone())
        .auth_ctx(auth_ctx)
        .start_time(start_time)
        .start_instant(start_instant)
        .target_url(target_url)
        .request_headers(headers.clone())
        .request_body(Bytes::new())
        .response_status(response_status)
        .response_body(body_reader)
        .provider(provider)
        .tfft_rx(tfft_rx)
        .mapper_ctx(mapper_ctx)
        .router_id(router_id)
        .deployment_target(deployment_target)
        .request_id(request_log_id)
        .response_id(response_log_id)
        .response_created_at(response_received_at)
        .prompt_ctx(None)
        .prompt_header_for_request_log(None)
        .session_ctx(None)
        .build();

    let app_state = app_state.clone();
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

fn build_error_response_json(api_err: &ApiError) -> Vec<u8> {
    let message = format!("{api_err}");
    serde_json::to_vec(&ErrorResponse {
        error: ErrorDetails {
            message,
            r#type: Some("gateway_error".to_string()),
            param: None,
            code: Some("fallback_log".to_string()),
        },
    })
    .unwrap_or_default()
}

fn error_status_code(api_err: &ApiError) -> http::StatusCode {
    match api_err {
        ApiError::InvalidRequest(_) => http::StatusCode::BAD_REQUEST,
        ApiError::Authentication(_) => http::StatusCode::UNAUTHORIZED,
        ApiError::Internal(_)
        | ApiError::StreamError(_)
        | ApiError::Panic(_) => http::StatusCode::INTERNAL_SERVER_ERROR,
    }
}
