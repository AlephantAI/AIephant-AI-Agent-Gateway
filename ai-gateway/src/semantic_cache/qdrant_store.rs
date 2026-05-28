use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticVectorHit {
    pub cache_key: String,
    pub score: f32,
    pub response: Vec<u8>,
}

#[derive(Clone)]
pub struct QdrantStore {
    pub base_url: String,
    pub api_key: Option<String>,
    pub client: reqwest::Client,
}

impl std::fmt::Debug for QdrantStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QdrantStore")
            .field("base_url", &self.base_url)
            .field("api_key_present", &self.api_key.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QdrantEnsureCollection {
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QdrantCollectionState {
    Missing,
    Exists { dimension: Option<usize> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantPointPayload {
    pub cache_key: String,
    pub params_hash: String,
    pub response_b64: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    result: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    score: f32,
    payload: QdrantPointPayload,
}

impl QdrantStore {
    pub async fn ensure_collection(
        &self,
        collection: &str,
        dimension: usize,
    ) -> Result<QdrantEnsureCollection, String> {
        if dimension == 0 {
            return Err("embedding dimension must be greater than zero".to_string());
        }

        match self.collection_state(collection).await? {
            QdrantCollectionState::Missing => {
                self.create_collection(collection, dimension).await?;
                Ok(QdrantEnsureCollection { created: true })
            }
            QdrantCollectionState::Exists {
                dimension: Some(existing),
            } if existing == dimension => Ok(QdrantEnsureCollection { created: false }),
            QdrantCollectionState::Exists {
                dimension: Some(existing),
            } => Err(format!(
                "qdrant collection dimension mismatch: collection \
                 {collection} has dimension {existing}, expected {dimension}"
            )),
            QdrantCollectionState::Exists { dimension: None } => Err(format!(
                "qdrant collection {collection} has unsupported vector config"
            )),
        }
    }

    pub async fn collection_state(
        &self,
        collection: &str,
    ) -> Result<QdrantCollectionState, String> {
        let mut req = self.client.get(collection_url(&self.base_url, collection));
        if let Some(api_key) = self.api_key.as_deref() {
            req = req.header("api-key", api_key);
        }

        let resp = req.send().await.map_err(|e| e.to_string())?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(QdrantCollectionState::Missing);
        }
        if !resp.status().is_success() {
            return Err(format!(
                "qdrant collection metadata failed: {}",
                resp.status()
            ));
        }

        let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok(QdrantCollectionState::Exists {
            dimension: collection_dimension_from_value(&value)?,
        })
    }

    pub async fn create_collection(
        &self,
        collection: &str,
        dimension: usize,
    ) -> Result<(), String> {
        let mut req = self
            .client
            .put(collection_url(&self.base_url, collection))
            .json(&json!({
                "vectors": {
                    "size": dimension,
                    "distance": "Cosine"
                }
            }));
        if let Some(api_key) = self.api_key.as_deref() {
            req = req.header("api-key", api_key);
        }

        let resp = req.send().await.map_err(|e| e.to_string())?;
        if resp.status() == StatusCode::CONFLICT {
            return Ok(());
        }
        if !resp.status().is_success() {
            return Err(format!(
                "qdrant create collection failed: {}",
                resp.status()
            ));
        }
        Ok(())
    }

    pub async fn search_top1(
        &self,
        collection: &str,
        vector: &[f32],
        params_hash: &str,
    ) -> Result<Option<SemanticVectorHit>, String> {
        if vector.is_empty() {
            return Ok(None);
        }
        let url = points_search_url(&self.base_url, collection);
        let mut req = self.client.post(url).json(&json!({
            "vector": vector,
            "limit": 1,
            "with_payload": true,
            "filter": {
                "must": [{
                    "key": "params_hash",
                    "match": {"value": params_hash}
                }]
            }
        }));
        if let Some(api_key) = self.api_key.as_deref() {
            req = req.header("api-key", api_key);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(format!("qdrant search failed: {}", resp.status()));
        }
        let parsed: SearchResponse = resp.json().await.map_err(|e| e.to_string())?;
        let now = now_unix();
        for hit in parsed.result {
            if let Some(decoded) = decode_payload_if_fresh(&hit.payload, now) {
                return Ok(Some(SemanticVectorHit {
                    cache_key: decoded.cache_key,
                    score: hit.score,
                    response: decoded.response,
                }));
            }
        }
        Ok(None)
    }

    pub async fn upsert(
        &self,
        collection: &str,
        cache_key: &str,
        params_hash: &str,
        vector: &[f32],
        response: &[u8],
        ttl_seconds: u64,
    ) -> Result<(), String> {
        let expires_at = if ttl_seconds == 0 {
            None
        } else {
            Some(now_unix() + i64::try_from(ttl_seconds).unwrap_or(i64::MAX))
        };
        let payload = QdrantPointPayload {
            cache_key: cache_key.to_string(),
            params_hash: params_hash.to_string(),
            response_b64: STANDARD.encode(response),
            expires_at,
        };
        let id = point_id(cache_key, params_hash);
        let url = points_upsert_url(&self.base_url, collection);
        let mut req = self.client.put(url).json(&json!({
            "points": [{
                "id": id,
                "vector": vector,
                "payload": payload
            }]
        }));
        if let Some(api_key) = self.api_key.as_deref() {
            req = req.header("api-key", api_key);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("qdrant upsert failed: {}", resp.status()));
        }
        Ok(())
    }
}

fn point_id(cache_key: &str, params_hash: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cache_key.hash(&mut h);
    params_hash.hash(&mut h);
    h.finish()
}

fn trim_slash(base: &str) -> String {
    base.trim_end_matches('/').to_string()
}

fn collection_url(base_url: &str, collection: &str) -> String {
    format!("{}/collections/{}", trim_slash(base_url), collection)
}

fn points_search_url(base_url: &str, collection: &str) -> String {
    format!("{}/points/search", collection_url(base_url, collection))
}

fn points_upsert_url(base_url: &str, collection: &str) -> String {
    format!("{}/points?wait=true", collection_url(base_url, collection))
}

fn collection_dimension_from_value(value: &serde_json::Value) -> Result<Option<usize>, String> {
    let Some(vectors) = value
        .get("result")
        .and_then(|v| v.get("config"))
        .and_then(|v| v.get("params"))
        .and_then(|v| v.get("vectors"))
    else {
        return Err("qdrant collection response missing vectors config".to_string());
    };

    if let Some(size) = vectors.get("size").and_then(serde_json::Value::as_u64) {
        return usize::try_from(size)
            .map(Some)
            .map_err(|_| "qdrant collection vector size is too large".to_string());
    }

    Ok(None)
}

fn now_unix() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs();
    i64::try_from(secs).unwrap_or(i64::MAX)
}

#[derive(Debug)]
pub struct DecodedPayload {
    pub cache_key: String,
    pub response: Vec<u8>,
}

#[must_use]
pub fn decode_payload_if_fresh(
    payload: &QdrantPointPayload,
    now_unix: i64,
) -> Option<DecodedPayload> {
    if let Some(exp) = payload.expires_at
        && exp > 0
        && exp < now_unix
    {
        return None;
    }
    let response = STANDARD.decode(payload.response_b64.as_bytes()).ok()?;
    Some(DecodedPayload {
        cache_key: payload.cache_key.clone(),
        response,
    })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::{
        QdrantPointPayload, QdrantStore, collection_dimension_from_value, collection_url,
        decode_payload_if_fresh, points_search_url, points_upsert_url,
    };

    #[test]
    fn decode_hit_skips_expired_payload() {
        let now = 1_700_000_000_i64;
        let hit = QdrantPointPayload {
            cache_key: "k".into(),
            params_hash: "p".into(),
            response_b64: base64::engine::general_purpose::STANDARD.encode(br#"{"ok":true}"#),
            expires_at: Some(now - 10),
        };
        assert!(decode_payload_if_fresh(&hit, now).is_none());
    }

    #[test]
    fn collection_dimension_from_value_reads_single_vector_size() {
        let value = json!({
            "result": {
                "config": {
                    "params": {
                        "vectors": {
                            "size": 1536,
                            "distance": "Cosine"
                        }
                    }
                }
            }
        });

        assert_eq!(collection_dimension_from_value(&value).unwrap(), Some(1536));
    }

    #[test]
    fn collection_dimension_from_value_treats_named_vectors_as_unsupported() {
        let value = json!({
            "result": {
                "config": {
                    "params": {
                        "vectors": {
                            "text": {
                                "size": 1536,
                                "distance": "Cosine"
                            }
                        }
                    }
                }
            }
        });

        assert_eq!(collection_dimension_from_value(&value).unwrap(), None);
    }

    #[test]
    fn qdrant_store_debug_redacts_api_key() {
        let store = QdrantStore {
            base_url: "http://qdrant.local".to_string(),
            api_key: Some("qdrant-secret".to_string()),
            client: reqwest::Client::new(),
        };

        let debug = format!("{store:?}");

        assert!(debug.contains("http://qdrant.local"));
        assert!(debug.contains("api_key_present"));
        assert!(debug.contains("true"));
        assert!(!debug.contains("qdrant-secret"));
    }

    #[test]
    fn qdrant_urls_use_runtime_collection() {
        assert_eq!(
            collection_url("http://qdrant.local/", "semantic_cache__openai__m__3"),
            "http://qdrant.local/collections/semantic_cache__openai__m__3"
        );
        assert_eq!(
            points_search_url("http://qdrant.local/", "semantic_cache__openai__m__3"),
            "http://qdrant.local/collections/semantic_cache__openai__m__3/points/search"
        );
        assert_eq!(
            points_upsert_url("http://qdrant.local/", "semantic_cache__openai__m__3"),
            "http://qdrant.local/collections/semantic_cache__openai__m__3/points?wait=true"
        );
    }
}
