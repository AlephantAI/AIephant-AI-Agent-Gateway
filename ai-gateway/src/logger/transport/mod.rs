mod http_impl;
mod redis_impl;

use std::sync::Arc;

use async_trait::async_trait;
pub use http_impl::HttpLogTransport;
pub use redis_impl::RedisStreamLogTransport;

use crate::{
    config::{Config, request_log::RequestLogTransport},
    error::logger::LoggerError,
    logger::service::AlephantHttpClient,
    metrics::Metrics,
    types::logger::LogMessage,
};

#[async_trait]
pub trait LogTransport: Send + Sync + std::fmt::Debug {
    async fn send(&self, log_message: &LogMessage) -> Result<(), LoggerError>;
}

#[must_use]
pub fn build_request_log_transport(
    config: &Config,
    http_client: &AlephantHttpClient,
    metrics: &Metrics,
    redis: Option<std::sync::Arc<crate::app_redis::AppRedis>>,
) -> Arc<dyn LogTransport> {
    let http = HttpLogTransport::new(
        http_client.request_client.clone(),
        config.alephant.log_collector_url.clone(),
        metrics.clone(),
    );
    match config.request_log.transport {
        RequestLogTransport::Http => Arc::new(http),
        RequestLogTransport::Redis => Arc::new(RedisStreamLogTransport::new(
            redis,
            config.request_log.request_response_stream_key.clone(),
            http,
            metrics.clone(),
        )),
    }
}
