use async_trait::async_trait;
use reqwest::Client;
use url::Url;

use super::LogTransport;
use crate::{error::logger::LoggerError, metrics::Metrics, types::logger::LogMessage};

#[derive(Clone, Debug)]
pub struct HttpLogTransport {
    client: Client,
    log_collector_url: Url,
    metrics: Metrics,
}

impl HttpLogTransport {
    #[must_use]
    pub fn new(client: Client, log_collector_url: Url, metrics: Metrics) -> Self {
        Self {
            client,
            log_collector_url,
            metrics,
        }
    }

    /// `record_send_counter`: when `false`, skips the successful-delivery
    /// `http` send counter (used when this POST is already counted as
    /// `redis_degraded_to_http`).
    pub(crate) async fn send_with_counter(
        &self,
        log_message: &LogMessage,
        record_send_counter: bool,
    ) -> Result<(), LoggerError> {
        let log_url = self.log_collector_url.join("/v1/log/request")?;
        if record_send_counter {
            self.metrics
                .ingest_log_sends
                .add(1, &[opentelemetry::KeyValue::new("transport_kind", "http")]);
        }
        let body = serde_json::to_string(log_message)?;
        tracing::trace!(
            body_byte_len = body.len(),
            "[request log transport http] sending log POST"
        );
        let log_response = self
            .client
            .post(log_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .header(
                "authorization",
                format!("Bearer {}", log_message.authorization),
            )
            .send()
            .await
            .map_err(|e| {
                tracing::debug!(error = %e, "failed to send request to alephant logger");
                self.metrics
                    .ingest_log_errors
                    .add(1, &[opentelemetry::KeyValue::new("transport_kind", "http")]);
                LoggerError::FailedToSendRequest(e)
            })?;

        let status_err = log_response.error_for_status_ref().err();
        let _body = log_response.text().await.unwrap_or_default();
        if let Some(e) = status_err {
            tracing::error!(error = %e, "failed to log request to alephant");
            self.metrics
                .ingest_log_errors
                .add(1, &[opentelemetry::KeyValue::new("transport_kind", "http")]);
            return Err(LoggerError::ResponseError(e));
        }
        Ok(())
    }
}

#[async_trait]
impl LogTransport for HttpLogTransport {
    async fn send(&self, log_message: &LogMessage) -> Result<(), LoggerError> {
        self.send_with_counter(log_message, true).await
    }
}
