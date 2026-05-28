use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;

use super::{HttpLogTransport, LogTransport};
use crate::{
    app_redis::AppRedis, error::logger::LoggerError, metrics::Metrics,
    types::logger::LogMessage,
};

/// Write request logs to Redis Stream (`XADD`, single `payload` field); cold
/// path falls back to HTTP.
#[derive(Debug)]
pub struct RedisStreamLogTransport {
    stream_key: String,
    /// `None` when `log_queue_redis_url` is unset; always use HTTP fallback.
    redis: Option<Arc<AppRedis>>,
    http_fallback: HttpLogTransport,
    metrics: Metrics,
    degraded: AtomicBool,
    warned_degrade: AtomicBool,
}

impl RedisStreamLogTransport {
    #[must_use]
    pub fn new(
        redis: Option<Arc<AppRedis>>,
        stream_key: String,
        http_fallback: HttpLogTransport,
        metrics: Metrics,
    ) -> Self {
        Self {
            stream_key,
            redis,
            http_fallback,
            metrics,
            degraded: AtomicBool::new(false),
            warned_degrade: AtomicBool::new(false),
        }
    }

    fn warn_degrade_once(
        &self,
        reason: &'static str,
        detail: impl std::fmt::Display,
    ) {
        if self
            .warned_degrade
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tracing::warn!(
                reason,
                detail = %detail,
                "request log: Redis queue unavailable; using HTTP for this process",
            );
        }
    }

    async fn send_degraded_http(
        &self,
        log_message: &LogMessage,
    ) -> Result<(), LoggerError> {
        self.metrics.ingest_log_sends.add(
            1,
            &[opentelemetry::KeyValue::new(
                "transport_kind",
                "redis_degraded_to_http",
            )],
        );
        self.http_fallback
            .send_with_counter(log_message, false)
            .await
    }
}

#[async_trait]
impl LogTransport for RedisStreamLogTransport {
    async fn send(&self, log_message: &LogMessage) -> Result<(), LoggerError> {
        if self.degraded.load(Ordering::SeqCst) {
            return self.send_degraded_http(log_message).await;
        }

        let Some(client) = self.redis.as_ref() else {
            self.warn_degrade_once("log_queue_redis_url empty", "missing URL");
            self.degraded.store(true, Ordering::SeqCst);
            return self.send_degraded_http(log_message).await;
        };

        if let Err(e) = client.ping().await {
            self.warn_degrade_once("redis connect failed", &e);
            self.degraded.store(true, Ordering::SeqCst);
            return self.send_degraded_http(log_message).await;
        }

        let payload = serde_json::to_string(log_message)?;
        match client.xadd_payload(&self.stream_key, &payload).await {
            Ok(()) => {
                self.metrics.ingest_log_sends.add(
                    1,
                    &[opentelemetry::KeyValue::new("transport_kind", "redis")],
                );
                Ok(())
            }
            Err(e) => {
                tracing::error!(error = %e, "request log: XADD failed");
                self.metrics.ingest_log_errors.add(
                    1,
                    &[opentelemetry::KeyValue::new("transport_kind", "redis")],
                );
                Err(LoggerError::RedisLogQueue(e))
            }
        }
    }
}
