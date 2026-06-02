use chrono::{DateTime, Utc};
use reqwest::header;
use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    app_state::AppState, error::logger::LoggerError, types::secret::Secret,
};

pub const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";
const X402_PAYMENT_LOG_HTTP_PATH: &str = "/v1/log/x402-payment";
const DEBUG_HEADERS_ENV: &str = "AI_GATEWAY_DEBUG_HEADERS";
const DEBUG_BODY_ENV: &str = "AI_GATEWAY_DEBUG_BODY";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum X402LogStage {
    SnapshotMiss,
    SnapshotError,
    PolicyDenied,
    PaymentRequired,
    VerifyFailed,
    SettleFailed,
    UpstreamCompleted,
    GatewayError,
}

#[derive(Debug, Clone, Serialize)]
pub struct X402PaymentLogMessage {
    pub event_time: DateTime<Utc>,
    pub workspace_id: String,
    pub activity_id: String,
    pub endpoint_id: String,
    pub agent_id: String,
    pub trace_id: String,
    pub direction: String,
    pub payment_status: String,
    pub settlement_status: String,
    pub settled_at: Option<DateTime<Utc>>,
    pub service_status: String,
    pub available_at: Option<DateTime<Utc>>,
    pub ledger_status: String,
    pub source: String,
    pub facilitator: String,
    pub network: String,
    pub asset: String,
    pub gross_revenue: f64,
    pub alephant_fee: f64,
    pub net_revenue: f64,
    pub ai_cost: f64,
    pub tx_hash: String,
    pub platform_receive_wallet_address: String,
    pub seller_receive_wallet_address: String,
    pub fee_wallet_address: String,
    pub buyer_wallet: String,
    pub fund_status: String,
    pub trace_status: String,
    pub agent_session_id: String,
    pub failure_reason: String,
    pub created_at: DateTime<Utc>,
    pub verify_time: Option<DateTime<Utc>>,
}

impl X402PaymentLogMessage {
    #[must_use]
    pub fn new(_stage: X402LogStage) -> Self {
        let now = Utc::now();
        Self {
            event_time: now,
            workspace_id: ZERO_UUID.to_string(),
            activity_id: ZERO_UUID.to_string(),
            endpoint_id: ZERO_UUID.to_string(),
            agent_id: ZERO_UUID.to_string(),
            trace_id: String::new(),
            direction: "inbound".to_string(),
            payment_status: String::new(),
            settlement_status: String::new(),
            settled_at: None,
            service_status: String::new(),
            available_at: None,
            ledger_status: String::new(),
            source: "ai_gateway".to_string(),
            facilitator: String::new(),
            network: String::new(),
            asset: String::new(),
            gross_revenue: 0.0,
            alephant_fee: 0.0,
            net_revenue: 0.0,
            ai_cost: 0.0,
            tx_hash: String::new(),
            platform_receive_wallet_address: String::new(),
            seller_receive_wallet_address: String::new(),
            fee_wallet_address: String::new(),
            buyer_wallet: String::new(),
            fund_status: String::new(),
            trace_status: String::new(),
            agent_session_id: String::new(),
            failure_reason: String::new(),
            created_at: now,
            verify_time: None,
        }
    }
}

#[must_use]
pub fn hash_sensitive(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

pub async fn write_x402_log(
    app_state: &AppState,
    message: &X402PaymentLogMessage,
) -> Result<(), LoggerError> {
    let payload = serde_json::to_string(message)?;
    let Some(redis) = app_state.redis() else {
        tracing::warn!(
            "x402 payment log Redis unavailable; using HTTP fallback"
        );
        return send_x402_log_http(app_state, payload).await;
    };

    let stream_key = &app_state.config().x402.log_stream_key;
    if let Err(error) = redis.xadd_payload(stream_key, &payload).await {
        tracing::error!(
            stream_key = %stream_key,
            error = %error,
            "x402 payment log Redis Stream write failed; using HTTP fallback"
        );
        return send_x402_log_http(app_state, payload).await;
    }

    tracing::info!(
        stream_key = %stream_key,
        activity_id = %message.activity_id,
        trace_id = %message.trace_id,
        service_status = %message.service_status,
        "x402 payment log written to Redis Stream",
    );
    Ok(())
}

fn x402_payment_log_url(log_collector_url: &Url) -> Result<Url, LoggerError> {
    Ok(log_collector_url.join(X402_PAYMENT_LOG_HTTP_PATH)?)
}

fn x402_payment_log_auth_value(token: &Secret<String>) -> Option<String> {
    let token = token.expose().trim();
    if token.is_empty() {
        None
    } else {
        Some(format!("Bearer {token}"))
    }
}

fn debug_env_flag_value_enabled(value: &str) -> bool {
    value.eq_ignore_ascii_case("true")
}

fn debug_env_flag_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| debug_env_flag_value_enabled(&value))
}

fn x402_payment_log_debug_header_lines(headers: &header::HeaderMap) -> String {
    let mut lines = Vec::new();
    for (name, value) in headers {
        lines.push(format!(
            "{}: {}",
            name.as_str(),
            value.to_str().unwrap_or("<non-utf8>")
        ));
    }
    lines.join("\n")
}

fn log_x402_payment_log_http_request(
    url: &Url,
    headers: &header::HeaderMap,
    payload: &str,
) {
    if debug_env_flag_enabled(DEBUG_HEADERS_ENV) {
        let headers = x402_payment_log_debug_header_lines(headers);
        tracing::info!(
            url = %url,
            headers = %headers,
            "x402 payment log HTTP request headers ({DEBUG_HEADERS_ENV})",
        );
    }
    if debug_env_flag_enabled(DEBUG_BODY_ENV) {
        tracing::info!(
            url = %url,
            body_byte_len = payload.len(),
            body = %payload,
            "x402 payment log HTTP request body ({DEBUG_BODY_ENV})",
        );
    }
}

async fn send_x402_log_http(
    app_state: &AppState,
    payload: String,
) -> Result<(), LoggerError> {
    let log_url =
        x402_payment_log_url(&app_state.config().alephant.log_collector_url)?;
    tracing::trace!(
        body_byte_len = payload.len(),
        "[x402 payment log http] sending log POST"
    );
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    if let Some(auth_value) = x402_payment_log_auth_value(
        &app_state
            .config()
            .alephant
            .logs_collector_x402_http_auth_token,
    ) {
        let auth_value =
            header::HeaderValue::from_str(&auth_value).map_err(|error| {
                LoggerError::UnexpectedResponse(format!(
                    "invalid x402 payment log auth header: {error}"
                ))
            })?;
        headers.insert(header::AUTHORIZATION, auth_value);
    }
    log_x402_payment_log_http_request(&log_url, &headers, &payload);

    let request = app_state
        .0
        .alephant_http_client
        .request_client
        .post(log_url)
        .headers(headers)
        .body(payload);

    let response = request.send().await.map_err(|error| {
        tracing::debug!(
            error = %error,
            "failed to send x402 payment log to alephant logger"
        );
        LoggerError::FailedToSendRequest(error)
    })?;

    let status_err = response.error_for_status_ref().err();
    let _body = response.text().await.unwrap_or_default();
    if let Some(error) = status_err {
        tracing::error!(
            error = %error,
            "failed to log x402 payment to alephant"
        );
        return Err(LoggerError::ResponseError(error));
    }

    tracing::info!("x402 payment log written via HTTP fallback");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::oneshot,
    };
    use tracing::{Event, Subscriber, field::Visit};
    use tracing_subscriber::{Layer, layer::SubscriberExt};

    use super::*;

    #[test]
    fn hash_sensitive_returns_stable_sha256_hex_without_raw_value() {
        let raw_value = "raw-payment-signature";

        let first = hash_sensitive(raw_value);
        let second = hash_sensitive(raw_value);

        assert_eq!(first.len(), 64);
        assert_eq!(first, second);
        assert_ne!(first, raw_value);
    }

    #[test]
    fn payment_log_serialization_omits_payment_signature_hash() {
        let raw_signature = "raw-payment-signature";
        let mut message =
            X402PaymentLogMessage::new(X402LogStage::VerifyFailed);
        message.failure_reason = raw_signature.to_string();

        let json = serde_json::to_string(&message).unwrap();

        assert!(json.contains("00000000-0000-0000-0000-000000000000"));
        assert!(!json.contains("payment_signature_hash"));
    }

    #[test]
    fn fresh_payment_log_serialization_matches_clickhouse_defaults() {
        let message = X402PaymentLogMessage::new(X402LogStage::PaymentRequired);

        let json = serde_json::to_string(&message).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(
            value["workspace_id"],
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            value["activity_id"],
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(
            value["endpoint_id"],
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(value["agent_id"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(value["direction"], "inbound");
        assert_eq!(value["source"], "ai_gateway");
        assert_eq!(value["gross_revenue"], 0.0);
        assert_eq!(value["alephant_fee"], 0.0);
        assert_eq!(value["net_revenue"], 0.0);
        assert_eq!(value["ai_cost"], 0.0);
        assert!(value["settled_at"].is_null());
        assert!(value["available_at"].is_null());
        assert!(value["verify_time"].is_null());
        assert!(value.get("trace").is_none());
        assert!(value.get("payment").is_none());
        assert!(value.get("upstream").is_none());

        let object = value.as_object().unwrap();
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        let mut expected = vec![
            "activity_id",
            "agent_id",
            "agent_session_id",
            "ai_cost",
            "alephant_fee",
            "asset",
            "available_at",
            "buyer_wallet",
            "created_at",
            "direction",
            "endpoint_id",
            "event_time",
            "facilitator",
            "failure_reason",
            "fee_wallet_address",
            "fund_status",
            "gross_revenue",
            "ledger_status",
            "net_revenue",
            "network",
            "payment_status",
            "platform_receive_wallet_address",
            "seller_receive_wallet_address",
            "service_status",
            "settled_at",
            "settlement_status",
            "source",
            "trace_id",
            "trace_status",
            "tx_hash",
            "verify_time",
            "workspace_id",
        ];
        expected.sort_unstable();
        assert_eq!(keys, expected);
    }

    #[test]
    fn x402_payment_log_http_url_uses_downstream_route() {
        let base = "http://logger.local/base".parse().unwrap();

        let url = x402_payment_log_url(&base).unwrap();

        assert_eq!(url.as_str(), "http://logger.local/v1/log/x402-payment");
    }

    #[test]
    fn x402_payment_log_http_auth_value_uses_bearer_token() {
        let token =
            crate::types::secret::Secret::from("x402-token".to_string());

        let value = x402_payment_log_auth_value(&token)
            .expect("auth value should be present");

        assert_eq!(value, "Bearer x402-token");
    }

    #[test]
    fn x402_payment_log_http_auth_value_skips_empty_token() {
        let token = crate::types::secret::Secret::from("   ".to_string());

        assert!(x402_payment_log_auth_value(&token).is_none());
    }

    #[tokio::test]
    async fn write_x402_log_http_fallback_sends_bearer_auth() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind log collector");
        let addr = listener.local_addr().expect("log collector addr");
        let (request_tx, request_rx) = oneshot::channel::<String>();

        tokio::spawn(async move {
            let (mut stream, _) =
                listener.accept().await.expect("accept log request");
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).await.expect("read request");
            let raw = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = request_tx.send(raw);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("write response");
        });

        let mut config = crate::config::Config::default();
        config.alephant.log_collector_url =
            format!("http://{addr}").parse().expect("collector url");
        config.alephant.logs_collector_x402_http_auth_token =
            Secret::from("collector-token".to_string());
        let app = crate::app::build_test_app(config).await.expect("test app");

        let message = X402PaymentLogMessage::new(X402LogStage::GatewayError);
        write_x402_log(&app.state, &message)
            .await
            .expect("http fallback log write");

        let raw_request = request_rx
            .await
            .expect("log collector should receive request");
        let lower_request = raw_request.to_ascii_lowercase();
        assert!(raw_request.starts_with("POST /v1/log/x402-payment "));
        assert!(
            lower_request.contains("authorization: bearer collector-token\r\n")
        );
    }

    #[test]
    fn x402_payment_log_http_request_debug_logs_headers_and_body_when_enabled()
    {
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();
        let _env_guard = EnvGuard::set_many(&[
            ("AI_GATEWAY_DEBUG_HEADERS", "true"),
            ("AI_GATEWAY_DEBUG_BODY", "true"),
        ]);
        let events = Arc::new(Mutex::new(Vec::new()));
        let subscriber =
            tracing_subscriber::registry().with(CaptureLayer(events.clone()));
        let url = "http://logger.local/v1/log/x402-payment".parse().unwrap();
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer collector-token"),
        );
        let payload = r#"{"activity_id":"activity-123"}"#;

        tracing::subscriber::with_default(subscriber, || {
            log_x402_payment_log_http_request(&url, &headers, payload);
        });

        let joined = events.lock().unwrap().join("\n").to_ascii_lowercase();
        assert!(joined.contains("x402 payment log http request headers"));
        assert!(joined.contains("x402 payment log http request body"));
        assert!(joined.contains("/v1/log/x402-payment"));
        assert!(joined.contains("authorization"));
        assert!(joined.contains("bearer collector-token"));
        assert!(!joined.contains("*****"));
        assert!(joined.contains("activity-123"));
    }

    #[test]
    fn x402_payment_log_debug_header_lines_prints_authorization_plaintext() {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer collector-token"),
        );

        let lines = x402_payment_log_debug_header_lines(&headers);

        assert_eq!(lines, "authorization: Bearer collector-token");
        assert!(!lines.contains("*****"));
    }

    struct CaptureLayer(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber,
    {
        fn on_event(
            &self,
            event: &Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = StringVisitor(String::new());
            event.record(&mut visitor);
            self.0.lock().unwrap().push(visitor.0);
        }
    }

    struct StringVisitor(String);

    impl Visit for StringVisitor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push_str(&format!("{}={value};", field.name()));
        }

        fn record_debug(
            &mut self,
            field: &tracing::field::Field,
            value: &dyn std::fmt::Debug,
        ) {
            self.0.push_str(&format!("{}={value:?};", field.name()));
        }
    }

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set_many(values: &[(&'static str, &'static str)]) -> Self {
            let previous = values
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for (key, value) in values {
                unsafe { std::env::set_var(key, value) };
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..) {
                match value {
                    Some(value) => unsafe { std::env::set_var(key, value) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }
}
