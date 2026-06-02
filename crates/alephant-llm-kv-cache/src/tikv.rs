//! TiKV Raw KV backend for LLM response caching (`internal` feature).

use std::{path::Path, time::Duration};

use async_trait::async_trait;
use tikv_client::{Config as TikvConfig, RawClient};

use crate::{backend::LlmKvBackend, tikv_util};

/// TiKV-backed [`LlmKvBackend`] using PD endpoints and raw get/put.
pub struct TikvKvClient {
    raw: RawClient,
    max_value_bytes: usize,
}

impl TikvKvClient {
    /// Connects to TiKV via PD. Uses TLS CA file when `ca_cert_path` is set.
    pub async fn connect(
        pd_endpoints: Vec<String>,
        max_value_bytes: usize,
        request_timeout_ms: u64,
        ca_cert_path: Option<&Path>,
    ) -> Result<Self, crate::error::LlmKvCacheError> {
        if pd_endpoints.is_empty() {
            return Err(crate::error::LlmKvCacheError::Config(
                "tikv_kv.pd_endpoints must be non-empty".into(),
            ));
        }

        let mut config = TikvConfig::default()
            .with_timeout(Duration::from_millis(request_timeout_ms))
            .with_grpc_max_decoding_message_size(max_value_bytes.saturating_add(1024 * 1024));
        if let Some(ca) = ca_cert_path {
            config.ca_path = Some(ca.to_path_buf());
        }

        let raw = RawClient::new_with_config(pd_endpoints, config)
            .await
            .map_err(|e| crate::error::LlmKvCacheError::Tikv(e.to_string()))?;

        Ok(Self {
            raw,
            max_value_bytes,
        })
    }
}

#[async_trait]
impl LlmKvBackend for TikvKvClient {
    async fn get(&self, key: &str) -> Result<Option<String>, crate::error::LlmKvCacheError> {
        tracing::info!("[TiKV KV get] key={key}");
        let k = key.to_string();
        let v = self
            .raw
            .get(k)
            .await
            .map_err(|e| crate::error::LlmKvCacheError::Tikv(e.to_string()))?;
        match &v {
            None => tracing::info!("[TiKV KV get] key={key} result=miss"),
            Some(bytes) => tracing::info!(
                "[TiKV KV get] key={key} result=hit value_len={}",
                bytes.len()
            ),
        }
        Ok(v.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    async fn put(
        &self,
        key: &str,
        value: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError> {
        if tikv_util::value_exceeds_limit(value.len(), self.max_value_bytes) {
            tracing::info!(
                "[TiKV KV put] skip: value too large key={key} len={} max={}",
                value.len(),
                self.max_value_bytes
            );
            tracing::warn!(
                len = value.len(),
                max = self.max_value_bytes,
                "skipping LLM KV put: value too large for TiKV backend"
            );
            return Ok(());
        }

        let k = key.to_string();
        let ttl = tikv_util::normalized_ttl_secs(expiration_ttl_secs);
        let v = value.to_string();
        tracing::info!("[TiKV KV put] key={key} ttl={ttl} value_len={}", v.len());

        match self.raw.put_with_ttl(k.clone(), v.clone(), ttl).await {
            Ok(()) => {
                tracing::info!("[TiKV KV put] key={key} ok (with_ttl)");
                Ok(())
            }
            Err(e) => {
                tracing::info!(
                    "[TiKV KV put] put_with_ttl failed, retry plain put \
                     key={key} err={e}"
                );
                tracing::warn!(
                    error = %e,
                    "llm kv tikv: put_with_ttl failed, retrying put without TTL"
                );
                self.raw
                    .put(k, v)
                    .await
                    .map_err(|e2| crate::error::LlmKvCacheError::Tikv(e2.to_string()))?;
                tracing::info!("[TiKV KV put] key={key} ok (plain put)");
                Ok(())
            }
        }
    }
}
