use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use axum_core::body::Body;
use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use tonic::{Request as GrpcRequest, Response as GrpcResponse, metadata::MetadataMap};
use tower::Service;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    error::api::{ErrorDetails, ErrorResponse},
    middleware::counted_body::CountedBody,
    payment_proto::{
        EndpointPaymentSnapshot, GetPaymentRequirementsRequest, GetPaymentRequirementsResponse,
        Money, RecordServiceResultRequest, RecordServiceResultResponse,
        RequestContext as PaymentRequestContext, VerifyAndSettlePaymentRequest,
        VerifyAndSettlePaymentResponse,
    },
    policy_proto::X402InboundEvaluateResponse,
    router::router_details::RouteType,
    session_headers::ALEPHANT_SESSION_ID_HEADER,
    store::router::DbX402PaymentActivityLogFields,
    types::{request::Request, response::Response as GatewayResponse},
    x402::{
        body_schema::{BodySchemaValidationError, validate_body_against_schema},
        forward_signature::{
            inject_forward_signature_headers, resolve_endpoint_signing_secret,
            upstream_path_with_query,
        },
        log::{X402LogStage, X402PaymentLogMessage, ZERO_UUID, hash_sensitive, write_x402_log},
        policy::{build_policy_request, evaluate_x402_inbound},
        proxy::{
            PAYMENT_SIGNATURE_HEADER, UpstreamProxyResult, build_upstream_url,
            filtered_upstream_headers, hash_body, method_for_upstream, proxy_paid_request,
        },
        snapshot::resolve_snapshot,
        types::X402EndpointSnapshot,
    },
};

const PUBLIC_PAYMENT_SIGNATURE_HEADER: &str = "PAYMENT-SIGNATURE";
const PAYMENT_REQUIRED_HEADER: &str = "Payment-Required";
const PAYMENT_RESPONSE_HEADER: &str = "Payment-Response";
const INVALID_REQUEST_ERROR_TYPE: &str = "invalid_request_error";
const PAYMENT_REQUIRED_ERROR_TYPE: &str = "payment_required";
const X402_GATEWAY_ERROR_TYPE: &str = "x402_gateway_error";
const X402_POLICY_DENIED_TYPE: &str = "x402_policy_denied";
const PAYMENT_REQUIREMENTS_FAILED_MESSAGE: &str = "x402 payment requirements failed";
const RECORD_SERVICE_RESULT_FAILED_MESSAGE: &str = "x402 record service result failed";
const DEBUG_HEADERS_ENV: &str = "AI_GATEWAY_DEBUG_HEADERS";
const DEBUG_BODY_ENV: &str = "AI_GATEWAY_DEBUG_BODY";

#[derive(Clone, Debug)]
pub struct X402AgentService {
    app_state: AppState,
    http_client: reqwest::Client,
}

impl X402AgentService {
    #[must_use]
    pub fn new(app_state: AppState) -> Self {
        let http_client = app_state.0.alephant_http_client.request_client.clone();
        Self {
            app_state,
            http_client,
        }
    }
}

impl Service<Request> for X402AgentService {
    type Response = GatewayResponse;
    type Error = Infallible;
    type Future = futures::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let app_state = self.app_state.clone();
        let http_client = self.http_client.clone();
        Box::pin(async move { Ok(handle_x402_request(app_state, http_client, req).await) })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct X402LogContext {
    trace_id: String,
    session_id: String,
    endpoint_id: String,
    workspace_id: String,
    agent_id: String,
    payment_network: String,
    payment_asset: String,
    payment_pay_to: String,
    payment_facilitator: String,
}

impl X402LogContext {
    #[must_use]
    pub(crate) fn from_request(
        trace_id: String,
        _request_id: String,
        session_id: String,
        _slug: String,
        _method: String,
        _public_path: String,
    ) -> Self {
        Self {
            trace_id,
            session_id,
            ..Self::default()
        }
    }

    #[must_use]
    pub(crate) fn with_resolved_snapshot(
        mut self,
        snapshot: &X402EndpointSnapshot,
        snapshot_source: &str,
    ) -> Self {
        let _ = snapshot_source;
        self.endpoint_id = snapshot.endpoint_id.to_string();
        self.workspace_id = snapshot.workspace_id.to_string();
        self.agent_id = snapshot
            .agent_id
            .map_or_else(String::new, |id| id.to_string());
        self.payment_network = snapshot.network.clone();
        self.payment_asset = snapshot.asset.clone();
        self.payment_pay_to = snapshot.receive_wallet_address.clone();
        self.payment_facilitator = snapshot.policy.facilitator.clone().unwrap_or_default();
        self
    }

    #[must_use]
    pub(crate) fn with_payment_context(self, context: &PaymentRequestContext) -> Self {
        let _ = context;
        self
    }

    #[must_use]
    pub(crate) fn with_upstream_url(self, upstream_url: String) -> Self {
        let _ = upstream_url;
        self
    }
}

pub(crate) fn build_x402_log_message(
    stage: X402LogStage,
    context: &X402LogContext,
) -> X402PaymentLogMessage {
    let mut message = X402PaymentLogMessage::new(stage);
    message.trace_id = context.trace_id.clone();
    message.agent_session_id = context.session_id.clone();
    message.endpoint_id = uuid_or_zero(&context.endpoint_id);
    message.workspace_id = uuid_or_zero(&context.workspace_id);
    message.agent_id = uuid_or_zero(&context.agent_id);
    message.network = context.payment_network.clone();
    message.asset = context.payment_asset.clone();
    message.seller_receive_wallet_address = context.payment_pay_to.clone();
    message.facilitator = context.payment_facilitator.clone();
    message
}

fn apply_policy_log_fields(
    message: &mut X402PaymentLogMessage,
    response: &X402InboundEvaluateResponse,
) {
    if let Some(detail) = &response.detail {
        message.fund_status = detail.fund_policy.clone();
    }
    if !response.allowed && !response.reason.is_empty() {
        message.failure_reason = response.reason.clone();
    }
}

fn apply_payment_requirements_log_fields(
    message: &mut X402PaymentLogMessage,
    response: &GetPaymentRequirementsResponse,
) {
    message.activity_id = uuid_or_zero(&response.activity_id);
    message.gross_revenue = 0.0;
    message.asset.clear();
    message.network.clear();
    message.seller_receive_wallet_address.clear();
    message.facilitator.clear();
    if let Some(accept) = response.accepts.first() {
        message.gross_revenue = parse_money_amount(&accept.amount);
        if !accept.asset.is_empty() {
            message.asset = accept.asset.clone();
        }
        if !accept.network.is_empty() {
            message.network = accept.network.clone();
        }
        if !accept.pay_to.is_empty() {
            message.seller_receive_wallet_address = accept.pay_to.clone();
        }
        if !accept.facilitator.is_empty() {
            message.facilitator = accept.facilitator.clone();
        }
    }
    if !response.success {
        apply_error_log_fields(
            message,
            "x402_payment_requirements_failed",
            payment_failure_message(
                &response.failure_reason,
                PAYMENT_REQUIREMENTS_FAILED_MESSAGE,
            ),
        );
    }
}

pub(crate) fn apply_verify_and_settle_log_fields(
    message: &mut X402PaymentLogMessage,
    response: &VerifyAndSettlePaymentResponse,
) {
    if !response.activity_id.is_empty() {
        message.activity_id = uuid_or_zero(&response.activity_id);
    }
    message.payment_status = response.payment_status.clone();
    message.settlement_status = response.settlement_status.clone();
    message.tx_hash = response.tx_hash.clone();
    message.failure_reason = response.failure_reason.clone();
    message.buyer_wallet = response.buyer_wallet.clone();
    if !response.facilitator.is_empty() {
        message.facilitator = response.facilitator.clone();
    }
    let (amount, asset, network) = money_fields(response.gross.as_ref());
    if !amount.is_empty() {
        message.gross_revenue = parse_money_amount(amount);
    }
    if !asset.is_empty() {
        message.asset = asset.to_string();
    }
    if !network.is_empty() {
        message.network = network.to_string();
    }
    let (amount, _, _) = money_fields(response.alephant_fee.as_ref());
    message.alephant_fee = parse_money_amount(amount);
    let (amount, _, _) = money_fields(response.net.as_ref());
    message.net_revenue = parse_money_amount(amount);
}

fn apply_upstream_log_fields(
    message: &mut X402PaymentLogMessage,
    _upstream: &UpstreamProxyResult,
    service_status: &str,
    failure_reason: &str,
) {
    message.service_status = service_status.to_string();
    message.failure_reason = failure_reason.to_string();
}

fn apply_error_log_fields(
    message: &mut X402PaymentLogMessage,
    kind: impl Into<String>,
    error_message: impl Into<String>,
) {
    let kind = kind.into();
    let error_message = error_message.into();
    message.ledger_status = kind;
    message.failure_reason = error_message;
}

fn uuid_or_zero(value: &str) -> String {
    Uuid::parse_str(value)
        .map(|id| id.to_string())
        .unwrap_or_else(|_| ZERO_UUID.to_string())
}

fn parse_money_amount(value: &str) -> f64 {
    value.parse::<f64>().unwrap_or(0.0)
}

async fn emit_x402_log_best_effort(app_state: &AppState, mut message: X402PaymentLogMessage) {
    enrich_x402_log_from_activity(app_state, &mut message).await;
    if let Err(error) = write_x402_log(app_state, &message).await {
        tracing::warn!(error = %error, "x402 payment log write failed");
    }
}

async fn enrich_x402_log_from_activity(app_state: &AppState, message: &mut X402PaymentLogMessage) {
    let Ok(activity_id) = Uuid::parse_str(&message.activity_id) else {
        return;
    };
    if activity_id == Uuid::nil() {
        return;
    }
    let Some(store) = app_state.router_store() else {
        return;
    };

    match store
        .fetch_x402_payment_activity_log_fields(activity_id)
        .await
    {
        Ok(Some(fields)) => apply_activity_log_fields(message, fields),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                activity_id = %activity_id,
                error = %error,
                "x402 payment activity log enrichment failed"
            );
        }
    }
}

fn apply_activity_log_fields(
    message: &mut X402PaymentLogMessage,
    fields: DbX402PaymentActivityLogFields,
) {
    if let Some(value) = fields.ale_receive_wallet_address {
        message.platform_receive_wallet_address = value;
    }
    if let Some(value) = fields.fee_wallet_address {
        message.fee_wallet_address = value;
    }
    if let Some(value) = fields.ai_cost {
        message.ai_cost = value;
    }
    if let Some(value) = fields.trace_status {
        message.trace_status = value;
    }
    message.settled_at = fields.settled_at;
    message.available_at = fields.available_at;
    message.verify_time = fields.verified_at;
}

fn with_on_response_body_completion(
    response: GatewayResponse,
    on_completion: Arc<dyn Fn() + Send + Sync>,
) -> GatewayResponse {
    let (parts, body) = response.into_parts();
    let body = Body::new(CountedBody::new(body, on_completion));
    Response::from_parts(parts, body)
}

fn emit_x402_log_after_response_body_completion(
    app_state: AppState,
    response: GatewayResponse,
    message: X402PaymentLogMessage,
) -> GatewayResponse {
    with_on_response_body_completion(
        response,
        Arc::new(move || {
            let app_state = app_state.clone();
            let message = message.clone();
            tokio::spawn(async move {
                emit_x402_log_best_effort(&app_state, message).await;
            });
        }),
    )
}

fn payment_grpc_log_label(direction: &str, method: &str) -> String {
    format!("x402 payment gRPC {direction}: {method}")
}

fn debug_env_flag_value_enabled(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
}

fn debug_env_flag_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| debug_env_flag_value_enabled(&value))
}

fn payment_grpc_request_with_auth<T>(
    body: T,
    payment_service_key: &str,
) -> Result<GrpcRequest<T>, &'static str> {
    let key = payment_service_key.trim();
    if key.is_empty() {
        return Err("PAYMENT_SERVICE_KEY is empty");
    }

    let mut request = GrpcRequest::new(body);
    let auth_value = format!("Bearer {key}");
    let auth_value = auth_value
        .parse()
        .map_err(|_| "PAYMENT_SERVICE_KEY is not valid metadata")?;
    request.metadata_mut().insert("authorization", auth_value);
    Ok(request)
}

fn redacted_payment_grpc_metadata(metadata: &MetadataMap) -> BTreeMap<String, String> {
    metadata
        .iter()
        .map(|entry| {
            let key = match entry {
                tonic::metadata::KeyAndValueRef::Ascii(key, _) => key.as_str().to_string(),
                tonic::metadata::KeyAndValueRef::Binary(key, _) => key.as_str().to_string(),
            };
            let value = if key.eq_ignore_ascii_case("authorization") {
                "Bearer *****".to_string()
            } else {
                match entry {
                    tonic::metadata::KeyAndValueRef::Ascii(_, value) => value
                        .to_str()
                        .map_or_else(|_| "<non-utf8>".to_string(), ToString::to_string),
                    tonic::metadata::KeyAndValueRef::Binary(_, _) => "<non-utf8>".to_string(),
                }
            };
            (key, value)
        })
        .collect()
}

fn log_payment_grpc_request<T: std::fmt::Debug>(method: &str, request: &GrpcRequest<T>) {
    let label = payment_grpc_log_label("request", method);
    if debug_env_flag_enabled(DEBUG_HEADERS_ENV) {
        tracing::info!(
            method,
            metadata = ?redacted_payment_grpc_metadata(request.metadata()),
            "{label} headers ({DEBUG_HEADERS_ENV})",
        );
    }
    if debug_env_flag_enabled(DEBUG_BODY_ENV) {
        tracing::info!(
            method,
            body = ?request.get_ref(),
            "{label} body ({DEBUG_BODY_ENV})",
        );
    }
}

fn log_payment_grpc_response<T: std::fmt::Debug>(method: &str, response: &GrpcResponse<T>) {
    let label = payment_grpc_log_label("response", method);
    if debug_env_flag_enabled(DEBUG_HEADERS_ENV) {
        tracing::info!(
            method,
            metadata = ?response.metadata(),
            "{label} headers ({DEBUG_HEADERS_ENV})",
        );
    }
    if debug_env_flag_enabled(DEBUG_BODY_ENV) {
        tracing::info!(
            method,
            body = ?response.get_ref(),
            "{label} body ({DEBUG_BODY_ENV})",
        );
    }
}

fn log_paid_upstream_request(
    method: &Method,
    url: &reqwest::Url,
    headers: &HeaderMap,
    body: &Bytes,
) {
    if debug_env_flag_enabled(DEBUG_HEADERS_ENV) {
        tracing::info!(
            method = %method,
            upstream_url = %url,
            headers = ?headers,
            "x402 paid upstream request headers ({DEBUG_HEADERS_ENV})",
        );
    }
    if debug_env_flag_enabled(DEBUG_BODY_ENV) {
        tracing::info!(
            method = %method,
            upstream_url = %url,
            body_size = body.len(),
            body_hash = %hash_body(body),
            body = %x402_upstream_body_for_debug_log(body),
            "x402 paid upstream request body ({DEBUG_BODY_ENV})",
        );
    }
}

fn x402_upstream_body_for_debug_log(body: &Bytes) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(body)
}

#[allow(clippy::too_many_lines)]
async fn handle_x402_request(
    app_state: AppState,
    http_client: reqwest::Client,
    req: Request,
) -> GatewayResponse {
    if !app_state.config().x402.enabled {
        return json_error_response(
            StatusCode::NOT_FOUND,
            "Resource not found",
            INVALID_REQUEST_ERROR_TYPE,
            "not_found",
        );
    }

    let Some(RouteType::X402Agent {
        slug,
        remaining_path,
    }) = req.extensions().get::<RouteType>().cloned()
    else {
        return json_error_response(
            StatusCode::NOT_FOUND,
            "Resource not found",
            INVALID_REQUEST_ERROR_TYPE,
            "not_found",
        );
    };

    let method = req.method().clone();
    let query = req.uri().query().map(str::to_string);
    let public_path = req.uri().path().to_string();
    let headers = req.headers().clone();
    let trace_id = trace_id_from_headers(&headers);
    let request_id = request_id_from_headers(&headers, &trace_id);
    let session_id = header_string(&headers, ALEPHANT_SESSION_ID_HEADER).unwrap_or_default();
    let payment_signature = header_string(&headers, PUBLIC_PAYMENT_SIGNATURE_HEADER)
        .or_else(|| header_string(&headers, PAYMENT_SIGNATURE_HEADER));
    let body = req.into_body();
    let log_context = X402LogContext::from_request(
        trace_id.clone(),
        request_id.clone(),
        session_id.clone(),
        slug.to_string(),
        method.as_str().to_string(),
        public_path,
    );

    let resolved = match resolve_snapshot(&app_state, &slug, method.as_str()).await {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            let mut message = build_x402_log_message(X402LogStage::SnapshotMiss, &log_context);
            apply_error_log_fields(&mut message, "not_found", "x402 snapshot not found");
            emit_x402_log_best_effort(&app_state, message).await;
            return json_error_response(
                StatusCode::NOT_FOUND,
                "Resource not found",
                INVALID_REQUEST_ERROR_TYPE,
                "not_found",
            );
        }
        Err(error) => {
            tracing::error!(error = %error, "x402 snapshot resolution failed");
            let mut message = build_x402_log_message(X402LogStage::SnapshotError, &log_context);
            apply_error_log_fields(&mut message, "x402_snapshot_error", error.to_string());
            emit_x402_log_best_effort(&app_state, message).await;
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "x402 snapshot resolution failed",
                X402_GATEWAY_ERROR_TYPE,
                "x402_snapshot_error",
            );
        }
    };
    let mut log_context =
        log_context.with_resolved_snapshot(&resolved.snapshot, resolved.source.as_str());

    let max_request_size = request_body_limit(&resolved.snapshot);
    if content_length_exceeds_policy(&headers, max_request_size) {
        let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
        apply_error_log_fields(
            &mut message,
            "x402_payload_too_large",
            "request body exceeds x402 policy limit",
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return payload_too_large_response();
    }

    let body = match collect_limited_request_body(body, max_request_size).await {
        Ok(body) => body,
        Err(RequestBodyReadError::TooLarge) => {
            let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
            apply_error_log_fields(
                &mut message,
                "x402_payload_too_large",
                "request body exceeds x402 policy limit",
            );
            emit_x402_log_best_effort(&app_state, message).await;
            return payload_too_large_response();
        }
        Err(RequestBodyReadError::ReadFailed) => {
            let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
            apply_error_log_fields(
                &mut message,
                "x402_body_read_failed",
                "failed to read request body",
            );
            emit_x402_log_best_effort(&app_state, message).await;
            return json_error_response(
                StatusCode::BAD_REQUEST,
                "failed to read request body",
                INVALID_REQUEST_ERROR_TYPE,
                "x402_body_read_failed",
            );
        }
    };

    if let Err(error) = validate_body_against_schema(&body, &resolved.snapshot.body_schema) {
        let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
        apply_error_log_fields(
            &mut message,
            body_schema_validation_error_code(&error),
            body_schema_validation_error_message(&error),
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return body_schema_validation_error_response(&error);
    }

    let policy_headers = safe_policy_headers(&headers);
    let policy_request = build_policy_request(
        &resolved.snapshot,
        &policy_headers,
        &body,
        app_state.config().x402.request_body_policy_max_bytes,
        &trace_id,
        &request_id,
        &session_id,
    );

    let policy_response = match evaluate_x402_inbound(&app_state, policy_request).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error, "x402 policy evaluation failed");
            let mut message = build_x402_log_message(X402LogStage::PolicyDenied, &log_context);
            apply_error_log_fields(&mut message, "x402_policy_unavailable", error.to_string());
            emit_x402_log_best_effort(&app_state, message).await;
            return json_error_response(
                StatusCode::FORBIDDEN,
                "x402 policy denied",
                X402_POLICY_DENIED_TYPE,
                "x402_policy_unavailable",
            );
        }
    };

    if !policy_response.allowed {
        let mut message = build_x402_log_message(X402LogStage::PolicyDenied, &log_context);
        apply_policy_log_fields(&mut message, &policy_response);
        apply_error_log_fields(
            &mut message,
            "x402_policy_denied",
            policy_denied_message(&policy_response.reason),
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return json_error_response(
            StatusCode::FORBIDDEN,
            policy_denied_message(&policy_response.reason),
            X402_POLICY_DENIED_TYPE,
            "x402_policy_denied",
        );
    }

    let payment_snapshot = endpoint_payment_snapshot(&resolved.snapshot);
    let payment_context = payment_context(&headers, &body, trace_id.clone(), request_id.clone());
    log_context = log_context.with_payment_context(&payment_context);

    match payment_signature {
        None => {
            handle_payment_requirements(app_state, payment_snapshot, payment_context, log_context)
                .await
        }
        Some(signature) => {
            handle_paid_request(
                app_state,
                http_client,
                resolved.snapshot,
                slug.to_string(),
                remaining_path.to_string(),
                method,
                query,
                headers,
                body,
                trace_id,
                request_id,
                payment_snapshot,
                payment_context,
                signature,
                log_context,
            )
            .await
        }
    }
}

async fn handle_payment_requirements(
    app_state: AppState,
    snapshot: EndpointPaymentSnapshot,
    context: PaymentRequestContext,
    log_context: X402LogContext,
) -> GatewayResponse {
    let Some(client) = app_state.x402_payment_client().await else {
        let mut message = build_x402_log_message(X402LogStage::PaymentRequired, &log_context);
        apply_error_log_fields(
            &mut message,
            "x402_payment_unavailable",
            "x402 payment service unavailable",
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return json_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "x402 payment service unavailable",
            X402_GATEWAY_ERROR_TYPE,
            "x402_payment_unavailable",
        );
    };

    let request = GetPaymentRequirementsRequest {
        snapshot: Some(snapshot),
        context: Some(context),
    };
    let request = match payment_grpc_request_with_auth(
        request,
        app_state.config().x402.payment_service_key.expose(),
    ) {
        Ok(request) => request,
        Err(error) => {
            tracing::error!(error, "x402 payment gRPC auth metadata build failed");
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "x402 payment service key is not configured",
                X402_GATEWAY_ERROR_TYPE,
                "x402_payment_service_key_unconfigured",
            );
        }
    };
    log_payment_grpc_request("GetPaymentRequirements", &request);
    let mut inner = client.inner();
    let timeout = app_state.config().x402.payment_timeout;

    match tokio::time::timeout(timeout, inner.get_payment_requirements(request)).await {
        Ok(Ok(response)) => {
            log_payment_grpc_response("GetPaymentRequirements", &response);
            let response = response.into_inner();
            let mut message = build_x402_log_message(X402LogStage::PaymentRequired, &log_context);
            apply_payment_requirements_log_fields(&mut message, &response);
            emit_x402_log_best_effort(&app_state, message).await;
            payment_required_response(&response)
        }
        Ok(Err(status)) => {
            tracing::warn!(status = %status, "x402 GetPaymentRequirements failed");
            let mut message = build_x402_log_message(X402LogStage::PaymentRequired, &log_context);
            apply_error_log_fields(
                &mut message,
                "x402_payment_requirements_failed",
                status.to_string(),
            );
            emit_x402_log_best_effort(&app_state, message).await;
            json_error_response(
                StatusCode::BAD_GATEWAY,
                PAYMENT_REQUIREMENTS_FAILED_MESSAGE,
                X402_GATEWAY_ERROR_TYPE,
                "x402_payment_requirements_failed",
            )
        }
        Err(_elapsed) => {
            tracing::warn!("x402 GetPaymentRequirements timed out");
            let mut message = build_x402_log_message(X402LogStage::PaymentRequired, &log_context);
            apply_error_log_fields(
                &mut message,
                "x402_payment_timeout",
                "x402 payment requirements timed out",
            );
            emit_x402_log_best_effort(&app_state, message).await;
            json_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "x402 payment requirements timed out",
                X402_GATEWAY_ERROR_TYPE,
                "x402_payment_timeout",
            )
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn handle_paid_request(
    app_state: AppState,
    http_client: reqwest::Client,
    snapshot: X402EndpointSnapshot,
    slug: String,
    remaining_path: String,
    inbound_method: Method,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
    _trace_id: String,
    _request_id: String,
    payment_snapshot: EndpointPaymentSnapshot,
    payment_context: PaymentRequestContext,
    payment_signature: String,
    log_context: X402LogContext,
) -> GatewayResponse {
    let mut log_context = log_context;
    let (upstream_method, upstream_url) = match prepare_paid_upstream_request(
        &snapshot,
        &slug,
        &remaining_path,
        &inbound_method,
        query.as_deref(),
    ) {
        Ok(upstream_request) => upstream_request,
        Err(error) => {
            tracing::error!(error = %error, "x402 upstream request build failed");
            let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
            apply_error_log_fields(&mut message, error.error_code(), error.to_string());
            emit_x402_log_best_effort(&app_state, message).await;
            return json_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                error.client_message(),
                X402_GATEWAY_ERROR_TYPE,
                error.error_code(),
            );
        }
    };
    log_context = log_context.with_upstream_url(upstream_url.as_str().to_string());

    let Some(payment_client) = app_state.x402_payment_client().await else {
        let mut message = build_x402_log_message(X402LogStage::PaymentRequired, &log_context);
        apply_error_log_fields(
            &mut message,
            "x402_payment_unavailable",
            "x402 payment service unavailable",
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return payment_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "x402 payment service unavailable",
            "x402_payment_unavailable",
        );
    };

    let mut inner = payment_client.inner();
    let payment_timeout = app_state.config().x402.payment_timeout;

    let verify_and_settle_request = VerifyAndSettlePaymentRequest {
        activity_id: String::new(),
        snapshot: Some(payment_snapshot.clone()),
        context: Some(payment_context.clone()),
        payment_signature: payment_signature.clone(),
    };
    let verify_and_settle_request = match payment_grpc_request_with_auth(
        verify_and_settle_request,
        app_state.config().x402.payment_service_key.expose(),
    ) {
        Ok(request) => request,
        Err(error) => {
            tracing::error!(error, "x402 payment gRPC auth metadata build failed");
            return payment_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "x402 payment service key is not configured",
                "x402_payment_service_key_unconfigured",
            );
        }
    };
    log_payment_grpc_request("VerifyAndSettlePayment", &verify_and_settle_request);
    let verify_and_settle_response = match tokio::time::timeout(
        payment_timeout,
        inner.verify_and_settle_payment(verify_and_settle_request),
    )
    .await
    {
        Ok(Ok(response)) => {
            log_payment_grpc_response("VerifyAndSettlePayment", &response);
            response.into_inner()
        }
        Ok(Err(status)) => {
            tracing::warn!(
                status = %status,
                "x402 VerifyAndSettlePayment failed"
            );
            let mut message = build_x402_log_message(X402LogStage::VerifyFailed, &log_context);
            apply_error_log_fields(&mut message, "x402_verify_failed", status.to_string());
            emit_x402_log_best_effort(&app_state, message).await;
            return payment_error_response(
                StatusCode::BAD_GATEWAY,
                "x402 payment verification failed",
                "x402_verify_failed",
            );
        }
        Err(_elapsed) => {
            tracing::warn!("x402 VerifyAndSettlePayment timed out");
            let mut message = build_x402_log_message(X402LogStage::VerifyFailed, &log_context);
            apply_error_log_fields(
                &mut message,
                "x402_verify_timeout",
                "x402 payment verification timed out",
            );
            emit_x402_log_best_effort(&app_state, message).await;
            return payment_error_response(
                StatusCode::GATEWAY_TIMEOUT,
                "x402 payment verification timed out",
                "x402_verify_timeout",
            );
        }
    };

    if !verify_and_settle_payment_succeeded(&verify_and_settle_response) {
        let stage = verify_and_settle_failure_stage(&verify_and_settle_response);
        let (code, fallback) = if matches!(stage, X402LogStage::SettleFailed) {
            ("x402_settle_failed", "x402 payment settlement failed")
        } else {
            ("x402_verify_failed", "x402 payment verification failed")
        };
        let mut message = build_x402_log_message(stage, &log_context);
        apply_verify_and_settle_log_fields(&mut message, &verify_and_settle_response);
        apply_error_log_fields(
            &mut message,
            code,
            payment_failure_message(&verify_and_settle_response.failure_reason, fallback),
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return payment_error_response(
            StatusCode::BAD_REQUEST,
            payment_failure_message(&verify_and_settle_response.failure_reason, fallback),
            code,
        );
    }

    let mut upstream_headers = filtered_upstream_headers(&headers, &snapshot.target.headers_policy);
    let endpoint_secret =
        match resolve_endpoint_signing_secret(&app_state, snapshot.endpoint_id).await {
            Ok(secret) => secret,
            Err(error) => {
                tracing::error!(
                    endpoint_id = %snapshot.endpoint_id,
                    error = %error,
                    "x402 endpoint signing secret resolution failed"
                );
                let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
                apply_verify_and_settle_log_fields(&mut message, &verify_and_settle_response);
                apply_error_log_fields(
                    &mut message,
                    "x402_signing_secret_unavailable",
                    error.to_string(),
                );
                emit_x402_log_best_effort(&app_state, message).await;
                return json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "x402 endpoint signing secret unavailable",
                    X402_GATEWAY_ERROR_TYPE,
                    "x402_signing_secret_unavailable",
                );
            }
        };
    let timestamp = current_unix_timestamp_seconds();
    let path_with_query = upstream_path_with_query(&upstream_url);
    if let Err(error) = inject_forward_signature_headers(
        &mut upstream_headers,
        snapshot.endpoint_id,
        &endpoint_secret,
        &timestamp,
        &upstream_method,
        &path_with_query,
        &body,
    ) {
        tracing::error!(error = %error, "x402 endpoint signing header injection failed");
        let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
        apply_verify_and_settle_log_fields(&mut message, &verify_and_settle_response);
        apply_error_log_fields(
            &mut message,
            "x402_signing_header_failed",
            error.to_string(),
        );
        emit_x402_log_best_effort(&app_state, message).await;
        return json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "x402 endpoint signing header failed",
            X402_GATEWAY_ERROR_TYPE,
            "x402_signing_header_failed",
        );
    }
    log_paid_upstream_request(&upstream_method, &upstream_url, &upstream_headers, &body);

    let upstream_result = proxy_paid_request(
        &http_client,
        upstream_method,
        upstream_url,
        upstream_headers,
        body,
        response_timeout(&snapshot),
    )
    .await;

    match upstream_result {
        Ok(upstream) => {
            let service_status = if upstream.status.is_success() {
                "succeeded"
            } else {
                "failed"
            };
            record_service_result_best_effort(
                &app_state,
                &verify_and_settle_response.activity_id,
                &payment_context.trace_id,
                &payment_snapshot.cache_billing_mode,
                service_status,
                i32::from(upstream.status.as_u16()),
                String::new(),
                upstream.response_hash.clone(),
            )
            .await;
            let mut message = build_x402_log_message(X402LogStage::UpstreamCompleted, &log_context);
            apply_verify_and_settle_log_fields(&mut message, &verify_and_settle_response);
            apply_upstream_log_fields(&mut message, &upstream, service_status, "");
            let response = upstream_response(
                upstream.status,
                &upstream.headers,
                upstream.body,
                &verify_and_settle_response.payment_response_header,
            );
            emit_x402_log_after_response_body_completion(app_state, response, message)
        }
        Err(error) => {
            let failure_reason = error.to_string();
            tracing::warn!(
                error = %failure_reason,
                "x402 paid upstream request failed"
            );
            record_service_result_best_effort(
                &app_state,
                &verify_and_settle_response.activity_id,
                &payment_context.trace_id,
                &payment_snapshot.cache_billing_mode,
                "gateway_error",
                0,
                failure_reason.clone(),
                String::new(),
            )
            .await;
            let mut message = build_x402_log_message(X402LogStage::GatewayError, &log_context);
            apply_verify_and_settle_log_fields(&mut message, &verify_and_settle_response);
            message.service_status = "gateway_error".to_string();
            message.failure_reason = failure_reason.clone();
            apply_error_log_fields(&mut message, "x402_upstream_failed", failure_reason);
            emit_x402_log_best_effort(&app_state, message).await;
            json_error_response(
                StatusCode::BAD_GATEWAY,
                "x402 upstream request failed",
                X402_GATEWAY_ERROR_TYPE,
                "x402_upstream_failed",
            )
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PaidUpstreamRequestError {
    Url(String),
    Method(String),
}

impl PaidUpstreamRequestError {
    fn client_message(&self) -> &'static str {
        match self {
            Self::Url(_) => "invalid x402 upstream URL",
            Self::Method(_) => "invalid x402 upstream method",
        }
    }

    fn error_code(&self) -> &'static str {
        match self {
            Self::Url(_) => "x402_invalid_upstream_url",
            Self::Method(_) => "x402_invalid_upstream_method",
        }
    }
}

impl std::fmt::Display for PaidUpstreamRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(error) => write!(f, "invalid x402 upstream URL: {error}"),
            Self::Method(error) => {
                write!(f, "invalid x402 upstream method: {error}")
            }
        }
    }
}

pub(crate) fn prepare_paid_upstream_request(
    snapshot: &X402EndpointSnapshot,
    _slug: &str,
    remaining_path: &str,
    inbound_method: &Method,
    query: Option<&str>,
) -> Result<(Method, reqwest::Url), PaidUpstreamRequestError> {
    let upstream_url = build_upstream_url(
        &snapshot.target.original_target_url,
        &snapshot.path,
        remaining_path,
        query,
    )
    .map_err(|error| PaidUpstreamRequestError::Url(error.to_string()))?;

    let upstream_method =
        method_for_upstream(inbound_method, snapshot).map_err(PaidUpstreamRequestError::Method)?;

    Ok((upstream_method, upstream_url))
}

fn current_unix_timestamp_seconds() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[allow(clippy::too_many_arguments)]
async fn record_service_result_best_effort(
    app_state: &AppState,
    activity_id: &str,
    trace_id: &str,
    cache_billing_mode: &str,
    service_status: &str,
    upstream_status_code: i32,
    failure_reason: String,
    response_hash: String,
) {
    let Some(client) = app_state.x402_payment_client().await else {
        tracing::warn!("x402 RecordServiceResult skipped: payment client missing");
        return;
    };

    let request = build_record_service_result_request(
        activity_id,
        trace_id,
        service_status,
        upstream_status_code,
        failure_reason,
        cache_billing_mode,
        response_hash,
    );
    let request = match payment_grpc_request_with_auth(
        request,
        app_state.config().x402.payment_service_key.expose(),
    ) {
        Ok(request) => request,
        Err(error) => {
            tracing::error!(error, "x402 RecordServiceResult auth metadata build failed");
            return;
        }
    };
    log_payment_grpc_request("RecordServiceResult", &request);
    let mut inner = client.inner();

    match tokio::time::timeout(
        app_state.config().x402.payment_timeout,
        inner.record_service_result(request),
    )
    .await
    {
        Ok(Ok(response)) => {
            log_payment_grpc_response("RecordServiceResult", &response);
            let response = response.into_inner();
            if !record_service_result_succeeded(&response) {
                tracing::warn!(
                    failure_reason = %payment_failure_message(
                        &response.failure_reason,
                        RECORD_SERVICE_RESULT_FAILED_MESSAGE,
                    ),
                    "x402 RecordServiceResult returned business failure"
                );
            }
        }
        Ok(Err(status)) => {
            tracing::warn!(status = %status, "x402 RecordServiceResult failed");
        }
        Err(_elapsed) => {
            tracing::warn!("x402 RecordServiceResult timed out");
        }
    }
}

fn build_record_service_result_request(
    activity_id: &str,
    trace_id: &str,
    service_status: &str,
    upstream_status_code: i32,
    failure_reason: String,
    cache_billing_mode: &str,
    response_hash: String,
) -> RecordServiceResultRequest {
    RecordServiceResultRequest {
        activity_id: activity_id.to_string(),
        trace_id: trace_id.to_string(),
        service_status: service_status.to_string(),
        upstream_status_code,
        failure_reason,
        cache_billing_mode: cache_billing_mode.to_string(),
        response_hash,
    }
}

fn trace_id_from_headers(headers: &HeaderMap) -> String {
    header_string(headers, "x-alephant-trace-id")
        .or_else(|| header_string(headers, "x-trace-id"))
        .or_else(|| header_string(headers, "x-request-id"))
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string())
}

fn request_id_from_headers(headers: &HeaderMap, trace_id: &str) -> String {
    header_string(headers, "x-request-id").unwrap_or_else(|| trace_id.to_string())
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestBodyReadError {
    TooLarge,
    ReadFailed,
}

fn request_body_limit(snapshot: &X402EndpointSnapshot) -> usize {
    usize::try_from(snapshot.policy.max_request_size.max(0)).unwrap_or(0)
}

pub(crate) fn content_length_exceeds_policy(headers: &HeaderMap, max_bytes: usize) -> bool {
    let max_bytes = u64::try_from(max_bytes).unwrap_or(u64::MAX);

    headers
        .get_all(header::CONTENT_LENGTH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|value| value.trim().parse::<u64>().ok())
        .any(|content_length| content_length > max_bytes)
}

pub(crate) async fn collect_limited_request_body(
    body: Body,
    max_bytes: usize,
) -> Result<Bytes, RequestBodyReadError> {
    match Limited::new(body, max_bytes).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) => {
            if error.downcast_ref::<LengthLimitError>().is_some() {
                Err(RequestBodyReadError::TooLarge)
            } else {
                tracing::warn!(error = %error, "x402 request body read failed");
                Err(RequestBodyReadError::ReadFailed)
            }
        }
    }
}

fn payload_too_large_response() -> GatewayResponse {
    json_error_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        "request body exceeds x402 policy limit",
        INVALID_REQUEST_ERROR_TYPE,
        "x402_payload_too_large",
    )
}

fn body_schema_validation_error_code(error: &BodySchemaValidationError) -> &'static str {
    match error {
        BodySchemaValidationError::InvalidJson(_) => "x402_body_json_invalid",
        BodySchemaValidationError::Mismatch(_) => "x402_body_schema_invalid",
        BodySchemaValidationError::InvalidSchema(_) => "x402_body_schema_config_invalid",
    }
}

fn body_schema_validation_error_message(error: &BodySchemaValidationError) -> &'static str {
    match error {
        BodySchemaValidationError::InvalidJson(_) => "request body must be valid JSON",
        BodySchemaValidationError::Mismatch(_) => {
            "request body does not match x402 endpoint schema"
        }
        BodySchemaValidationError::InvalidSchema(_) => "x402 endpoint body schema is invalid",
    }
}

fn body_schema_validation_error_response(error: &BodySchemaValidationError) -> GatewayResponse {
    match error {
        BodySchemaValidationError::InvalidJson(_) | BodySchemaValidationError::Mismatch(_) => {
            json_error_response(
                StatusCode::BAD_REQUEST,
                body_schema_validation_error_message(error),
                INVALID_REQUEST_ERROR_TYPE,
                body_schema_validation_error_code(error),
            )
        }
        BodySchemaValidationError::InvalidSchema(_) => json_error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            body_schema_validation_error_message(error),
            X402_GATEWAY_ERROR_TYPE,
            body_schema_validation_error_code(error),
        ),
    }
}

pub(crate) fn safe_policy_headers(headers: &HeaderMap) -> HeaderMap {
    let mut safe = HeaderMap::new();

    for (name, value) in headers {
        if should_strip_policy_header(name) || value.to_str().is_err() {
            continue;
        }

        safe.append(name, value.clone());
    }

    safe
}

fn should_strip_policy_header(name: &HeaderName) -> bool {
    let name = name.as_str();
    let lower = name.to_ascii_lowercase();

    is_payment_header(name)
        || is_sensitive_policy_header_name(&lower)
        || lower == ALEPHANT_SESSION_ID_HEADER
        || lower.starts_with("alephant-session-")
}

fn is_sensitive_policy_header_name(name: &str) -> bool {
    name == "authorization"
        || name == "proxy-authorization"
        || name == "proxy-authenticate"
        || name == "cookie"
        || name == "set-cookie"
        || name.contains("auth")
        || name.contains("cookie")
        || name.contains("key")
        || name.contains("token")
        || name.contains("secret")
        || name.contains("credential")
        || name.contains("signature")
        || name.contains("payment")
        || name.contains("session")
}

fn is_payment_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("payment-signature")
        || name
            .get(.."payment-".len().min(name.len()))
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("payment-"))
}

fn policy_denied_message(reason: &str) -> String {
    if reason.is_empty() {
        "x402 policy denied".to_string()
    } else {
        reason.to_string()
    }
}

fn payment_failure_message(reason: &str, fallback: &'static str) -> String {
    if reason.is_empty() {
        fallback.to_string()
    } else {
        reason.to_string()
    }
}

fn payment_required_error_message(response: &GetPaymentRequirementsResponse) -> String {
    if response.success {
        "Payment required".to_string()
    } else {
        payment_failure_message(
            &response.failure_reason,
            PAYMENT_REQUIREMENTS_FAILED_MESSAGE,
        )
    }
}

fn record_service_result_succeeded(response: &RecordServiceResultResponse) -> bool {
    response.success
}

fn verify_and_settle_payment_succeeded(response: &VerifyAndSettlePaymentResponse) -> bool {
    response.success
}

fn verify_and_settle_failure_stage(response: &VerifyAndSettlePaymentResponse) -> X402LogStage {
    if response.payment_status.eq_ignore_ascii_case("success") {
        X402LogStage::SettleFailed
    } else {
        X402LogStage::VerifyFailed
    }
}

pub(crate) fn response_timeout(snapshot: &X402EndpointSnapshot) -> Duration {
    let seconds = snapshot.policy.timeout_seconds.max(1);
    Duration::from_secs(u64::try_from(seconds).unwrap_or(1))
}

pub(crate) fn endpoint_payment_snapshot(
    snapshot: &X402EndpointSnapshot,
) -> EndpointPaymentSnapshot {
    EndpointPaymentSnapshot {
        workspace_id: snapshot.workspace_id.to_string(),
        endpoint_id: snapshot.endpoint_id.to_string(),
        agent_id: snapshot
            .agent_id
            .map_or_else(String::new, |id| id.to_string()),
        snapshot_revision: snapshot.snapshot_revision.to_string(),
        method: snapshot.method.clone(),
        path: snapshot.path.clone(),
        price: Some(Money {
            amount: snapshot.price_amount.clone(),
            asset: snapshot.asset.clone(),
            network: snapshot.network.clone(),
        }),
        receive_wallet_address: snapshot.receive_wallet_address.clone(),
        ale_receive_wallet_address: String::new(),
        fee_wallet_address: String::new(),
        fee_bps: snapshot.fee_bps,
        facilitator: snapshot.policy.facilitator.clone().unwrap_or_default(),
        cache_billing_mode: snapshot.policy.cache_billing_mode.clone(),
        resource_url: snapshot.target.original_target_url.clone(),
        resource_description: snapshot.name.clone(),
        resource_mime_type: String::new(),
        split_treasury_id: String::new(),
    }
}

fn payment_context(
    headers: &HeaderMap,
    body: &Bytes,
    trace_id: String,
    request_id: String,
) -> PaymentRequestContext {
    PaymentRequestContext {
        trace_id,
        request_id,
        request_hash: hash_body(body),
        buyer_ip_hash: header_string(headers, "x-forwarded-for")
            .or_else(|| header_string(headers, "x-real-ip"))
            .map_or_else(String::new, |value| hash_sensitive(&value)),
        user_agent_hash: header_string(headers, "user-agent")
            .map_or_else(String::new, |value| hash_sensitive(&value)),
    }
}

fn money_fields(price: Option<&Money>) -> (&str, &str, &str) {
    price.map_or(("", "", ""), |price| {
        (
            price.amount.as_str(),
            price.asset.as_str(),
            price.network.as_str(),
        )
    })
}

pub(crate) fn build_payment_required_body(response: &GetPaymentRequirementsResponse) -> Vec<u8> {
    let body = ErrorResponse {
        error: ErrorDetails {
            message: payment_required_error_message(response),
            r#type: Some(PAYMENT_REQUIRED_ERROR_TYPE.to_string()),
            param: None,
            code: Some("x402_payment_required".to_string()),
        },
    };
    serde_json::to_vec(&body).unwrap_or_default()
}

fn payment_required_response(response: &GetPaymentRequirementsResponse) -> GatewayResponse {
    let mut response_builder = Response::builder().status(StatusCode::PAYMENT_REQUIRED);
    response_builder = response_builder.header(header::CONTENT_TYPE, "application/json");
    if !response.payment_required_header.is_empty() {
        response_builder =
            response_builder.header(PAYMENT_REQUIRED_HEADER, &response.payment_required_header);
    }

    response_builder
        .body(Body::from(build_payment_required_body(response)))
        .unwrap_or_else(|error| {
            tracing::error!(error = %error, "x402 payment required response build failed");
            empty_response(StatusCode::PAYMENT_REQUIRED)
        })
}

fn payment_error_response(
    status: StatusCode,
    message: impl Into<String>,
    code: &'static str,
) -> GatewayResponse {
    json_error_response(status, message, INVALID_REQUEST_ERROR_TYPE, code)
}

fn json_error_response(
    status: StatusCode,
    message: impl Into<String>,
    ty: impl Into<String>,
    code: impl Into<String>,
) -> GatewayResponse {
    let body = serde_json::to_vec(&ErrorResponse {
        error: ErrorDetails {
            message: message.into(),
            r#type: Some(ty.into()),
            param: None,
            code: Some(code.into()),
        },
    })
    .unwrap_or_default();

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|error| {
            tracing::error!(error = %error, "x402 JSON response build failed");
            empty_response(status)
        })
}

fn upstream_response(
    status: StatusCode,
    headers: &HeaderMap,
    body: Bytes,
    payment_response_header: &str,
) -> GatewayResponse {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = sanitized_upstream_response_headers(headers);

    if !payment_response_header.is_empty() {
        if let Ok(value) = HeaderValue::from_str(payment_response_header) {
            response
                .headers_mut()
                .insert(PAYMENT_RESPONSE_HEADER, value);
        } else {
            tracing::warn!("x402 settlement returned invalid Payment-Response header");
        }
    }

    response
}

fn sanitized_upstream_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut sanitized = HeaderMap::new();
    let connection_tokens = connection_header_tokens(headers);

    for (name, value) in headers {
        if should_strip_upstream_response_header(name, &connection_tokens) {
            continue;
        }

        sanitized.append(name, value.clone());
    }

    sanitized
}

fn connection_header_tokens(headers: &HeaderMap) -> HashSet<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect()
}

fn should_strip_upstream_response_header(
    name: &HeaderName,
    connection_tokens: &HashSet<HeaderName>,
) -> bool {
    let name_str = name.as_str();

    is_payment_header(name_str)
        || is_hop_by_hop_or_framing_header(name_str)
        || connection_tokens.contains(name)
}

fn is_hop_by_hop_or_framing_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("upgrade")
        || name.eq_ignore_ascii_case("te")
        || name.eq_ignore_ascii_case("trailer")
        || name.eq_ignore_ascii_case("keep-alive")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("proxy-authenticate")
        || name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("http2-settings")
}

fn empty_response(status: StatusCode) -> GatewayResponse {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use axum_core::body::Body;
    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue, Response, StatusCode, header};
    use http_body_util::BodyExt;
    use uuid::Uuid;

    use crate::{
        app::build_test_app,
        config::Config,
        payment_proto::{
            EndpointPaymentSnapshot as PaymentEndpointSnapshot, GetPaymentRequirementsRequest,
            GetPaymentRequirementsResponse, RecordServiceResultResponse,
            RequestContext as PaymentRequestContext, VerifyAndSettlePaymentRequest,
            VerifyAndSettlePaymentResponse,
        },
        store::router::DbX402PaymentActivityLogFields,
        x402::{
            body_schema::BodySchemaValidationError,
            log::{X402LogStage, X402PaymentLogMessage, hash_sensitive},
            service::{
                RequestBodyReadError, X402LogContext, apply_activity_log_fields,
                apply_payment_requirements_log_fields, apply_verify_and_settle_log_fields,
                body_schema_validation_error_code, body_schema_validation_error_message,
                body_schema_validation_error_response, build_payment_required_body,
                build_record_service_result_request, build_x402_log_message,
                collect_limited_request_body, content_length_exceeds_policy,
                debug_env_flag_value_enabled, endpoint_payment_snapshot,
                handle_payment_requirements, json_error_response, payment_error_response,
                payment_grpc_log_label, payment_grpc_request_with_auth,
                payment_required_error_message, payment_required_response,
                prepare_paid_upstream_request, record_service_result_succeeded,
                redacted_payment_grpc_metadata, response_timeout, safe_policy_headers,
                upstream_response, verify_and_settle_payment_succeeded,
                with_on_response_body_completion, x402_upstream_body_for_debug_log,
            },
            types::{
                X402EndpointSnapshot, X402OriginAuthSnapshot, X402PolicySnapshot,
                X402TargetSnapshot,
            },
        },
    };

    fn test_snapshot() -> X402EndpointSnapshot {
        X402EndpointSnapshot {
            endpoint_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            workspace_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            agent_id: Some(Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap()),
            status: "active".to_string(),
            name: "Weather API".to_string(),
            slug: "weather".to_string(),
            endpoint_type: Some("agent".to_string()),
            method: "POST".to_string(),
            path: "/weather".to_string(),
            pricing_model: "fixed".to_string(),
            price_amount: "0.25".to_string(),
            asset: "USDC".to_string(),
            network: "base".to_string(),
            receive_wallet_address: "0xabc".to_string(),
            fee_bps: 100,
            body_schema: serde_json::Value::Null,
            target: X402TargetSnapshot {
                kind: "http".to_string(),
                original_target_url: "https://api.example.com".to_string(),
                forward_method: "preserve".to_string(),
                path_rewrite: serde_json::json!({}),
                headers_policy: vec![],
                origin_signature_required: false,
            },
            origin_auth: X402OriginAuthSnapshot {
                active_secret_version: 1,
            },
            policy: X402PolicySnapshot {
                policy_id: Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
                buyer_access: "public".to_string(),
                rate_limit_rpm: 60,
                max_request_size: 1024,
                timeout_seconds: 0,
                payment_retry_attempts: 1,
                schema_validation_required: false,
                facilitator: Some("facilitator-a".to_string()),
                cache_billing_mode: "full".to_string(),
                cache_hit_discount_bps: 0,
            },
            snapshot_revision: 42,
            config_revision: Some(42),
            compiled_at: None,
        }
    }

    #[test]
    fn endpoint_payment_snapshot_maps_money_and_identifiers() {
        let snapshot = test_snapshot();

        let payment_snapshot = endpoint_payment_snapshot(&snapshot);

        assert_eq!(
            payment_snapshot.workspace_id,
            "22222222-2222-2222-2222-222222222222"
        );
        assert_eq!(
            payment_snapshot.endpoint_id,
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(
            payment_snapshot.agent_id,
            "33333333-3333-3333-3333-333333333333"
        );
        assert_eq!(payment_snapshot.snapshot_revision, "42");
        assert_eq!(payment_snapshot.method, "POST");
        assert_eq!(payment_snapshot.path, "/weather");
        assert_eq!(payment_snapshot.receive_wallet_address, "0xabc");
        assert_eq!(payment_snapshot.facilitator, "facilitator-a");
        assert_eq!(payment_snapshot.cache_billing_mode, "full");
        let price = payment_snapshot.price.unwrap();
        assert_eq!(price.amount, "0.25");
        assert_eq!(price.asset, "USDC");
        assert_eq!(price.network, "base");
    }

    #[test]
    fn response_timeout_clamps_to_at_least_one_second() {
        assert_eq!(response_timeout(&test_snapshot()).as_secs(), 1);
    }

    #[test]
    fn payment_grpc_request_adds_authorization_metadata() {
        let request = GetPaymentRequirementsRequest::default();
        let request = payment_grpc_request_with_auth(request, "secret-key")
            .expect("metadata should be valid");

        assert_eq!(
            request
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer secret-key")
        );
    }

    #[test]
    fn payment_grpc_request_rejects_empty_authorization_key() {
        let request = GetPaymentRequirementsRequest::default();
        let error = payment_grpc_request_with_auth(request, "   ")
            .expect_err("empty key should be rejected");

        assert_eq!(error, "PAYMENT_SERVICE_KEY is empty");
    }

    #[test]
    fn payment_grpc_debug_metadata_redacts_authorization() {
        let request =
            payment_grpc_request_with_auth(GetPaymentRequirementsRequest::default(), "secret-key")
                .expect("metadata");

        let metadata = redacted_payment_grpc_metadata(request.metadata());

        assert_eq!(
            metadata.get("authorization").map(String::as_str),
            Some("Bearer *****")
        );
    }

    #[test]
    fn content_length_over_policy_max_is_rejected_before_body_read() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("11"));

        assert!(content_length_exceeds_policy(&headers, 10));
        assert!(!content_length_exceeds_policy(&headers, 11));
    }

    #[tokio::test]
    async fn limited_body_collect_returns_payload_too_large_on_overflow() {
        let error = collect_limited_request_body(Body::from("too large"), 3)
            .await
            .expect_err("body should exceed limit");

        assert!(matches!(error, RequestBodyReadError::TooLarge));

        let body = collect_limited_request_body(Body::from("ok"), 3)
            .await
            .expect("body under limit");
        assert_eq!(body, Bytes::from_static(b"ok"));
    }

    #[test]
    fn x402_upstream_body_debug_log_prints_utf8_text() {
        let body = Bytes::from_static("{\"message\":\"给我一份 x402 的使用指南\"}".as_bytes());

        assert_eq!(
            x402_upstream_body_for_debug_log(&body),
            "{\"message\":\"给我一份 x402 的使用指南\"}"
        );
    }

    #[tokio::test]
    async fn body_schema_validation_error_maps_to_invalid_request_response() {
        let error = BodySchemaValidationError::Mismatch("missing model".to_string());

        let response = body_schema_validation_error_response(&error);

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(value["error"]["code"], "x402_body_schema_invalid");
    }

    #[tokio::test]
    async fn invalid_endpoint_body_schema_maps_to_gateway_error_response() {
        let error = BodySchemaValidationError::InvalidSchema("invalid schema".to_string());

        let response = body_schema_validation_error_response(&error);

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "x402_gateway_error");
        assert_eq!(value["error"]["code"], "x402_body_schema_config_invalid");
    }

    #[test]
    fn body_schema_validation_runs_before_policy_stage_codes() {
        let json_error = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let invalid_json = BodySchemaValidationError::InvalidJson(json_error);
        let mismatch = BodySchemaValidationError::Mismatch("missing model".to_string());

        assert_eq!(
            body_schema_validation_error_code(&invalid_json),
            "x402_body_json_invalid"
        );
        assert_eq!(
            body_schema_validation_error_code(&mismatch),
            "x402_body_schema_invalid"
        );
        assert_eq!(
            body_schema_validation_error_message(&mismatch),
            "request body does not match x402 endpoint schema"
        );
    }

    #[test]
    fn safe_policy_headers_strip_sensitive_payment_and_auth_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("PAYMENT-SIGNATURE", HeaderValue::from_static("sig"));
        headers.insert("Payment-Trace", HeaderValue::from_static("payment"));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("bearer"));
        headers.insert(header::COOKIE, HeaderValue::from_static("session"));
        headers.insert("x-api-key", HeaderValue::from_static("key"));
        headers.insert("openai-api-key", HeaderValue::from_static("openai"));
        headers.insert("x-goog-api-key", HeaderValue::from_static("goog"));
        headers.insert(
            "x-amz-security-token",
            HeaderValue::from_static("aws-token"),
        );
        headers.insert("x-auth-token", HeaderValue::from_static("auth-token"));
        headers.insert("x-auth", HeaderValue::from_static("auth"));
        headers.insert(
            "x-forwarded-auth",
            HeaderValue::from_static("forwarded-auth"),
        );
        headers.insert("x-cookie", HeaderValue::from_static("cookie"));
        headers.insert("session-cookie", HeaderValue::from_static("session-cookie"));
        headers.insert("x-client-secret", HeaderValue::from_static("client-secret"));
        headers.insert(header::USER_AGENT, HeaderValue::from_static("agent"));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/plain"));
        headers.insert("x-request-id", HeaderValue::from_static("request-1"));

        let safe = safe_policy_headers(&headers);

        assert!(!safe.contains_key("payment-signature"));
        assert!(!safe.contains_key("payment-trace"));
        assert!(!safe.contains_key(header::AUTHORIZATION));
        assert!(!safe.contains_key(header::COOKIE));
        assert!(!safe.contains_key("x-api-key"));
        assert!(!safe.contains_key("openai-api-key"));
        assert!(!safe.contains_key("x-goog-api-key"));
        assert!(!safe.contains_key("x-amz-security-token"));
        assert!(!safe.contains_key("x-auth-token"));
        assert!(!safe.contains_key("x-auth"));
        assert!(!safe.contains_key("x-forwarded-auth"));
        assert!(!safe.contains_key("x-cookie"));
        assert!(!safe.contains_key("session-cookie"));
        assert!(!safe.contains_key("x-client-secret"));
        assert_eq!(
            safe.get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some("agent")
        );
        assert_eq!(
            safe.get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            safe.get(header::ACCEPT)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain")
        );
        assert_eq!(
            safe.get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("request-1")
        );
    }

    #[test]
    fn paid_upstream_request_validation_rejects_invalid_url_and_method() {
        let mut snapshot = test_snapshot();
        snapshot.target.original_target_url = "https://api.example.com?token=leak".to_string();

        let error = prepare_paid_upstream_request(
            &snapshot,
            "weather",
            "forecast",
            &http::Method::GET,
            None,
        )
        .expect_err("query-bearing upstream URL should be rejected");

        assert!(error.to_string().contains("upstream URL"));

        let mut snapshot = test_snapshot();
        snapshot.target.forward_method = "configured".to_string();
        snapshot.method = "not a method".to_string();

        let error = prepare_paid_upstream_request(
            &snapshot,
            "weather",
            "forecast",
            &http::Method::GET,
            None,
        )
        .expect_err("invalid configured upstream method should be rejected");

        assert!(error.to_string().contains("upstream method"));
    }

    #[test]
    fn paid_upstream_request_uses_snapshot_path_instead_of_public_slug() {
        let mut snapshot = test_snapshot();
        snapshot.path = "/internal/health".to_string();

        let (_method, upstream_url) = prepare_paid_upstream_request(
            &snapshot,
            "gethealth",
            "",
            &http::Method::GET,
            Some("bdd=123&ftty=345&ayjj=234"),
        )
        .expect("upstream request should build");

        assert_eq!(
            upstream_url.as_str(),
            "https://api.example.com/internal/health?bdd=123&ftty=345&ayjj=234"
        );
    }

    #[tokio::test]
    async fn upstream_response_sanitizes_sensitive_and_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("x-remove, close"),
        );
        headers.insert("x-remove", HeaderValue::from_static("remove-me"));
        headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("4"));
        headers.insert("Payment-Response", HeaderValue::from_static("upstream"));
        headers.insert("Payment-Required", HeaderValue::from_static("required"));
        headers.insert("Payment-Trace", HeaderValue::from_static("trace"));
        headers.insert("x-benign", HeaderValue::from_static("keep"));

        let response = upstream_response(
            StatusCode::OK,
            &headers,
            Bytes::from_static(b"body"),
            "gateway-settlement",
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("payment-response")
                .and_then(|value| value.to_str().ok()),
            Some("gateway-settlement")
        );
        assert!(!response.headers().contains_key("payment-required"));
        assert!(!response.headers().contains_key("payment-trace"));
        assert!(!response.headers().contains_key(header::CONNECTION));
        assert!(!response.headers().contains_key("x-remove"));
        assert!(!response.headers().contains_key(header::TRANSFER_ENCODING));
        assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
        assert_eq!(
            response
                .headers()
                .get("x-benign")
                .and_then(|value| value.to_str().ok()),
            Some("keep")
        );

        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        assert_eq!(body, Bytes::from_static(b"body"));
    }

    #[tokio::test]
    async fn response_body_completion_callback_runs_after_body_consumed() {
        let called = Arc::new(AtomicBool::new(false));
        let called_for_callback = called.clone();
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("done"))
            .unwrap();

        let response = with_on_response_body_completion(
            response,
            Arc::new(move || {
                called_for_callback.store(true, Ordering::SeqCst);
            }),
        );

        assert!(!called.load(Ordering::SeqCst));

        let body = response.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(&body[..], b"done");
        assert!(called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn payment_requirements_missing_client_returns_service_unavailable() {
        let app = build_test_app(Config::default()).await.expect("test app");
        let response = handle_payment_requirements(
            app.state,
            endpoint_payment_snapshot(&test_snapshot()),
            PaymentRequestContext {
                trace_id: "trace-1".to_string(),
                request_id: "request-1".to_string(),
                request_hash: "request-hash".to_string(),
                buyer_ip_hash: String::new(),
                user_agent_hash: String::new(),
            },
            X402LogContext::from_request(
                "trace-1".to_string(),
                "request-1".to_string(),
                String::new(),
                "weather".to_string(),
                "POST".to_string(),
                "/x402/weather".to_string(),
            )
            .with_resolved_snapshot(&test_snapshot(), "db"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["error"]["message"],
            "x402 payment service unavailable"
        );
        assert_eq!(value["error"]["type"], "x402_gateway_error");
        assert_eq!(value["error"]["code"], "x402_payment_unavailable");
    }

    #[tokio::test]
    async fn paid_payment_error_response_uses_caller_status_not_402() {
        let response = payment_error_response(
            StatusCode::BAD_REQUEST,
            "x402 payment verification failed",
            "x402_verify_failed",
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(value["error"]["code"], "x402_verify_failed");
    }

    #[tokio::test]
    async fn payment_required_response_sets_status_content_type_and_header() {
        let response = payment_required_response(&GetPaymentRequirementsResponse {
            payment_required_header: "x402 header value".to_string(),
            accepts: vec![crate::payment_proto::PaymentAcceptSummary {
                scheme: "exact".to_string(),
                network: "base".to_string(),
                amount: "0.25".to_string(),
                asset: "USDC".to_string(),
                pay_to: "0xabc".to_string(),
                resource: String::new(),
                facilitator: String::new(),
                accept_hash: String::new(),
            }],
            success: true,
            ..Default::default()
        });

        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response
                .headers()
                .get("payment-required")
                .and_then(|value| value.to_str().ok()),
            Some("x402 header value")
        );

        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "payment_required");
        assert_eq!(value["error"]["code"], "x402_payment_required");
        assert!(value.get("paymentRequirements").is_none());
    }

    #[tokio::test]
    async fn json_error_response_sets_status_and_error_code_body() {
        let response = json_error_response(
            StatusCode::FORBIDDEN,
            "x402 policy denied",
            "x402_policy_denied",
            "x402_policy_unavailable",
        );

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );

        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["message"], "x402 policy denied");
        assert_eq!(value["error"]["type"], "x402_policy_denied");
        assert_eq!(value["error"]["code"], "x402_policy_unavailable");
    }

    #[test]
    fn verify_and_settle_request_defaults_missing_activity_id_to_empty_string() {
        let request = VerifyAndSettlePaymentRequest {
            activity_id: String::new(),
            snapshot: Some(PaymentEndpointSnapshot::default()),
            context: Some(PaymentRequestContext::default()),
            payment_signature: "sig".into(),
        };

        assert_eq!(request.activity_id, "");
        assert_eq!(request.payment_signature, "sig");
    }

    #[test]
    fn payment_grpc_log_label_includes_direction_and_method() {
        assert_eq!(
            payment_grpc_log_label("request", "VerifyAndSettlePayment"),
            "x402 payment gRPC request: VerifyAndSettlePayment"
        );
        assert_eq!(
            payment_grpc_log_label("response", "RecordServiceResult"),
            "x402 payment gRPC response: RecordServiceResult"
        );
    }

    #[test]
    fn payment_grpc_debug_env_flags_only_enable_on_true() {
        assert!(debug_env_flag_value_enabled("true"));
        assert!(debug_env_flag_value_enabled("TRUE"));
        assert!(!debug_env_flag_value_enabled("false"));
        assert!(!debug_env_flag_value_enabled("1"));
        assert!(!debug_env_flag_value_enabled(""));
    }

    #[test]
    fn payment_required_body_contains_only_error_object() {
        let response = crate::payment_proto::GetPaymentRequirementsResponse {
            activity_id: "activity-1".to_string(),
            payment_required_header: "header-value".to_string(),
            expires_at: Some(crate::google::protobuf::Timestamp {
                seconds: 1_760_000_000,
                nanos: 0,
            }),
            success: true,
            accepts: vec![crate::payment_proto::PaymentAcceptSummary {
                scheme: "exact".to_string(),
                network: "base".to_string(),
                asset: "USDC".to_string(),
                amount: "0.25".to_string(),
                pay_to: "0xabc".to_string(),
                resource: "https://api.example.com/weather".to_string(),
                facilitator: "facilitator-a".to_string(),
                accept_hash: "accept-hash".to_string(),
            }],
            ..Default::default()
        };

        let body = build_payment_required_body(&response);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["error"]["type"], "payment_required");
        assert_eq!(value["error"]["code"], "x402_payment_required");
        assert_eq!(value["error"]["message"], "Payment required");
        assert!(value.get("paymentRequirements").is_none());
        assert!(value.get("accepts").is_none());
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("payTo"));
        assert!(!serialized.contains("acceptHash"));
        assert!(!serialized.contains("facilitator-a"));
    }

    #[test]
    fn payment_required_body_uses_failure_reason_when_success_false() {
        let response = GetPaymentRequirementsResponse {
            failure_reason: "payment configuration is unavailable".to_string(),
            success: false,
            ..Default::default()
        };

        let body = build_payment_required_body(&response);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["error"]["message"],
            "payment configuration is unavailable"
        );
        assert_eq!(
            payment_required_error_message(&GetPaymentRequirementsResponse {
                success: false,
                ..Default::default()
            }),
            "x402 payment requirements failed"
        );
    }

    #[test]
    fn payment_requirements_log_fields_use_first_accept_summary() {
        let mut message = X402PaymentLogMessage::new(X402LogStage::PaymentRequired);

        apply_payment_requirements_log_fields(
            &mut message,
            &GetPaymentRequirementsResponse {
                activity_id: "55555555-5555-5555-5555-555555555555".to_string(),
                success: true,
                accepts: vec![
                    crate::payment_proto::PaymentAcceptSummary {
                        scheme: "exact".to_string(),
                        network: "base".to_string(),
                        asset: "USDC".to_string(),
                        amount: "1.25".to_string(),
                        pay_to: "0xseller".to_string(),
                        resource: "https://api.example.com/weather".to_string(),
                        facilitator: "coinbase".to_string(),
                        accept_hash: "accept-1".to_string(),
                    },
                    crate::payment_proto::PaymentAcceptSummary {
                        scheme: "exact".to_string(),
                        network: "ethereum".to_string(),
                        asset: "USDC".to_string(),
                        amount: "9.99".to_string(),
                        pay_to: "0xother".to_string(),
                        resource: "https://api.example.com/other".to_string(),
                        facilitator: "other".to_string(),
                        accept_hash: "accept-2".to_string(),
                    },
                ],
                ..Default::default()
            },
        );

        assert_eq!(message.activity_id, "55555555-5555-5555-5555-555555555555");
        assert_eq!(message.gross_revenue, 1.25);
        assert_eq!(message.asset, "USDC");
        assert_eq!(message.network, "base");
        assert_eq!(message.seller_receive_wallet_address, "0xseller");
        assert_eq!(message.facilitator, "coinbase");
    }

    #[test]
    fn payment_requirements_log_fields_tolerate_empty_accepts() {
        let mut message = X402PaymentLogMessage::new(X402LogStage::PaymentRequired);
        message.gross_revenue = 9.99;
        message.asset = "USDC".to_string();
        message.network = "base".to_string();
        message.seller_receive_wallet_address = "0xsnapshot".to_string();
        message.facilitator = "snapshot-facilitator".to_string();

        apply_payment_requirements_log_fields(
            &mut message,
            &GetPaymentRequirementsResponse {
                activity_id: "55555555-5555-5555-5555-555555555555".to_string(),
                success: true,
                accepts: vec![],
                ..Default::default()
            },
        );

        assert_eq!(message.activity_id, "55555555-5555-5555-5555-555555555555");
        assert_eq!(message.gross_revenue, 0.0);
        assert_eq!(message.asset, "");
        assert_eq!(message.network, "");
        assert_eq!(message.seller_receive_wallet_address, "");
        assert_eq!(message.facilitator, "");
    }

    #[test]
    fn paid_call_log_matches_clickhouse_fields_without_signature_hash() {
        let raw_signature = "raw-payment-signature";
        let signature_hash = hash_sensitive(raw_signature);
        let payment_context = PaymentRequestContext {
            trace_id: "trace-1".to_string(),
            request_id: "request-1".to_string(),
            request_hash: "request-hash".to_string(),
            buyer_ip_hash: "ip-hash".to_string(),
            user_agent_hash: "ua-hash".to_string(),
        };
        let context = X402LogContext::from_request(
            "trace-1".to_string(),
            "request-1".to_string(),
            "session-1".to_string(),
            "weather".to_string(),
            "POST".to_string(),
            "/x402/weather".to_string(),
        )
        .with_resolved_snapshot(&test_snapshot(), "redis")
        .with_payment_context(&payment_context)
        .with_upstream_url("https://api.example.com/weather".to_string());

        let mut message = build_x402_log_message(X402LogStage::UpstreamCompleted, &context);
        apply_verify_and_settle_log_fields(
            &mut message,
            &VerifyAndSettlePaymentResponse {
                activity_id: "55555555-5555-5555-5555-555555555555".to_string(),
                payment_status: "success".to_string(),
                settlement_status: "success".to_string(),
                buyer_wallet: "0xbuyer".to_string(),
                payment_signature_hash: signature_hash.clone(),
                facilitator: "facilitator-a".to_string(),
                split_status: "complete".to_string(),
                tx_hash: "0xtx".to_string(),
                ..Default::default()
            },
        );

        assert_eq!(message.trace_id, "trace-1");
        assert_eq!(
            message.workspace_id,
            test_snapshot().workspace_id.to_string()
        );
        assert_eq!(message.endpoint_id, test_snapshot().endpoint_id.to_string());
        assert_eq!(
            message.agent_id,
            test_snapshot().agent_id.unwrap().to_string()
        );
        assert_eq!(message.agent_session_id, "session-1");
        assert_eq!(message.buyer_wallet, "0xbuyer");
        assert_eq!(message.activity_id, "55555555-5555-5555-5555-555555555555");
        assert_eq!(message.payment_status, "success");
        assert_eq!(message.settlement_status, "success");

        let json = serde_json::to_string(&message).unwrap();
        assert!(!json.contains("payment_signature_hash"));
        assert!(!json.contains(&signature_hash));
        assert!(json.contains("\"direction\":\"inbound\""));
        assert!(json.contains("\"source\":\"ai_gateway\""));
        assert!(!json.contains(raw_signature));
    }

    #[test]
    fn activity_log_fields_enrich_clickhouse_columns() {
        let mut message = X402PaymentLogMessage::new(X402LogStage::UpstreamCompleted);
        let settled_at = chrono::DateTime::parse_from_rfc3339("2026-05-22T01:02:03Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let available_at = chrono::DateTime::parse_from_rfc3339("2026-05-22T01:03:03Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let verified_at = chrono::DateTime::parse_from_rfc3339("2026-05-22T01:01:03Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        apply_activity_log_fields(
            &mut message,
            DbX402PaymentActivityLogFields {
                ale_receive_wallet_address: Some("0xplatform".to_string()),
                fee_wallet_address: Some("0xfee".to_string()),
                ai_cost: Some(0.42),
                trace_status: Some("Complete".to_string()),
                settled_at: Some(settled_at),
                available_at: Some(available_at),
                verified_at: Some(verified_at),
            },
        );

        assert_eq!(message.platform_receive_wallet_address, "0xplatform");
        assert_eq!(message.fee_wallet_address, "0xfee");
        assert_eq!(message.ai_cost, 0.42);
        assert_eq!(message.trace_status, "Complete");
        assert_eq!(message.settled_at, Some(settled_at));
        assert_eq!(message.available_at, Some(available_at));
        assert_eq!(message.verify_time, Some(verified_at));
    }

    #[test]
    fn verify_and_settle_log_fields_fill_payment_and_settlement_columns() {
        let mut message = X402PaymentLogMessage::new(X402LogStage::UpstreamCompleted);

        apply_verify_and_settle_log_fields(
            &mut message,
            &VerifyAndSettlePaymentResponse {
                activity_id: "66666666-6666-6666-6666-666666666666".into(),
                payment_status: "success".into(),
                settlement_status: "success".into(),
                buyer_wallet: "0xbuyer".into(),
                facilitator: "facilitator-a".into(),
                tx_hash: "0xtx".into(),
                gross: Some(crate::payment_proto::Money {
                    amount: "1.25".into(),
                    asset: "USDC".into(),
                    network: "base".into(),
                }),
                alephant_fee: Some(crate::payment_proto::Money {
                    amount: "0.05".into(),
                    asset: "USDC".into(),
                    network: "base".into(),
                }),
                net: Some(crate::payment_proto::Money {
                    amount: "1.20".into(),
                    asset: "USDC".into(),
                    network: "base".into(),
                }),
                ..Default::default()
            },
        );

        assert_eq!(message.activity_id, "66666666-6666-6666-6666-666666666666");
        assert_eq!(message.payment_status, "success");
        assert_eq!(message.settlement_status, "success");
        assert_eq!(message.buyer_wallet, "0xbuyer");
        assert_eq!(message.facilitator, "facilitator-a");
        assert_eq!(message.tx_hash, "0xtx");
        assert_eq!(message.gross_revenue, 1.25);
        assert_eq!(message.alephant_fee, 0.05);
        assert_eq!(message.net_revenue, 1.20);
        assert_eq!(message.asset, "USDC");
        assert_eq!(message.network, "base");
    }

    #[test]
    fn x402_payment_success_uses_response_success_flags() {
        let response = VerifyAndSettlePaymentResponse {
            payment_status: "failed".to_string(),
            success: true,
            ..Default::default()
        };
        assert!(verify_and_settle_payment_succeeded(&response));

        let response = VerifyAndSettlePaymentResponse {
            payment_status: "success".to_string(),
            success: false,
            ..Default::default()
        };
        assert!(!verify_and_settle_payment_succeeded(&response));
    }

    #[test]
    fn record_service_result_success_uses_response_success_flag() {
        assert!(record_service_result_succeeded(
            &RecordServiceResultResponse {
                success: true,
                ..Default::default()
            }
        ));
        assert!(!record_service_result_succeeded(
            &RecordServiceResultResponse {
                success: false,
                ..Default::default()
            }
        ));
    }

    #[test]
    fn record_service_result_request_uses_current_proto_fields() {
        let request = build_record_service_result_request(
            "settle-activity",
            "trace-1",
            "success",
            200,
            String::new(),
            "disabled",
            "response-hash".to_string(),
        );

        assert_eq!(request.activity_id, "settle-activity");
        assert_eq!(request.trace_id, "trace-1");
        assert_eq!(request.service_status, "success");
        assert_eq!(request.upstream_status_code, 200);
        assert_eq!(request.failure_reason, "");
        assert_eq!(request.cache_billing_mode, "disabled");
        assert_eq!(request.response_hash, "response-hash");
    }
}
