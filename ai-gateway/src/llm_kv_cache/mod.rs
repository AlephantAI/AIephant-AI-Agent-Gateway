//! LLM response KV cache with bucketed Alephant header semantics.

use std::sync::Arc;

use alephant_llm_kv_cache::LlmKvBackend;

use crate::{config::Config, error::init::InitError};

/// Builds the process-wide KV backend for the active `external` / `internal`
/// feature.
#[allow(clippy::unused_async)] // `external` branch has no `.await`; signature stays unified.
pub async fn build_llm_kv_backend(
    config: &Config,
) -> Result<Arc<dyn LlmKvBackend + Send + Sync>, InitError> {
    #[cfg(feature = "external")]
    {
        use alephant_llm_kv_cache::cloudflare::CloudflareKvClient;

        let cf = config.cloudflare_kv.as_ref().ok_or_else(|| {
            InitError::StoreNotConfigured(
                "cloudflare_kv (required for --features external)",
            )
        })?;
        if cf.api_base.trim().is_empty()
            || cf.account_id.is_empty()
            || cf.namespace_id.is_empty()
        {
            return Err(InitError::InvalidBalancer(
                "cloudflare_kv.api_base, account_id, namespace_id must be \
                 non-empty"
                    .into(),
            ));
        }
        let token = cf.api_token.expose();
        if token.trim().is_empty() {
            return Err(InitError::InvalidBalancer(
                "cloudflare_kv.api_token must be non-empty".into(),
            ));
        }
        let client = CloudflareKvClient {
            http: reqwest::Client::new(),
            api_base: cf.api_base.clone(),
            account_id: cf.account_id.clone(),
            namespace_id: cf.namespace_id.clone(),
            api_token: token.clone(),
        };
        Ok(Arc::new(
            alephant_llm_kv_cache::LazyCloudflareKvBackend::new(client),
        ))
    }
    #[cfg(all(feature = "internal", not(feature = "external")))]
    {
        match &config.tikv_kv {
            None => Ok(Arc::new(alephant_llm_kv_cache::InternalStubBackend)),
            Some(cfg) => {
                if cfg.pd_endpoints.is_empty() {
                    return Err(InitError::InvalidBalancer(
                        "tikv_kv.pd_endpoints must be non-empty when tikv_kv \
                         is set"
                            .into(),
                    ));
                }
                Ok(Arc::new(alephant_llm_kv_cache::LazyTikvBackend::new(
                    cfg.pd_endpoints.clone(),
                    cfg.max_value_bytes,
                    cfg.request_timeout_ms,
                    cfg.ca_cert_path.clone(),
                )))
            }
        }
    }
}
