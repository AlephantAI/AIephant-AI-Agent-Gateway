use async_trait::async_trait;
use reqwest::Client;

use crate::{
    backend::LlmKvBackend,
    put_retry::{
        PutClassifiedError, put_with_backoff, put_with_backoff_classified,
    },
};

const MAX_KV_VALUE_BYTES: usize = 25 * 1024 * 1024;

/// Cloudflare KV enforces a minimum TTL of 60 seconds.
const MIN_CF_TTL_SECS: u64 = 60;

/// Classified GET error for lazy backoff (spec 2026-04-21 §4.4).
#[derive(Debug)]
pub enum CfKvErrorKind {
    Transient {
        status: Option<u16>,
        message: String,
    },
    Terminal {
        status: u16,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct CloudflareKvClient {
    pub http: Client,
    pub api_base: String,
    pub account_id: String,
    pub namespace_id: String,
    pub api_token: String,
}

impl CloudflareKvClient {
    fn values_url(&self, key: &str) -> String {
        let base = self.api_base.trim_end_matches('/');
        format!(
            "{base}/accounts/{}/storage/kv/namespaces/{}/values/{key}",
            self.account_id, self.namespace_id
        )
    }

    pub async fn get_key_classified(
        &self,
        key: &str,
    ) -> Result<Option<String>, CfKvErrorKind> {
        let url = self.values_url(key);
        let res = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await
            .map_err(|e| CfKvErrorKind::Transient {
                status: None,
                message: e.to_string(),
            })?;
        let status = res.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status.is_success() {
            let body =
                res.text().await.map_err(|e| CfKvErrorKind::Transient {
                    status: Some(status.as_u16()),
                    message: e.to_string(),
                })?;
            return Ok(Some(body));
        }
        let code = status.as_u16();
        let msg = format!("kv get: {status}");
        if status.is_server_error() || code == 429 {
            return Err(CfKvErrorKind::Transient {
                status: Some(code),
                message: msg,
            });
        }
        if code >= 400 {
            return Err(CfKvErrorKind::Terminal {
                status: code,
                message: msg,
            });
        }
        Err(CfKvErrorKind::Transient {
            status: Some(code),
            message: msg,
        })
    }

    pub async fn get_raw(
        &self,
        key: &str,
    ) -> Result<Option<String>, crate::error::LlmKvCacheError> {
        self.get_key_classified(key).await.map_err(|e| match e {
            CfKvErrorKind::Transient { message, .. }
            | CfKvErrorKind::Terminal { message, .. } => {
                crate::error::LlmKvCacheError::Http(message)
            }
        })
    }

    /// Writes JSON value; skips silently when over Cloudflare KV size limit.
    pub async fn put_raw(
        &self,
        key: &str,
        body: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError> {
        if body.len() > MAX_KV_VALUE_BYTES {
            tracing::warn!(
                len = body.len(),
                max = MAX_KV_VALUE_BYTES,
                "skipping LLM KV put: value too large"
            );
            return Ok(());
        }
        let ttl = cf_clamp_ttl(expiration_ttl_secs);
        let url = self.values_url(key);
        put_with_backoff(&self.http, &url, &self.api_token, body, ttl).await
    }

    pub async fn put_raw_classified(
        &self,
        key: &str,
        body: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), PutClassifiedError> {
        if body.len() > MAX_KV_VALUE_BYTES {
            tracing::warn!(
                len = body.len(),
                max = MAX_KV_VALUE_BYTES,
                "skipping LLM KV put: value too large"
            );
            return Ok(());
        }
        let ttl = cf_clamp_ttl(expiration_ttl_secs);
        let url = self.values_url(key);
        put_with_backoff_classified(
            &self.http,
            &url,
            &self.api_token,
            body,
            ttl,
        )
        .await
    }
}

fn cf_clamp_ttl(expiration_ttl_secs: u64) -> u64 {
    let ttl = expiration_ttl_secs.max(MIN_CF_TTL_SECS);
    if ttl != expiration_ttl_secs {
        tracing::warn!(
            requested = expiration_ttl_secs,
            clamped = ttl,
            "Cloudflare KV requires TTL >= {MIN_CF_TTL_SECS}s; clamped"
        );
    }
    ttl
}

#[async_trait]
impl LlmKvBackend for CloudflareKvClient {
    async fn get(
        &self,
        key: &str,
    ) -> Result<Option<String>, crate::error::LlmKvCacheError> {
        self.get_raw(key).await
    }

    async fn put(
        &self,
        key: &str,
        value: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError> {
        self.put_raw(key, value, expiration_ttl_secs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_clamp_ttl_zero_becomes_60() {
        assert_eq!(cf_clamp_ttl(0), MIN_CF_TTL_SECS);
    }

    #[test]
    fn cf_clamp_ttl_below_min_becomes_60() {
        assert_eq!(cf_clamp_ttl(1), MIN_CF_TTL_SECS);
        assert_eq!(cf_clamp_ttl(30), MIN_CF_TTL_SECS);
        assert_eq!(cf_clamp_ttl(59), MIN_CF_TTL_SECS);
    }

    #[test]
    fn cf_clamp_ttl_at_min_unchanged() {
        assert_eq!(cf_clamp_ttl(60), 60);
    }

    #[test]
    fn cf_clamp_ttl_above_min_unchanged() {
        assert_eq!(cf_clamp_ttl(120), 120);
        assert_eq!(cf_clamp_ttl(3600), 3600);
    }
}
