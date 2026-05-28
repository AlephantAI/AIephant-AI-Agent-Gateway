use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;

const OPENAI_PROVIDER_CODE: &str = "openai";
const REDIS_KEY_OPENAI_BASE_URL: &str = "alephant:embeddings:base-url:openai";
const REDIS_BACKFILL_TTL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Default)]
pub struct EmbeddingBaseUrlResolver {
    memory_cache: Arc<RwLock<HashMap<String, String>>>,
    static_redis_base_url: Option<String>,
    static_db_base_url: Option<String>,
    redis: Option<Arc<crate::app_redis::AppRedis>>,
    router_store: Option<crate::store::router::RouterStore>,
}

impl EmbeddingBaseUrlResolver {
    #[must_use]
    pub fn new(
        memory_base_url: Option<String>,
        redis_base_url: Option<String>,
        db_base_url: Option<String>,
    ) -> Self {
        let mut memory = HashMap::new();
        if let Some(v) = memory_base_url
            && !v.trim().is_empty()
        {
            memory.insert(OPENAI_PROVIDER_CODE.to_string(), v);
        }
        Self {
            memory_cache: Arc::new(RwLock::new(memory)),
            static_redis_base_url: redis_base_url,
            static_db_base_url: db_base_url,
            redis: None,
            router_store: None,
        }
    }

    #[must_use]
    pub fn from_runtime(
        memory_base_url: Option<String>,
        redis: Option<Arc<crate::app_redis::AppRedis>>,
        router_store: Option<crate::store::router::RouterStore>,
    ) -> Self {
        let mut memory = HashMap::new();
        if let Some(v) = memory_base_url
            && !v.trim().is_empty()
        {
            memory.insert(OPENAI_PROVIDER_CODE.to_string(), v);
        }
        Self {
            memory_cache: Arc::new(RwLock::new(memory)),
            static_redis_base_url: None,
            static_db_base_url: None,
            redis,
            router_store,
        }
    }

    pub async fn resolve_openai_base_url(&self) -> Result<String, String> {
        if let Some(v) = self.get_memory_openai_base_url().await {
            return Ok(v);
        }

        if let Some(v) = normalize(self.static_redis_base_url.as_deref()) {
            self.set_memory_openai_base_url(v.clone()).await;
            return Ok(v);
        }

        if let Some(redis) = &self.redis
            && let Ok(Some(v)) =
                redis.get_string(REDIS_KEY_OPENAI_BASE_URL).await
            && let Some(v) = normalize(Some(v.as_str()))
        {
            self.set_memory_openai_base_url(v.clone()).await;
            return Ok(v);
        }

        if let Some(v) = normalize(self.static_db_base_url.as_deref()) {
            self.set_memory_openai_base_url(v.clone()).await;
            self.backfill_redis(v.clone()).await;
            return Ok(v);
        }

        if let Some(store) = &self.router_store
            && let Ok(rows) = store.get_all_providers_for_gateway().await
            && let Some(v) = rows.into_iter().find_map(|p| {
                if p.code.eq_ignore_ascii_case(OPENAI_PROVIDER_CODE) {
                    normalize(p.default_base_url.as_deref())
                } else {
                    None
                }
            })
        {
            self.set_memory_openai_base_url(v.clone()).await;
            self.backfill_redis(v.clone()).await;
            return Ok(v);
        }

        Err("openai embeddings base_url not found".to_string())
    }

    async fn get_memory_openai_base_url(&self) -> Option<String> {
        let map = self.memory_cache.read().await;
        map.get(OPENAI_PROVIDER_CODE)
            .and_then(|v| normalize(Some(v.as_str())))
    }

    async fn set_memory_openai_base_url(&self, url: String) {
        let mut map = self.memory_cache.write().await;
        map.insert(OPENAI_PROVIDER_CODE.to_string(), url);
    }

    async fn backfill_redis(&self, url: String) {
        if let Some(redis) = &self.redis {
            let _ = redis
                .set_ex(
                    REDIS_KEY_OPENAI_BASE_URL,
                    &url,
                    REDIS_BACKFILL_TTL_SECONDS,
                )
                .await;
        }
    }
}

fn normalize(v: Option<&str>) -> Option<String> {
    let s = v?.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::EmbeddingBaseUrlResolver;

    fn test_resolver_with_sources(
        memory: Option<&str>,
        redis: Option<&str>,
        db: Option<&str>,
    ) -> EmbeddingBaseUrlResolver {
        EmbeddingBaseUrlResolver::new(
            memory.map(ToOwned::to_owned),
            redis.map(ToOwned::to_owned),
            db.map(ToOwned::to_owned),
        )
    }

    #[tokio::test]
    async fn resolve_base_url_prefers_memory_then_redis_then_db() {
        let resolver = test_resolver_with_sources(
            Some("https://mem.openai.local"),
            Some("https://redis.openai.local"),
            Some("https://db.openai.local"),
        );
        let got = resolver.resolve_openai_base_url().await.unwrap();
        assert_eq!(got, "https://mem.openai.local");
    }
}
