//! Lazy TiKV connect: no connect until `get`/`put` (spec 2026-04-21).

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use crate::{
    TikvKvClient,
    backend::LlmKvBackend,
    backoff::{
        DEFAULT_BACKOFF_BASE_MS, DEFAULT_BACKOFF_CAP_MS, DEFAULT_BACKOFF_MAX_SHIFT, next_delay_ms,
    },
};

/// TiKV backend that connects to PD on first `get`/`put` after each backoff
/// window.
pub struct LazyTikvBackend {
    pd_endpoints: Vec<String>,
    max_value_bytes: usize,
    request_timeout_ms: u64,
    ca_cert_path: Option<PathBuf>,
    inner: RwLock<Option<Arc<TikvKvClient>>>,
    connect_lock: Mutex<()>,
    gate: Mutex<(Instant, u32)>,
}

impl LazyTikvBackend {
    pub fn new(
        pd_endpoints: Vec<String>,
        max_value_bytes: usize,
        request_timeout_ms: u64,
        ca_cert_path: Option<PathBuf>,
    ) -> Self {
        Self {
            pd_endpoints,
            max_value_bytes,
            request_timeout_ms,
            ca_cert_path,
            inner: RwLock::new(None),
            connect_lock: Mutex::new(()),
            gate: Mutex::new((Instant::now(), 0)),
        }
    }

    async fn ensure_client(&self) -> Option<Arc<TikvKvClient>> {
        if let Some(c) = self.inner.read().await.as_ref() {
            return Some(Arc::clone(c));
        }
        {
            let (next_retry, _) = *self.gate.lock().await;
            if Instant::now() < next_retry {
                return None;
            }
        }
        let _g = self.connect_lock.lock().await;
        if let Some(c) = self.inner.read().await.as_ref() {
            return Some(Arc::clone(c));
        }
        {
            let (next_retry, _) = *self.gate.lock().await;
            if Instant::now() < next_retry {
                return None;
            }
        }
        let ca = self.ca_cert_path.as_deref();
        match TikvKvClient::connect(
            self.pd_endpoints.clone(),
            self.max_value_bytes,
            self.request_timeout_ms,
            ca,
        )
        .await
        {
            Ok(client) => {
                let arc = Arc::new(client);
                *self.inner.write().await = Some(Arc::clone(&arc));
                let mut g = self.gate.lock().await;
                g.0 = Instant::now();
                g.1 = 0;
                tracing::info!(target: "alephant_llm_kv", "tikv lazy: connected to PD");
                Some(arc)
            }
            Err(e) => {
                let mut g = self.gate.lock().await;
                g.1 = g.1.saturating_add(1);
                let delay_ms = next_delay_ms(
                    g.1,
                    DEFAULT_BACKOFF_BASE_MS,
                    DEFAULT_BACKOFF_CAP_MS,
                    DEFAULT_BACKOFF_MAX_SHIFT,
                );
                g.0 = Instant::now() + Duration::from_millis(delay_ms);
                tracing::warn!(
                    target: "alephant_llm_kv",
                    failures = g.1,
                    retry_after_ms = delay_ms,
                    error = %e,
                    "tikv lazy: connect failed"
                );
                None
            }
        }
    }
}

#[async_trait]
impl LlmKvBackend for LazyTikvBackend {
    async fn get(&self, key: &str) -> Result<Option<String>, crate::error::LlmKvCacheError> {
        let Some(client) = self.ensure_client().await else {
            return Ok(None);
        };
        client.get(key).await
    }

    async fn put(
        &self,
        key: &str,
        value: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError> {
        let Some(client) = self.ensure_client().await else {
            return Ok(());
        };
        client.put(key, value, expiration_ttl_secs).await
    }
}
