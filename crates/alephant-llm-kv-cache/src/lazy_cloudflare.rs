//! Lazy Cloudflare KV: backoff between attempts without background tasks (spec
//! 2026-04-21).

use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    backend::LlmKvBackend,
    backoff::{
        DEFAULT_BACKOFF_BASE_MS, DEFAULT_BACKOFF_CAP_MS,
        DEFAULT_BACKOFF_MAX_SHIFT, next_delay_ms,
    },
    cloudflare::{CfKvErrorKind, CloudflareKvClient},
    put_retry::PutClassifiedError,
};

const TERMINAL_CF_COOLDOWN: Duration = Duration::from_secs(60);

/// Wraps [`CloudflareKvClient`] with on-demand backoff when CF is unreachable.
pub struct LazyCloudflareKvBackend {
    inner: CloudflareKvClient,
    gate: Mutex<(Instant, u32)>,
}

impl LazyCloudflareKvBackend {
    pub fn new(inner: CloudflareKvClient) -> Self {
        Self {
            inner,
            gate: Mutex::new((Instant::now(), 0)),
        }
    }

    fn register_transient_failure(gate: &mut (Instant, u32)) {
        gate.1 = gate.1.saturating_add(1);
        let delay_ms = next_delay_ms(
            gate.1,
            DEFAULT_BACKOFF_BASE_MS,
            DEFAULT_BACKOFF_CAP_MS,
            DEFAULT_BACKOFF_MAX_SHIFT,
        );
        gate.0 = Instant::now() + Duration::from_millis(delay_ms);
    }

    fn register_terminal_failure(gate: &mut (Instant, u32)) {
        gate.1 = gate.1.saturating_add(1);
        gate.0 = Instant::now() + TERMINAL_CF_COOLDOWN;
    }

    fn register_success(gate: &mut (Instant, u32)) {
        gate.0 = Instant::now();
        gate.1 = 0;
    }
}

#[async_trait]
impl LlmKvBackend for LazyCloudflareKvBackend {
    async fn get(
        &self,
        key: &str,
    ) -> Result<Option<String>, crate::error::LlmKvCacheError> {
        {
            let g = self.gate.lock().await;
            if Instant::now() < g.0 {
                return Ok(None);
            }
        }

        match self.inner.get_key_classified(key).await {
            Ok(v) => {
                let mut g = self.gate.lock().await;
                let recovered = g.1 > 0;
                Self::register_success(&mut g);
                if recovered {
                    tracing::info!(
                        target: "alephant_llm_kv",
                        "cloudflare lazy: GET recovered after backoff"
                    );
                }
                Ok(v)
            }
            Err(CfKvErrorKind::Transient { status, message }) => {
                let mut g = self.gate.lock().await;
                Self::register_transient_failure(&mut g);
                let retry_after_ms =
                    g.0.checked_duration_since(Instant::now())
                        .unwrap_or_default()
                        .as_millis() as u64;
                tracing::warn!(
                    target: "alephant_llm_kv",
                    failures = g.1,
                    retry_after_ms,
                    ?status,
                    error = %message,
                    "cloudflare lazy: GET transient failure"
                );
                Ok(None)
            }
            Err(CfKvErrorKind::Terminal { status, message }) => {
                let mut g = self.gate.lock().await;
                Self::register_terminal_failure(&mut g);
                tracing::warn!(
                    target: "alephant_llm_kv",
                    failures = g.1,
                    status,
                    error = %message,
                    "cloudflare lazy: GET terminal failure"
                );
                Ok(None)
            }
        }
    }

    async fn put(
        &self,
        key: &str,
        value: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError> {
        {
            let g = self.gate.lock().await;
            if Instant::now() < g.0 {
                return Ok(());
            }
        }

        match self
            .inner
            .put_raw_classified(key, value, expiration_ttl_secs)
            .await
        {
            Ok(()) => {
                let mut g = self.gate.lock().await;
                let recovered = g.1 > 0;
                Self::register_success(&mut g);
                if recovered {
                    tracing::info!(
                        target: "alephant_llm_kv",
                        "cloudflare lazy: PUT recovered after backoff"
                    );
                }
                Ok(())
            }
            Err(PutClassifiedError::TransientExhausted(message)) => {
                let mut g = self.gate.lock().await;
                Self::register_transient_failure(&mut g);
                let retry_after_ms =
                    g.0.checked_duration_since(Instant::now())
                        .unwrap_or_default()
                        .as_millis() as u64;
                tracing::warn!(
                    target: "alephant_llm_kv",
                    failures = g.1,
                    retry_after_ms,
                    error = %message,
                    "cloudflare lazy: PUT transient exhausted"
                );
                Ok(())
            }
            Err(PutClassifiedError::Terminal(message)) => {
                let mut g = self.gate.lock().await;
                Self::register_terminal_failure(&mut g);
                tracing::warn!(
                    target: "alephant_llm_kv",
                    failures = g.1,
                    error = %message,
                    "cloudflare lazy: PUT terminal failure"
                );
                Ok(())
            }
        }
    }
}
