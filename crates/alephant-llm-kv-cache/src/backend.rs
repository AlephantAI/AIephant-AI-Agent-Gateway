use async_trait::async_trait;

#[async_trait]
pub trait LlmKvBackend: Send + Sync {
    async fn get(
        &self,
        key: &str,
    ) -> Result<Option<String>, crate::error::LlmKvCacheError>;

    async fn put(
        &self,
        key: &str,
        value: &str,
        expiration_ttl_secs: u64,
    ) -> Result<(), crate::error::LlmKvCacheError>;
}
