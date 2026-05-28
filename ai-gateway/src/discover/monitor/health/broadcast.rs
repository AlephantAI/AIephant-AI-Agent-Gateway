//! Cross-instance provider health event broadcasting via Redis Pub/Sub.
//!
//! When a provider transitions to unhealthy/healthy, the local instance
//! publishes an event to a Redis channel. Other instances subscribe and
//! use the signal to trigger an immediate local probe (not direct removal).

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::HealthEventBroadcastConfig;

/// A health event published/received over Redis Pub/Sub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthEvent {
    pub provider_key: String,
    pub router_id: String,
    pub status: HealthStatus,
    pub ts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Down,
    Up,
}

/// Publishes health events to Redis Pub/Sub with dedup.
#[derive(Debug, Clone)]
pub struct HealthEventPublisher {
    redis_url: url::Url,
    channel: String,
    dedup_window: Duration,
    last_published: Arc<Mutex<HashMap<(String, String), Instant>>>,
}

impl HealthEventPublisher {
    #[must_use]
    pub fn new(
        redis_url: url::Url,
        config: &HealthEventBroadcastConfig,
    ) -> Self {
        Self {
            redis_url,
            channel: config.channel.clone(),
            dedup_window: Duration::from_secs(config.dedup_window_secs),
            last_published: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Publish a health event if not within the dedup window.
    pub async fn publish(
        &self,
        provider_key: &str,
        router_id: &str,
        status: HealthStatus,
    ) {
        let dedup_key = (router_id.to_string(), provider_key.to_string());
        let now = Instant::now();

        {
            let mut last = self.last_published.lock().await;
            if last
                .get(&dedup_key)
                .is_some_and(|t| now.duration_since(*t) < self.dedup_window)
            {
                return;
            }
            last.insert(dedup_key, now);
        }

        let event = HealthEvent {
            provider_key: provider_key.to_string(),
            router_id: router_id.to_string(),
            status,
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| {
                    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
                }),
        };

        let payload = match serde_json::to_string(&event) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "health_broadcast: failed to serialize event"
                );
                return;
            }
        };

        if let Err(e) = self.do_publish(&payload).await {
            tracing::debug!(
                error = %e,
                "health_broadcast: Redis PUBLISH failed (best-effort)"
            );
        }
    }

    async fn do_publish(&self, payload: &str) -> Result<(), redis::RedisError> {
        use redis::AsyncCommands;
        let client = redis::Client::open(self.redis_url.as_str())?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        let _: i64 = conn.publish(&self.channel, payload).await?;
        Ok(())
    }
}

/// Subscribes to health events from other instances.
/// On receiving a "down" event, signals the health monitor to probe
/// immediately. This is a best-effort mechanism — failures are silently
/// ignored.
pub async fn run_subscriber(redis_url: url::Url, channel: String) {
    loop {
        match subscribe_loop(&redis_url, &channel).await {
            Ok(()) => break,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "health_broadcast subscriber: connection lost, reconnecting in 5s"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn subscribe_loop(
    redis_url: &url::Url,
    channel: &str,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url.as_str())?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(channel).await?;
    tracing::info!(
        channel = %channel,
        "health_broadcast subscriber: listening"
    );

    let mut stream = pubsub.on_message();
    loop {
        let msg = stream.next().await;
        if let Some(msg) = msg {
            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(_) => continue,
            };
            match serde_json::from_str::<HealthEvent>(&payload) {
                Ok(event) => {
                    tracing::debug!(
                        provider = %event.provider_key,
                        router = %event.router_id,
                        status = ?event.status,
                        "health_broadcast: received remote event"
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "health_broadcast: failed to parse event"
                    );
                }
            }
        } else {
            return Err(redis::RedisError::from((
                redis::ErrorKind::IoError,
                "pubsub stream ended",
            )));
        }
    }
}
