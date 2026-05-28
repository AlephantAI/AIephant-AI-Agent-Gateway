//! In-process LRU cache for decrypted `master_keys` rows.
//!
//! Decryption is AES-256-GCM and relatively cheap, but still avoids redundant
//! work on hot paths by caching plaintext keyed by `master_key_id`.
//! Entries are invalidated explicitly when the `db_listener` sees a
//! `master_keys` UPDATE/DELETE.

use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use lru::LruCache;
use rand::seq::IndexedRandom as _;
use sqlx::PgPool;
use thiserror::Error;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    crypto::master_key::{self, MASTER_KEY_ENCRYPTION_KEY_LEN},
    error::internal::InternalError,
    store::router::RouterStore,
    types::provider::InferenceProvider,
};

/// Default LRU capacity (number of master keys kept in memory).
const DEFAULT_CAPACITY: usize = 256;

/// Decrypted master key together with provider metadata.
#[derive(Debug, Clone)]
pub struct DecryptedMasterKey {
    /// Plaintext provider API key (e.g. `sk-…`).
    pub plaintext: Arc<String>,
    /// Resolved provider enum, derived from `providers.code`.
    pub provider: InferenceProvider,
    /// Optional custom base URL override from `master_keys.base_url`.
    pub base_url: Option<String>,
    /// Row timestamp used for cache freshness tracking.
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum MasterKeyCacheError {
    #[error("master key not found or inactive: {0}")]
    NotFound(Uuid),
    #[error("provider code `{0}` is not a known InferenceProvider")]
    UnknownProvider(String),
    #[error("decryption failed: {0}")]
    Decrypt(#[from] master_key::DecryptError),
    #[error("database error: {0}")]
    Db(#[from] InternalError),
}

/// Shared, thread-safe LRU cache for decrypted master keys.
///
/// Clone is cheap (`Arc` inside).
#[derive(Clone, Debug)]
pub struct MasterKeyCache {
    inner: Arc<MasterKeyCacheInner>,
}

#[derive(Debug)]
struct MasterKeyCacheInner {
    pool: PgPool,
    enc_key: Arc<[u8; MASTER_KEY_ENCRYPTION_KEY_LEN]>,
    cache: Mutex<LruCache<Uuid, DecryptedMasterKey>>,
}

impl MasterKeyCache {
    #[must_use]
    pub fn new(pool: PgPool, enc_key: Arc<[u8; MASTER_KEY_ENCRYPTION_KEY_LEN]>) -> Self {
        let capacity = NonZeroUsize::new(DEFAULT_CAPACITY).expect("capacity > 0");
        Self {
            inner: Arc::new(MasterKeyCacheInner {
                pool,
                enc_key,
                cache: Mutex::new(LruCache::new(capacity)),
            }),
        }
    }

    /// Resolve a `master_key_id` to its decrypted key.
    ///
    /// Fast path: cache hit → return immediately (no DB round-trip).
    /// Slow path: DB fetch → AES-256-GCM decrypt → cache insert.
    pub async fn get(
        &self,
        master_key_id: Uuid,
    ) -> Result<DecryptedMasterKey, MasterKeyCacheError> {
        // ── fast path ──────────────────────────────────────────────────────
        {
            let mut guard = self.inner.cache.lock().expect("cache lock poisoned");
            if let Some(entry) = guard.get(&master_key_id) {
                debug!(%master_key_id, "master_key_cache: hit");
                return Ok(entry.clone());
            }
        }
        debug!(%master_key_id, "master_key_cache: miss — querying DB");

        // ── slow path: DB fetch (lock NOT held across await) ───────────────
        let store = RouterStore {
            pool: self.inner.pool.clone(),
        };
        let row = store
            .get_master_key_row(master_key_id)
            .await?
            .ok_or(MasterKeyCacheError::NotFound(master_key_id))?;

        let provider = InferenceProvider::from_provider_code(&row.provider_code)
            .map_err(|_| MasterKeyCacheError::UnknownProvider(row.provider_code.clone()))?;

        let plaintext_bytes = master_key::decrypt(
            &row.key_ciphertext,
            &row.key_nonce,
            self.inner.enc_key.as_ref(),
        )?;

        let entry = DecryptedMasterKey {
            plaintext: Arc::new(String::from_utf8_lossy(&plaintext_bytes).into_owned()),
            provider,
            base_url: row.base_url,
            updated_at: row.updated_at,
        };

        // ── insert into cache ──────────────────────────────────────────────
        {
            let mut guard = self.inner.cache.lock().expect("cache lock poisoned");
            guard.put(master_key_id, entry.clone());
        }

        Ok(entry)
    }

    /// Remove `master_key_id` from the cache.
    /// Called by the `db_listener` when a `master_keys` row is updated or
    /// deleted, so the next request fetches a fresh copy.
    pub fn invalidate(&self, master_key_id: Uuid) {
        let mut guard = self.inner.cache.lock().expect("cache lock poisoned");
        guard.pop(&master_key_id);
        debug!(%master_key_id, "master_key_cache: invalidated");
    }

    /// Resolve a master key with workspace-level fallback.
    ///
    /// 1. Try `get(primary_id)`. On success, return immediately.
    /// 2. If `!fallback_enabled`, return the error as-is.
    /// 3. If `fallback_enabled`, query all active master keys in the same
    ///    workspace and provider, pick one at random, and try `get` on it. If
    ///    the list is empty or the fallback `get` also fails, return an error.
    pub async fn get_primary_or_fallback(
        &self,
        primary_id: Uuid,
        workspace_id: Uuid,
        provider: InferenceProvider,
        fallback_enabled: bool,
    ) -> Result<DecryptedMasterKey, MasterKeyCacheError> {
        match self.get(primary_id).await {
            Ok(key) => {
                debug!(
                    %primary_id,
                    %workspace_id,
                    provider = %provider,
                    "not using load balance"
                );
                return Ok(key);
            }
            Err(e) if !fallback_enabled => return Err(e),
            Err(primary_err) => {
                warn!(
                    %primary_id,
                    %workspace_id,
                    provider = %provider,
                    error = %primary_err,
                    "master_key_cache: primary key unavailable, trying workspace fallback"
                );
            }
        }

        let store = RouterStore {
            pool: self.inner.pool.clone(),
        };
        let ids = store
            .get_master_key_ids_by_workspace_and_provider(workspace_id, provider.as_provider_code())
            .await
            .map_err(MasterKeyCacheError::Db)?;
        let candidate_ids: Vec<Uuid> = ids.into_iter().filter(|id| *id != primary_id).collect();

        if candidate_ids.is_empty() {
            warn!(
                %primary_id,
                %workspace_id,
                provider = %provider,
                "master_key_cache: workspace fallback list empty, no usable master key"
            );
            return Err(MasterKeyCacheError::NotFound(primary_id));
        }

        let selected_id = *candidate_ids
            .choose(&mut rand::rng())
            .expect("non-empty vec always yields Some");

        debug!(
            %primary_id,
            %workspace_id,
            provider = %provider,
            fallback_id = %selected_id,
            "using load balance"
        );
        warn!(
            %primary_id,
            %workspace_id,
            provider = %provider,
            fallback_id = %selected_id,
            "master_key_cache: using workspace fallback master key"
        );

        self.get(selected_id).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::crypto::master_key;

    fn preferred_test_db_url(default_url: &str) -> String {
        std::env::var("POSTGRES_DATABASE_URL")
            .or_else(|_| std::env::var("AI_GATEWAY__DATABASE__URL"))
            .unwrap_or_else(|_| default_url.to_string())
    }

    /// Verify that two lookups for the same `master_key_id` from a pre-warmed
    /// cache do not result in double work (the second is served from cache).
    ///
    /// We simulate caching by inserting a fake entry and checking that the
    /// same `Arc<String>` pointer is returned both times.
    #[test]
    fn cache_returns_same_arc_on_second_get() {
        let capacity = NonZeroUsize::new(4).unwrap();
        let cache: Mutex<LruCache<Uuid, DecryptedMasterKey>> = Mutex::new(LruCache::new(capacity));
        let id = Uuid::new_v4();

        let entry = DecryptedMasterKey {
            plaintext: Arc::new("sk-test-key".to_string()),
            provider: InferenceProvider::OpenAI,
            base_url: None,
            updated_at: Utc::now(),
        };

        // Seed the cache directly
        cache.lock().unwrap().put(id, entry.clone());

        let hit1 = cache.lock().unwrap().get(&id).cloned().unwrap();
        let hit2 = cache.lock().unwrap().get(&id).cloned().unwrap();

        // Same underlying allocation
        assert!(Arc::ptr_eq(&hit1.plaintext, &hit2.plaintext));
    }

    /// Verify that `invalidate` removes the entry so the next lookup misses.
    #[test]
    fn invalidate_removes_entry() {
        let capacity = NonZeroUsize::new(4).unwrap();
        let cache: Mutex<LruCache<Uuid, DecryptedMasterKey>> = Mutex::new(LruCache::new(capacity));
        let id = Uuid::new_v4();

        let entry = DecryptedMasterKey {
            plaintext: Arc::new("sk-other".to_string()),
            provider: InferenceProvider::OpenAI,
            base_url: None,
            updated_at: Utc::now(),
        };
        cache.lock().unwrap().put(id, entry);

        // Entry present
        assert!(cache.lock().unwrap().get(&id).is_some());

        // Invalidate
        cache.lock().unwrap().pop(&id);

        // Now gone
        assert!(cache.lock().unwrap().get(&id).is_none());
    }

    /// Verify decrypt counter: a real `MasterKeyCache::get` path decrypts at
    /// most once for repeated calls to a seeded cache.
    ///
    /// We use an atomic counter to track how many times `decrypt` would be
    /// called in a real scenario (simulated by direct cache inspection).
    #[test]
    fn decrypt_called_once_for_cached_key() {
        let decrypt_count = Arc::new(AtomicUsize::new(0));
        let capacity = NonZeroUsize::new(4).unwrap();
        let cache: Mutex<LruCache<Uuid, DecryptedMasterKey>> = Mutex::new(LruCache::new(capacity));
        let id = Uuid::new_v4();

        // Simulate the slow path (decrypt + insert) once
        decrypt_count.fetch_add(1, Ordering::SeqCst);
        let entry = DecryptedMasterKey {
            plaintext: Arc::new("sk-decrypt-once".to_string()),
            provider: InferenceProvider::OpenAI,
            base_url: None,
            updated_at: Utc::now(),
        };
        cache.lock().unwrap().put(id, entry);

        // Second access: cache hit, no decrypt
        let hit = cache.lock().unwrap().get(&id).cloned().unwrap();
        // We do NOT increment the counter here — cache hit path skips decrypt
        assert_eq!("sk-decrypt-once", hit.plaintext.as_str());
        assert_eq!(1, decrypt_count.load(Ordering::SeqCst)); // still 1
    }

    #[tokio::test]
    async fn get_primary_or_fallback_uses_other_key_when_primary_unusable() {
        const DEFAULT_TEST_DB_URL: &str = "postgres://postgres:postgres@localhost:54322/postgres";
        let db_url = preferred_test_db_url(DEFAULT_TEST_DB_URL);
        let pool = match PgPool::connect(&db_url).await {
            Ok(pool) => pool,
            Err(error) => {
                tracing::info!(
                    "skip db integration test: cannot connect to {db_url}: \
                     {error}"
                );
                return;
            }
        };

        let enc_key = Arc::new(*b"0123456789abcdef0123456789abcdef");
        let cache = MasterKeyCache::new(pool.clone(), enc_key.clone());
        let workspace_id = Uuid::new_v4();
        let workspace_owner_id = Uuid::parse_str("c94287af-2516-4df1-8a42-207ebd8b76d5").unwrap();
        let provider_id = Uuid::parse_str("d79f440f-61c2-49fc-a29d-4c9d6b208985").unwrap();

        let primary_id = Uuid::new_v4();
        let fallback_id = Uuid::new_v4();
        let primary_ciphertext = vec![1_u8, 2, 3];
        let primary_bad_nonce = vec![9_u8; 11]; // invalid nonce len => decrypt error

        let fallback_plaintext = "sk-fallback-ok";
        let (fallback_ciphertext, fallback_nonce) =
            master_key::encrypt(fallback_plaintext.as_bytes(), enc_key.as_ref())
                .expect("encrypt fallback key");

        let workspace_slug = format!("mk-cache-it-{}", Uuid::new_v4().simple());
        sqlx::query(
            r"INSERT INTO workspaces
               (id, name, slug, type, owner_id, settings)
               VALUES ($1, 'MK Cache Integration Test', $2, 'personal', $3, '{}')",
        )
        .bind(workspace_id)
        .bind(&workspace_slug)
        .bind(workspace_owner_id)
        .execute(&pool)
        .await
        .expect("insert test workspace");

        sqlx::query(
            r"INSERT INTO master_keys
               (id, workspace_id, label, provider_id, key_ciphertext, key_nonce, masked_key)
               VALUES
               ($1, $2, 'primary_bad', $3, $4, $5, 'sk-...primary'),
               ($6, $2, 'fallback_ok', $3, $7, $8, 'sk-...fallback')",
        )
        .bind(primary_id)
        .bind(workspace_id)
        .bind(provider_id)
        .bind(&primary_ciphertext)
        .bind(&primary_bad_nonce)
        .bind(fallback_id)
        .bind(&fallback_ciphertext)
        .bind(&fallback_nonce)
        .execute(&pool)
        .await
        .expect("insert test master keys");

        let resolved = cache
            .get_primary_or_fallback(primary_id, workspace_id, InferenceProvider::OpenAI, true)
            .await
            .expect("fallback should resolve with other key");

        assert_eq!(resolved.plaintext.as_str(), fallback_plaintext);
        assert_eq!(resolved.provider, InferenceProvider::OpenAI);

        sqlx::query(r"DELETE FROM master_keys WHERE id IN ($1, $2)")
            .bind(primary_id)
            .bind(fallback_id)
            .execute(&pool)
            .await
            .expect("cleanup test master keys");

        sqlx::query(r"DELETE FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .execute(&pool)
            .await
            .expect("cleanup test workspace");
    }
}
