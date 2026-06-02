use async_trait::async_trait;

use crate::backend::LlmKvBackend;

/// Domestic / TiKV placeholder: always MISS, no-op writes.
pub struct InternalStubBackend;

#[async_trait]
impl LlmKvBackend for InternalStubBackend {
    async fn get(
        &self,
        _key: &str,
    ) -> Result<Option<String>, crate::error::LlmKvCacheError> {
        Ok(None)
    }

    async fn put(
        &self,
        _key: &str,
        _value: &str,
        _expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError> {
        Ok(())
    }
}
