use std::sync::Arc;

use async_trait::async_trait;
use http::HeaderMap;

use super::{
    EmbeddingBaseUrlResolver, EmbeddingIdentity, OpenAiEmbedderClient, QdrantEnsureCollection,
    QdrantStore, SemanticPolicy, build_cache_key, collection_name_for_embedding,
    extract_embed_text_from_body, parse_embedding_identity,
};

const ALEPHANT_EMBEDDINGS_MODEL: &str = "Alephant-Embeddings-Model";
const ALEPHANT_EMBEDDINGS_KEY: &str = "Alephant-Embeddings-Key";

#[derive(Debug, Clone)]
pub struct SemanticHit {
    pub cache_reference_id: String,
    pub response_bytes: Vec<u8>,
    pub score: f32,
}

pub struct SemanticLookupRequest<'a> {
    pub path: &'a str,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
}

#[derive(Clone)]
pub struct PreparedSemanticRequest {
    pub path: String,
    pub body: Vec<u8>,
    pub embed_text: String,
    pub policy: SemanticPolicy,
    pub embedding_identity: EmbeddingIdentity,
    pub api_key: String,
}

impl std::fmt::Debug for PreparedSemanticRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSemanticRequest")
            .field("path", &self.path)
            .field("body_len", &self.body.len())
            .field("embed_text_len", &self.embed_text.len())
            .field("policy", &self.policy)
            .field("embedding_identity", &self.embedding_identity)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SemanticWriteContext {
    pub collection: String,
    pub cache_key: String,
    pub params_hash: String,
    pub vector: Vec<f32>,
    pub response: Vec<u8>,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct SemanticLookupOutcome {
    pub hit: Option<SemanticHit>,
    pub write: Option<SemanticWriteContext>,
}

#[derive(Debug, Clone)]
pub struct SemanticWriteRequest {
    pub collection: String,
    pub cache_key: String,
    pub params_hash: String,
    pub vector: Vec<f32>,
    pub response: Vec<u8>,
    pub ttl_seconds: u64,
}

#[async_trait]
pub trait SemanticEmbedder: Send + Sync {
    async fn embed(
        &self,
        base_url: &str,
        api_key: &str,
        model: &str,
        input: &str,
    ) -> Result<Vec<f32>, String>;
}

#[async_trait]
pub trait SemanticVectorStore: Send + Sync {
    async fn ensure_collection(
        &self,
        collection: &str,
        dimension: usize,
    ) -> Result<QdrantEnsureCollection, String>;

    async fn search_top1(
        &self,
        collection: &str,
        vector: &[f32],
        params_hash: &str,
    ) -> Result<Option<super::SemanticVectorHit>, String>;

    async fn upsert(
        &self,
        collection: &str,
        cache_key: &str,
        params_hash: &str,
        vector: &[f32],
        response: &[u8],
        ttl_seconds: u64,
    ) -> Result<(), String>;
}

#[async_trait]
impl SemanticEmbedder for OpenAiEmbedderClient {
    async fn embed(
        &self,
        base_url: &str,
        api_key: &str,
        model: &str,
        input: &str,
    ) -> Result<Vec<f32>, String> {
        self.embed(base_url, api_key, model, input).await
    }
}

#[async_trait]
impl SemanticVectorStore for QdrantStore {
    async fn ensure_collection(
        &self,
        collection: &str,
        dimension: usize,
    ) -> Result<QdrantEnsureCollection, String> {
        self.ensure_collection(collection, dimension).await
    }

    async fn search_top1(
        &self,
        collection: &str,
        vector: &[f32],
        params_hash: &str,
    ) -> Result<Option<super::SemanticVectorHit>, String> {
        self.search_top1(collection, vector, params_hash).await
    }

    async fn upsert(
        &self,
        collection: &str,
        cache_key: &str,
        params_hash: &str,
        vector: &[f32],
        response: &[u8],
        ttl_seconds: u64,
    ) -> Result<(), String> {
        self.upsert(
            collection,
            cache_key,
            params_hash,
            vector,
            response,
            ttl_seconds,
        )
        .await
    }
}

#[derive(Clone)]
pub struct SemanticCacheService {
    embedder: Arc<dyn SemanticEmbedder>,
    vector_store: Arc<dyn SemanticVectorStore>,
    base_url_resolver: EmbeddingBaseUrlResolver,
    default_threshold: f32,
    default_ttl_seconds: u64,
}

impl std::fmt::Debug for SemanticCacheService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticCacheService")
            .field("default_threshold", &self.default_threshold)
            .field("default_ttl_seconds", &self.default_ttl_seconds)
            .finish_non_exhaustive()
    }
}

impl SemanticCacheService {
    #[must_use]
    pub fn new(
        embedder: Arc<dyn SemanticEmbedder>,
        vector_store: Arc<dyn SemanticVectorStore>,
        base_url_resolver: EmbeddingBaseUrlResolver,
        default_threshold: f32,
        default_ttl_seconds: u64,
    ) -> Self {
        Self {
            embedder,
            vector_store,
            base_url_resolver,
            default_threshold,
            default_ttl_seconds,
        }
    }

    pub fn prepare_request(
        &self,
        path: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<Option<PreparedSemanticRequest>, String> {
        if !semantic_path_supported(path) {
            return Ok(None);
        }
        let Some(raw_model) = header_value(headers, ALEPHANT_EMBEDDINGS_MODEL) else {
            return Ok(None);
        };
        let embedding_identity = parse_embedding_identity(&raw_model)?;
        let Some(api_key) = header_value(headers, ALEPHANT_EMBEDDINGS_KEY) else {
            return Ok(None);
        };
        let policy = SemanticPolicy::from_headers(
            headers,
            self.default_threshold,
            self.default_ttl_seconds,
        )?;
        let embed_text = extract_embed_text_from_body(body)?;
        Ok(Some(PreparedSemanticRequest {
            path: path.to_string(),
            body: body.to_vec(),
            embed_text,
            policy,
            embedding_identity,
            api_key,
        }))
    }

    pub async fn try_hit(
        &self,
        req: &SemanticLookupRequest<'_>,
    ) -> Result<Option<SemanticHit>, String> {
        let Some(prepared) = self.prepare_request(req.path, req.headers, req.body)? else {
            return Ok(None);
        };
        Ok(self.try_hit_prepared(&prepared).await?.hit)
    }

    pub async fn try_hit_prepared(
        &self,
        prepared: &PreparedSemanticRequest,
    ) -> Result<SemanticLookupOutcome, String> {
        tracing::info!(
            target: "semantic_cache::service",
            path = %prepared.path,
            embed_text_len = prepared.embed_text.len(),
            "semantic cache lookup started"
        );
        let write = self.resolve_vector_context(prepared).await?;
        let hit = self
            .vector_store
            .search_top1(&write.collection, &write.vector, &write.params_hash)
            .await?;
        let Some(hit) = hit else {
            tracing::info!(
                target: "semantic_cache::service",
                path = %prepared.path,
                params_hash = %write.params_hash,
                "semantic cache lookup miss: no candidate"
            );
            return Ok(SemanticLookupOutcome {
                hit: None,
                write: Some(write),
            });
        };
        if hit.score < prepared.policy.threshold {
            tracing::info!(
                target: "semantic_cache::service",
                path = %prepared.path,
                params_hash = %write.params_hash,
                score = hit.score,
                threshold = prepared.policy.threshold,
                "semantic cache lookup miss: score below threshold"
            );
            return Ok(SemanticLookupOutcome {
                hit: None,
                write: Some(write),
            });
        }
        tracing::info!(
            target: "semantic_cache::service",
            path = %prepared.path,
            params_hash = %write.params_hash,
            score = hit.score,
            threshold = prepared.policy.threshold,
            cache_key = %hit.cache_key,
            "semantic cache lookup hit"
        );
        Ok(SemanticLookupOutcome {
            hit: Some(SemanticHit {
                cache_reference_id: hit.cache_key,
                response_bytes: hit.response,
                score: hit.score,
            }),
            write: None,
        })
    }

    pub async fn store_response(
        &self,
        path: &str,
        headers: &HeaderMap,
        body: &[u8],
        response: &[u8],
    ) -> Result<(), String> {
        let Some(prepared) = self.prepare_request(path, headers, body)? else {
            return Ok(());
        };
        self.store_response_prepared(&prepared, response).await
    }

    pub async fn store_response_prepared(
        &self,
        prepared: &PreparedSemanticRequest,
        response: &[u8],
    ) -> Result<(), String> {
        let write = self.resolve_vector_context(prepared).await?;
        self.store_response_with_context(write, response).await
    }

    pub async fn store_response_with_context(
        &self,
        mut write: SemanticWriteContext,
        response: &[u8],
    ) -> Result<(), String> {
        if !response.is_empty() {
            write.response = response.to_vec();
        }
        let upsert_result = self
            .vector_store
            .upsert(
                &write.collection,
                &write.cache_key,
                &write.params_hash,
                &write.vector,
                &write.response,
                write.ttl_seconds,
            )
            .await;
        match &upsert_result {
            Ok(()) => tracing::info!(
                target: "semantic_cache::service",
                collection = %write.collection,
                cache_key = %write.cache_key,
                params_hash = %write.params_hash,
                vector_dim = write.vector.len(),
                ttl_seconds = write.ttl_seconds,
                stored = true,
                "semantic cache stored into qdrant"
            ),
            Err(err) => tracing::info!(
                target: "semantic_cache::service",
                collection = %write.collection,
                cache_key = %write.cache_key,
                params_hash = %write.params_hash,
                vector_dim = write.vector.len(),
                ttl_seconds = write.ttl_seconds,
                stored = false,
                error = %err,
                "semantic cache failed to store into qdrant"
            ),
        }
        upsert_result
    }

    async fn resolve_vector_context(
        &self,
        prepared: &PreparedSemanticRequest,
    ) -> Result<SemanticWriteContext, String> {
        let base_url = self.base_url_resolver.resolve_openai_base_url().await?;
        let vector = self
            .embedder
            .embed(
                &base_url,
                &prepared.api_key,
                prepared
                    .embedding_identity
                    .model_for_openai_compatible_api(),
                &prepared.embed_text,
            )
            .await?;
        let dimension = vector.len();
        let collection = collection_name_for_embedding(&prepared.embedding_identity, dimension)?;
        self.vector_store
            .ensure_collection(&collection, dimension)
            .await?;
        let identity = prepared
            .embedding_identity
            .params_hash_identity(&base_url, dimension);
        let built = build_cache_key(&prepared.path, &prepared.body, &identity)?;
        Ok(SemanticWriteContext {
            collection,
            cache_key: built.cache_key,
            params_hash: built.params_hash,
            vector,
            response: Vec::new(),
            ttl_seconds: prepared.policy.ttl_seconds,
        })
    }

    pub fn store_async(&self, req: SemanticWriteRequest) {
        let store = self.vector_store.clone();
        tokio::spawn(async move {
            if let Err(err) = store
                .upsert(
                    &req.collection,
                    &req.cache_key,
                    &req.params_hash,
                    &req.vector,
                    &req.response,
                    req.ttl_seconds,
                )
                .await
            {
                tracing::info!(
                    target: "semantic_cache::service",
                    cache_key = %req.cache_key,
                    params_hash = %req.params_hash,
                    vector_dim = req.vector.len(),
                    ttl_seconds = req.ttl_seconds,
                    stored = false,
                    error = %err,
                    "semantic cache failed to store into qdrant (async)"
                );
                tracing::warn!(%err, "semantic cache upsert failed");
            } else {
                tracing::info!(
                    target: "semantic_cache::service",
                    cache_key = %req.cache_key,
                    params_hash = %req.params_hash,
                    vector_dim = req.vector.len(),
                    ttl_seconds = req.ttl_seconds,
                    stored = true,
                    "semantic cache stored into qdrant (async)"
                );
            }
        });
    }
}

fn header_value(headers: &HeaderMap, key: &str) -> Option<String> {
    let raw = headers.get(key)?;
    let v = raw.to_str().ok()?.trim();
    if v.is_empty() {
        return None;
    }
    Some(v.to_string())
}

fn semantic_path_supported(path: &str) -> bool {
    !path.contains("/embeddings")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use http::HeaderMap;

    use super::{
        EmbeddingBaseUrlResolver, SemanticCacheService, SemanticEmbedder, SemanticLookupRequest,
        SemanticVectorStore, SemanticWriteContext,
    };
    use crate::semantic_cache::{QdrantEnsureCollection, SemanticVectorHit};

    struct MockEmbedder {
        vector: Vec<f32>,
    }
    #[async_trait]
    impl SemanticEmbedder for MockEmbedder {
        async fn embed(
            &self,
            _base_url: &str,
            _api_key: &str,
            _model: &str,
            _input: &str,
        ) -> Result<Vec<f32>, String> {
            Ok(self.vector.clone())
        }
    }

    struct MockStore {
        hit: Mutex<Option<SemanticVectorHit>>,
        ensure_calls: Mutex<Vec<(String, usize)>>,
        search_calls: Mutex<Vec<(String, String)>>,
        upsert_calls: Mutex<Vec<(String, String, String, Vec<f32>, Vec<u8>, u64)>>,
    }
    #[async_trait]
    impl SemanticVectorStore for MockStore {
        async fn ensure_collection(
            &self,
            collection: &str,
            dimension: usize,
        ) -> Result<QdrantEnsureCollection, String> {
            self.ensure_calls
                .lock()
                .unwrap()
                .push((collection.to_string(), dimension));
            Ok(QdrantEnsureCollection { created: false })
        }

        async fn search_top1(
            &self,
            collection: &str,
            _vector: &[f32],
            params_hash: &str,
        ) -> Result<Option<SemanticVectorHit>, String> {
            self.search_calls
                .lock()
                .unwrap()
                .push((collection.to_string(), params_hash.to_string()));
            Ok(self.hit.lock().unwrap().clone())
        }

        async fn upsert(
            &self,
            collection: &str,
            cache_key: &str,
            params_hash: &str,
            vector: &[f32],
            response: &[u8],
            ttl_seconds: u64,
        ) -> Result<(), String> {
            self.upsert_calls.lock().unwrap().push((
                collection.to_string(),
                cache_key.to_string(),
                params_hash.to_string(),
                vector.to_vec(),
                response.to_vec(),
                ttl_seconds,
            ));
            Ok(())
        }
    }

    fn test_chat_request<'a>(body: &'a [u8], headers: &'a HeaderMap) -> SemanticLookupRequest<'a> {
        SemanticLookupRequest {
            path: "/v1/chat/completions",
            headers,
            body,
        }
    }

    fn test_service(
        hit: Option<SemanticVectorHit>,
        dimension: usize,
    ) -> (SemanticCacheService, Arc<MockStore>) {
        let store = Arc::new(MockStore {
            hit: Mutex::new(hit),
            ensure_calls: Mutex::new(Vec::new()),
            search_calls: Mutex::new(Vec::new()),
            upsert_calls: Mutex::new(Vec::new()),
        });
        let svc = SemanticCacheService::new(
            Arc::new(MockEmbedder {
                vector: vec![0.1; dimension],
            }),
            store.clone(),
            EmbeddingBaseUrlResolver::new(Some("https://mem.openai.local".to_string()), None, None),
            0.9,
            3600,
        );
        (svc, store)
    }

    fn semantic_hit(score: f32) -> SemanticVectorHit {
        SemanticVectorHit {
            cache_key: "semantic-key-1".to_string(),
            score,
            response: br#"{"id":"from-semantic-cache"}"#.to_vec(),
        }
    }

    fn semantic_headers(model: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("Alephant-Embeddings-Model", model.parse().unwrap());
        headers.insert("Alephant-Embeddings-Key", "sk-test".parse().unwrap());
        headers
    }

    #[tokio::test]
    async fn try_hit_returns_semantic_hit_when_score_reaches_threshold() {
        let (svc, _store) = test_service(Some(semantic_hit(0.95)), 1536);
        let headers = semantic_headers("openai/text-embedding-3-large");
        let req = test_chat_request(
            br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#,
            &headers,
        );
        let hit = svc.try_hit(&req).await.unwrap();
        assert!(hit.is_some());
        assert_eq!(
            hit.unwrap().cache_reference_id,
            "semantic-key-1".to_string()
        );
    }

    #[test]
    fn prepare_request_returns_none_when_embeddings_headers_missing() {
        let (svc, _store) = test_service(Some(semantic_hit(0.95)), 1536);
        let headers = HeaderMap::new();
        let prepared = svc
            .prepare_request(
                "/v1/chat/completions",
                &headers,
                br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#,
            )
            .unwrap();
        assert!(prepared.is_none());
    }

    #[test]
    fn prepared_semantic_request_debug_redacts_api_key() {
        let (svc, _store) = test_service(Some(semantic_hit(0.95)), 1536);
        let mut headers = semantic_headers("openai/text-embedding-3-small");
        headers.insert("Alephant-Embeddings-Key", "sk-secret".parse().unwrap());
        let prepared = svc
            .prepare_request(
                "/v1/chat/completions",
                &headers,
                br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#,
            )
            .unwrap()
            .expect("prepared");

        let debug = format!("{prepared:?}");

        assert!(!debug.contains("sk-secret"));
        assert!(debug.contains("[redacted]"));
        assert!(debug.contains("body_len"));
        assert!(debug.contains("embed_text_len"));
    }

    #[tokio::test]
    async fn try_hit_prepared_returns_hit_and_no_write_context() {
        let (svc, _store) = test_service(Some(semantic_hit(0.95)), 1536);
        let headers = semantic_headers("openai/text-embedding-3-small");
        let prepared = svc
            .prepare_request(
                "/v1/chat/completions",
                &headers,
                br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#,
            )
            .unwrap()
            .expect("prepared");

        let outcome = svc.try_hit_prepared(&prepared).await.unwrap();
        assert!(outcome.hit.is_some());
        assert!(outcome.write.is_none());
    }

    #[tokio::test]
    async fn try_hit_prepared_ensures_and_searches_derived_collection() {
        let (svc, store) = test_service(None, 1536);
        let headers = semantic_headers("openai/text-embedding-3-small");
        let prepared = svc
            .prepare_request(
                "/v1/chat/completions",
                &headers,
                br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#,
            )
            .unwrap()
            .expect("prepared");

        let outcome = svc.try_hit_prepared(&prepared).await.unwrap();

        assert!(outcome.hit.is_none());
        assert!(outcome.write.is_some());
        assert_eq!(
            store.ensure_calls.lock().unwrap().as_slice(),
            [(
                "semantic_cache__openai__text-embedding-3-small__1536".to_string(),
                1536
            )]
        );
        assert_eq!(
            store.search_calls.lock().unwrap()[0].0,
            "semantic_cache__openai__text-embedding-3-small__1536"
        );
        assert_eq!(
            outcome.write.unwrap().collection,
            "semantic_cache__openai__text-embedding-3-small__1536"
        );
    }

    #[tokio::test]
    async fn store_response_with_context_upserts_using_context_collection() {
        let (svc, store) = test_service(None, 1536);
        let write = SemanticWriteContext {
            collection: "semantic_cache__openai__text-embedding-3-small__1536".to_string(),
            cache_key: "cache-key".to_string(),
            params_hash: "params-hash".to_string(),
            vector: vec![0.1; 1536],
            response: br#"{"id":"response"}"#.to_vec(),
            ttl_seconds: 60,
        };

        svc.store_response_with_context(write, br#"{"id":"replacement"}"#)
            .await
            .unwrap();

        let upserts = store.upsert_calls.lock().unwrap();
        assert_eq!(upserts.len(), 1);
        assert_eq!(
            upserts[0].0,
            "semantic_cache__openai__text-embedding-3-small__1536"
        );
        assert_eq!(upserts[0].4, br#"{"id":"replacement"}"#.to_vec());
    }
}
