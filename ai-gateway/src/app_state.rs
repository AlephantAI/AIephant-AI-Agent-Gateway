use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use opentelemetry::KeyValue;
use rustc_hash::{FxHashMap as HashMap, FxHashMap};
use tokio::sync::{
    RwLock,
    mpsc::{Receiver, Sender},
};
use tower::discover::Change;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    config::{
        Config, providers::ProvidersConfig,
        response_headers::ResponseHeadersConfig, router::RouterConfig,
    },
    content_filter::ContentFilterClientHolder,
    crypto::master_key::MASTER_KEY_ENCRYPTION_KEY_LEN,
    discover::{
        monitor::{
            health::provider::HealthMonitorMap,
            metrics::EndpointMetricsRegistry, rate_limit::RateLimitMonitorMap,
        },
        router::BareModelExpandIndex,
    },
    error::init::InitError,
    logger::service::AlephantHttpClient,
    metrics::Metrics,
    router::service::Router,
    semantic_cache::SemanticCacheService,
    store::{
        master_key_cache::MasterKeyCache,
        router::{DbVirtualKey, RouterStore},
        s3::BaseS3Client,
    },
    types::{
        org::OrgId,
        provider::{InferenceProvider, ProviderKeyMap, ProviderKeys},
        rate_limit::{
            RateLimitEvent, RateLimitEventReceivers, RateLimitEventSenders,
        },
        router::RouterId,
    },
    virtual_key::legacy_key::Key,
};

/// Maps each workspace UUID to the set of `InferenceProvider`s that the
/// workspace has explicitly enabled in the `provider_configs` DB table.
///
/// An absent key means the workspace has no `provider_configs` rows, and all
/// providers are allowed (compat behaviour).  An empty `HashSet`
/// value is treated the same way (defensive, should not occur in practice).
pub type WorkspaceProviderAllowlist =
    FxHashMap<Uuid, HashSet<InferenceProvider>>;

#[derive(Debug, Clone)]
pub struct AppState(pub Arc<InnerAppState>);

impl AppState {
    #[must_use]
    pub fn response_headers_config(&self) -> ResponseHeadersConfig {
        self.0.config.response_headers
    }

    #[must_use]
    pub fn config(&self) -> &Config {
        &self.0.config
    }

    #[must_use]
    pub fn request_log_transport(
        &self,
    ) -> &std::sync::Arc<dyn crate::logger::transport::LogTransport> {
        &self.0.request_log_transport
    }

    #[must_use]
    pub fn redis(&self) -> Option<&std::sync::Arc<crate::app_redis::AppRedis>> {
        self.0.redis.as_ref()
    }

    #[must_use]
    pub fn llm_kv(
        &self,
    ) -> &std::sync::Arc<dyn alephant_llm_kv_cache::LlmKvBackend + Send + Sync>
    {
        &self.0.llm_kv
    }

    #[must_use]
    pub fn semantic_cache(&self) -> Option<&Arc<SemanticCacheService>> {
        self.0.semantic_cache.as_ref()
    }

    /// Whether the first DB poll cycle has run (cache-warming attempted).
    #[must_use]
    pub fn is_cache_warmed(&self) -> bool {
        self.0.cache_warmed.load(Ordering::Acquire)
    }

    /// Open the readiness gate (called once by `DatabaseListener` after the
    /// first poll attempt).
    pub fn mark_cache_warmed(&self) {
        self.0.cache_warmed.store(true, Ordering::Release);
        tracing::info!("cache_warmed: readiness gate open");
    }

    /// Returns the provider bootstrap DB fingerprint recorded by initial
    /// provider discovery.
    #[must_use]
    pub fn provider_bootstrap_fingerprint(&self) -> Option<String> {
        self.0
            .provider_bootstrap_fingerprint
            .read()
            .expect("provider_bootstrap_fingerprint lock poisoned")
            .clone()
    }

    /// Records the provider bootstrap DB fingerprint for initial provider
    /// discovery.
    pub fn mark_providers_bootstrapped(&self, fingerprint: String) {
        *self
            .0
            .provider_bootstrap_fingerprint
            .write()
            .expect("provider_bootstrap_fingerprint lock poisoned") =
            Some(fingerprint);
    }

    pub async fn content_filter_client(
        &self,
    ) -> Option<Arc<crate::content_filter::ContentFilterGrpcClient>> {
        self.0.content_filter.get().await
    }

    pub async fn x402_payment_client(
        &self,
    ) -> Option<Arc<crate::x402::payment_client::X402PaymentGrpcClient>> {
        self.0.x402_payment.get().await
    }

    #[must_use]
    pub fn router_store(&self) -> Option<&RouterStore> {
        self.0.router_store.as_ref()
    }

    /// Returns a snapshot of the current provider configuration.
    ///
    /// Reflects the latest data from the DB (updated hot by
    /// `db_listener`).
    ///
    /// Uses a sync `std::sync::RwLock` so this can be called from both async
    /// and non-async contexts without `.await`.
    #[must_use]
    pub fn get_providers_config(&self) -> ProvidersConfig {
        self.0
            .providers_config
            .read()
            .expect("providers_config lock poisoned")
            .clone()
    }

    /// Replaces the in-memory provider configuration.
    ///
    /// Called by `ProviderDbDiscovery` on startup and by `db_listener`
    /// whenever `providers` or `provider_models` changes.
    pub fn set_providers_config(&self, config: ProvidersConfig) {
        *self
            .0
            .providers_config
            .write()
            .expect("providers_config lock poisoned") = config;
    }

    /// Built with `ProvidersConfig`; expands bare `model_id` from policy into
    /// `code/model` candidates.
    #[must_use]
    pub fn get_bare_model_expand_index(&self) -> BareModelExpandIndex {
        self.0
            .bare_model_expand_index
            .read()
            .expect("bare_model_expand_index lock poisoned")
            .clone()
    }

    /// Paired with `set_providers_config` on Cloud hot reload.
    pub fn set_bare_model_expand_index(&self, index: BareModelExpandIndex) {
        *self
            .0
            .bare_model_expand_index
            .write()
            .expect("bare_model_expand_index lock poisoned") = index;
    }

    /// Returns a snapshot of the workspace → provider allowlist (F-10).
    #[must_use]
    pub fn get_workspace_provider_allowlist(
        &self,
    ) -> WorkspaceProviderAllowlist {
        self.0
            .workspace_provider_allowlist
            .read()
            .expect("workspace_provider_allowlist lock poisoned")
            .clone()
    }

    /// Replaces the in-memory workspace provider allowlist.
    ///
    /// Called by `ProviderDbDiscovery` at startup and by `db_listener`
    /// whenever `provider_configs` changes.
    pub fn set_workspace_provider_allowlist(
        &self,
        allowlist: WorkspaceProviderAllowlist,
    ) {
        *self
            .0
            .workspace_provider_allowlist
            .write()
            .expect("workspace_provider_allowlist lock poisoned") = allowlist;
    }

    /// Returns `true` when `provider` is allowed for `workspace_id`.
    ///
    /// Returns `true` unconditionally when:
    /// * the allowlist is empty (no Cloud DB data loaded yet), or
    /// * the workspace has no entry in `provider_configs` (all providers
    ///   allowed for that workspace — compat semantics).
    #[must_use]
    pub fn is_provider_allowed_for_workspace(
        &self,
        workspace_id: Uuid,
        provider: &InferenceProvider,
    ) -> bool {
        let allowlist = self
            .0
            .workspace_provider_allowlist
            .read()
            .expect("workspace_provider_allowlist lock poisoned");
        // Empty global map → no Cloud DB data loaded yet → allow all.
        if allowlist.is_empty() {
            return true;
        }
        // Workspace absent → no provider_configs rows → allow all (compat).
        let Some(allowed) = allowlist.get(&workspace_id) else {
            return true;
        };
        // Empty set is defensive (shouldn't occur) → allow all.
        if allowed.is_empty() {
            return true;
        }
        allowed.contains(provider)
    }

    #[must_use]
    pub fn get_provider_is_router_flags(&self) -> FxHashMap<String, bool> {
        self.0
            .provider_is_router_flags
            .read()
            .expect("provider_is_router_flags lock poisoned")
            .clone()
    }

    pub fn set_provider_is_router_flags(&self, flags: FxHashMap<String, bool>) {
        *self
            .0
            .provider_is_router_flags
            .write()
            .expect("provider_is_router_flags lock poisoned") = flags;
    }

    /// Runtime skip for embedded model whitelist + model-mapping on
    /// OpenAI-compatible paths.
    ///
    /// When the snapshot is empty (no Cloud DB seed path), only `openrouter`
    /// matches — same as historic YAML-only behaviour.
    #[must_use]
    pub fn provider_skips_model_mapping_catalog(
        &self,
        provider: &InferenceProvider,
    ) -> bool {
        let InferenceProvider::Named(code) = provider else {
            return false;
        };
        let key = code.as_str();
        let map = self.get_provider_is_router_flags();
        if map.is_empty() {
            return key == "openrouter";
        }
        map.get(key).copied().unwrap_or(false)
    }
}

#[derive(derive_more::Debug)]
pub struct InnerAppState {
    pub config: Config,
    pub s3: BaseS3Client,
    pub router_store: Option<RouterStore>,
    pub alephant_http_client: AlephantHttpClient,
    pub request_log_transport:
        std::sync::Arc<dyn crate::logger::transport::LogTransport>,
    /// Mirrors `request_log.log_queue_redis_url`; `None` when unset.
    pub redis: Option<std::sync::Arc<crate::app_redis::AppRedis>>,
    /// Top level metrics which are exported to OpenTelemetry.
    pub metrics: Metrics,
    /// Metrics to track provider health and rate limits.
    /// Not used for OpenTelemetry, only used for the load balancer to be
    /// dynamically updated based on provider health and rate limits.
    pub endpoint_metrics: EndpointMetricsRegistry,
    pub health_monitors: HealthMonitorMap,
    pub rate_limit_monitors: RateLimitMonitorMap,
    pub rate_limit_senders: RateLimitEventSenders,
    pub rate_limit_receivers: RateLimitEventReceivers,
    pub router_tx: RwLock<Option<Sender<Change<RouterId, Router>>>>,

    pub provider_keys: ProviderKeys,
    pub alephant_api_keys: RwLock<Option<HashSet<Key>>>,
    pub router_organization_map: RwLock<HashMap<RouterId, OrgId>>,
    /// AES-256 master key encryption key for decrypting
    /// `master_keys.key_ciphertext`.
    pub master_key_encryption_key:
        Option<Arc<[u8; MASTER_KEY_ENCRYPTION_KEY_LEN]>>,
    /// In-memory cache of active virtual keys, keyed by `key_hash`.
    pub virtual_keys_cache: RwLock<Option<HashMap<String, DbVirtualKey>>>,
    /// LRU cache of decrypted master keys.
    pub master_key_cache: Option<MasterKeyCache>,
    /// Learned regional endpoint cache used when Redis is unavailable.
    #[debug(skip)]
    pub regional_endpoint_cache:
        crate::dispatcher::regional_endpoint::RegionalEndpointCache,
    /// Hot-updatable provider configuration.
    ///
    /// Populated from `providers` / `provider_models` DB tables at startup
    /// and updated by `db_listener` on every change.
    ///
    /// Uses `std::sync::RwLock` (not async) so callers can read it in both
    /// sync and async contexts without needing `.await`.
    pub providers_config: std::sync::RwLock<ProvidersConfig>,

    /// Multi-map bare `model_id` → `code/model` entries from the gateway
    /// registry built with `provider_models` + `providers`; hot-reloaded
    /// together with
    /// [`Self::providers_config`](InnerAppState::providers_config).
    pub bare_model_expand_index: std::sync::RwLock<BareModelExpandIndex>,

    /// Fingerprint recorded before initial DB provider discovery reads
    /// provider state and builds the initial provider-derived router set.
    pub provider_bootstrap_fingerprint: std::sync::RwLock<Option<String>>,

    /// Router IDs successfully built by initial DB provider discovery.
    pub provider_bootstrapped_router_ids: std::sync::RwLock<HashSet<RouterId>>,

    /// gRPC client for `policy.v1.PolicyService`, used by content filtering
    /// and x402 inbound policy evaluation.
    pub content_filter: ContentFilterClientHolder,

    /// gRPC client for `payment.v1.X402PaymentService` when `x402.enabled`.
    pub x402_payment: crate::x402::payment_client::X402PaymentClientHolder,

    /// Workspace → allowed-providers map (F-10).
    ///
    /// Loaded from `provider_configs JOIN providers` at startup and refreshed
    /// by `db_listener` whenever `provider_configs` changes.
    ///
    /// Uses `std::sync::RwLock` for the same reason as `providers_config`.
    pub workspace_provider_allowlist:
        std::sync::RwLock<WorkspaceProviderAllowlist>,
    /// `providers.code` → DB `is_router`; refreshed with
    /// [`Self::providers_config`].
    pub provider_is_router_flags: std::sync::RwLock<FxHashMap<String, bool>>,
    /// LLM response KV cache backend (Cloudflare, TiKV, or stub).
    #[debug(skip)]
    pub llm_kv:
        std::sync::Arc<dyn alephant_llm_kv_cache::LlmKvBackend + Send + Sync>,
    /// Semantic cache service (Qdrant + embeddings), optional.
    pub semantic_cache: Option<Arc<SemanticCacheService>>,
    /// Set to `true` after the first `poll_database` attempt completes
    /// (regardless of success). Used by the readiness probe to gate traffic
    /// until the initial cache-warming cycle has run.
    pub cache_warmed: AtomicBool,
}

impl AppState {
    pub async fn get_rate_limit_tx(
        &self,
        router_id: &RouterId,
    ) -> Result<Sender<RateLimitEvent>, InitError> {
        let rate_limit_channels = self.0.rate_limit_senders.read().await;
        let rate_limit_tx =
            rate_limit_channels.get(router_id).ok_or_else(|| {
                InitError::RateLimitChannelsNotInitialized(router_id.clone())
            })?;
        Ok(rate_limit_tx.clone())
    }

    pub async fn add_rate_limit_tx(
        &self,
        router_id: RouterId,
        rate_limit_tx: Sender<RateLimitEvent>,
    ) {
        let mut rate_limit_channels = self.0.rate_limit_senders.write().await;
        rate_limit_channels.insert(router_id, rate_limit_tx);
    }

    pub async fn add_rate_limit_rx(
        &self,
        router_id: RouterId,
        rate_limit_rx: Receiver<RateLimitEvent>,
    ) {
        let mut rate_limit_channels = self.0.rate_limit_receivers.write().await;
        rate_limit_channels.insert(router_id, rate_limit_rx);
    }

    pub async fn get_router_tx(
        &self,
    ) -> Option<Sender<Change<RouterId, Router>>> {
        let router_tx = self.0.router_tx.read().await;
        router_tx.clone()
    }

    pub async fn set_router_tx(&self, tx: Sender<Change<RouterId, Router>>) {
        let mut router_tx = self.0.router_tx.write().await;
        *router_tx = Some(tx);
    }

    pub async fn check_alephant_api_key(
        &self,
        api_key_hash: &str,
    ) -> Option<Key> {
        let alephant_api_keys = self.0.alephant_api_keys.read().await;
        alephant_api_keys
            .as_ref()?
            .iter()
            .find(|k| k.key_hash == api_key_hash)
            .cloned()
    }

    // ------------------------------------------------------------------
    // virtual_keys_cache helpers
    // ------------------------------------------------------------------

    /// Look up a virtual key by its `key_hash`.
    /// Returns `None` if the cache is not initialised or the key is not
    /// found.
    pub async fn check_virtual_key(
        &self,
        key_hash: &str,
    ) -> Option<DbVirtualKey> {
        self.0
            .virtual_keys_cache
            .read()
            .await
            .as_ref()?
            .get(key_hash)
            .cloned()
    }

    /// Resolve a virtual key for auth: memory first, then optional PG
    /// single-row fallback when the cache is initialised (Cloud path).
    ///
    /// On PG errors, logs, increments `vk_pg_fallback_db_errors_total`, and
    /// returns `None` (caller treats as invalid credentials).
    pub async fn resolve_virtual_key_for_auth(
        &self,
        key_hash: &str,
    ) -> Option<DbVirtualKey> {
        if let Some(vk) = self.check_virtual_key(key_hash).await {
            return Some(vk);
        }

        let store = self.0.router_store.as_ref()?;

        if self.0.virtual_keys_cache.read().await.is_none() {
            return None;
        }

        match store.get_db_virtual_key_by_key_hash(key_hash).await {
            Ok(Some(vk)) => {
                self.0.metrics.vk.pg_fallback_heal.add(1, &[]);
                info!(
                    key_hash_prefix = %key_hash.get(..12).unwrap_or(""),
                    "virtual_key: PG fallback loaded row into memory cache",
                );
                self.set_virtual_key(vk.clone()).await;
                Some(vk)
            }
            Ok(None) => None,
            Err(e) => {
                self.0.metrics.vk.pg_fallback_db_errors.add(1, &[]);
                warn!(
                    error = %e,
                    "virtual_key: PG fallback query failed",
                );
                None
            }
        }
    }

    /// Upsert a virtual key into the cache (insert or overwrite).
    /// No-op when the cache is `None`.
    pub async fn set_virtual_key(&self, vk: DbVirtualKey) {
        let mut cache = self.0.virtual_keys_cache.write().await;
        if let Some(map) = cache.as_mut()
            && upsert_virtual_key_entry(map, vk)
        {
            self.0.metrics.routers.alephant_api_keys.add(1, &[]);
        }
    }

    /// Remove a virtual key from the cache.  No-op when the cache is `None`.
    pub async fn remove_virtual_key(&self, key_hash: &str) {
        let mut cache = self.0.virtual_keys_cache.write().await;
        if let Some(map) = cache.as_mut()
            && remove_virtual_key_entry(map, key_hash)
        {
            self.0.metrics.routers.alephant_api_keys.add(-1, &[]);
        }
    }

    pub async fn set_alephant_api_key(
        &self,
        api_key: Key,
    ) -> Result<Option<HashSet<Key>>, InitError> {
        let mut alephant_api_keys = self.0.alephant_api_keys.write().await;
        alephant_api_keys
            .as_mut()
            .ok_or_else(|| InitError::RouterApiKeysNotInitialized)?
            .insert(api_key.clone());
        self.0.metrics.routers.alephant_api_keys.add(1, &[]);
        Ok(alephant_api_keys.clone())
    }

    pub async fn remove_alephant_api_key(
        &self,
        api_key_hash: String,
    ) -> Result<Option<HashSet<Key>>, InitError> {
        let mut alephant_api_keys = self.0.alephant_api_keys.write().await;
        alephant_api_keys
            .as_mut()
            .ok_or_else(|| InitError::RouterApiKeysNotInitialized)?
            .retain(|k| k.key_hash != api_key_hash);
        self.0.metrics.routers.alephant_api_keys.add(-1, &[]);
        Ok(alephant_api_keys.clone())
    }

    pub async fn set_router_organization_map(
        &self,
        map: HashMap<RouterId, OrgId>,
    ) {
        let mut router_organization_map =
            self.0.router_organization_map.write().await;
        router_organization_map.clone_from(&map);
    }

    pub async fn set_router_organization(
        &self,
        router_id: RouterId,
        organization_id: OrgId,
    ) {
        let mut router_organization_map =
            self.0.router_organization_map.write().await;
        router_organization_map.insert(router_id, organization_id);
    }

    pub async fn get_router_organization(
        &self,
        router_id: &RouterId,
    ) -> Option<OrgId> {
        let router_organization_map =
            self.0.router_organization_map.read().await;
        router_organization_map.get(router_id).copied()
    }

    pub fn increment_router_metrics(
        &self,
        router_id: &RouterId,
        router_config: &RouterConfig,
        organization_id: Option<OrgId>,
    ) {
        let metrics = &self.0.metrics;
        let org_id = organization_id
            .as_ref()
            .map_or_else(|| "unknown".to_string(), ToString::to_string);
        metrics.routers.routers.add(
            1,
            &[
                KeyValue::new("organization_id", org_id.clone()),
                KeyValue::new("router_id", router_id.to_string()),
            ],
        );
        for (endpoint_type, balance_config) in &router_config.load_balance.0 {
            metrics.routers.router_strategies.add(
                1,
                &[
                    KeyValue::new("organization_id", org_id.clone()),
                    KeyValue::new("router_id", router_id.to_string()),
                    KeyValue::new(
                        "endpoint_type",
                        endpoint_type.as_ref().to_string(),
                    ),
                    KeyValue::new(
                        "balance_config",
                        balance_config.as_ref().to_string(),
                    ),
                ],
            );
        }
        if router_config.model_mappings.is_some() {
            metrics
                .routers
                .model_mappings
                .add(1, &[KeyValue::new("router_id", router_id.to_string())]);
        }
        if router_config.retries.is_some() {
            metrics
                .routers
                .retries_enabled
                .add(1, &[KeyValue::new("router_id", router_id.to_string())]);
        }
    }

    pub fn decrement_router_metrics(
        &self,
        router_id: &RouterId,
        router_config: &RouterConfig,
        organization_id: Option<OrgId>,
    ) {
        let metrics = &self.0.metrics;
        let org_id = organization_id
            .as_ref()
            .map_or_else(|| "unknown".to_string(), ToString::to_string);
        metrics.routers.routers.add(
            -1,
            &[
                KeyValue::new("organization_id", org_id.clone()),
                KeyValue::new("router_id", router_id.to_string()),
            ],
        );
        for (endpoint_type, balance_config) in &router_config.load_balance.0 {
            metrics.routers.router_strategies.add(
                1,
                &[
                    KeyValue::new("organization_id", org_id.clone()),
                    KeyValue::new("router_id", router_id.to_string()),
                    KeyValue::new(
                        "endpoint_type",
                        endpoint_type.as_ref().to_string(),
                    ),
                    KeyValue::new(
                        "balance_config",
                        balance_config.as_ref().to_string(),
                    ),
                ],
            );
        }
        if router_config.model_mappings.is_some() {
            metrics
                .routers
                .model_mappings
                .add(1, &[KeyValue::new("router_id", router_id.to_string())]);
        }
        if router_config.retries.is_some() {
            metrics
                .routers
                .retries_enabled
                .add(1, &[KeyValue::new("router_id", router_id.to_string())]);
        }
    }

    pub async fn set_all_provider_keys(
        &self,
        provider_keys: HashMap<OrgId, ProviderKeyMap>,
    ) {
        let num_keys = provider_keys.values().map(|m| m.len()).sum::<usize>();
        self.0
            .metrics
            .routers
            .provider_api_keys
            .add(i64::try_from(num_keys).unwrap_or(i64::MAX), &[]);
        self.0
            .provider_keys
            .set_all_provider_keys(provider_keys)
            .await;
    }

    pub async fn set_org_provider_keys(
        &self,
        org_id: OrgId,
        provider_keys: ProviderKeyMap,
    ) {
        let num_keys = provider_keys.len();
        self.0
            .metrics
            .routers
            .provider_api_keys
            .add(i64::try_from(num_keys).unwrap_or(i64::MAX), &[]);
        self.0
            .provider_keys
            .set_org_provider_keys(org_id, provider_keys)
            .await;
    }
}

fn upsert_virtual_key_entry(
    map: &mut HashMap<String, DbVirtualKey>,
    vk: DbVirtualKey,
) -> bool {
    map.insert(vk.key_hash.clone(), vk).is_none()
}

fn remove_virtual_key_entry(
    map: &mut HashMap<String, DbVirtualKey>,
    key_hash: &str,
) -> bool {
    map.remove(key_hash).is_some()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::{app::build_test_app, config::Config};

    fn sample_vk(key_hash: &str, workspace_id: Uuid) -> DbVirtualKey {
        DbVirtualKey {
            id: Uuid::new_v4(),
            workspace_id,
            master_key_id: Uuid::new_v4(),
            key_hash: key_hash.to_string(),
            key_prefix: "vk-test".to_string(),
            label: "vk-label".to_string(),
            entity_type: Some("user".to_string()),
            entity_id: Some(Uuid::new_v4()),
            status: "active".to_string(),
            expires_at: None,
            deleted_at: None,
            updated_at: Utc::now(),
            rate_limit_rpm: None,
            rate_limit_rph: None,
            allowed_models: None,
            blocked_models: None,
            subscription_log_limit: 90,
        }
    }

    #[test]
    fn upsert_virtual_key_entry_returns_true_only_on_first_insert() {
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let first = sample_vk("kh-1", workspace_id);
        let second = sample_vk("kh-1", workspace_id);

        assert!(upsert_virtual_key_entry(&mut map, first));
        assert!(!upsert_virtual_key_entry(&mut map, second));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn remove_virtual_key_entry_reflects_presence() {
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let vk = sample_vk("kh-2", workspace_id);
        assert!(upsert_virtual_key_entry(&mut map, vk));

        assert!(remove_virtual_key_entry(&mut map, "kh-2"));
        assert!(!remove_virtual_key_entry(&mut map, "kh-2"));
    }

    #[test]
    fn upsert_preserves_rate_limit_fields() {
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let vk = DbVirtualKey {
            rate_limit_rpm: Some(60),
            rate_limit_rph: Some(1000),
            ..sample_vk("kh-rl", workspace_id)
        };
        upsert_virtual_key_entry(&mut map, vk.clone());

        let cached = map.get("kh-rl").expect("entry should be present");
        assert_eq!(cached.rate_limit_rpm, Some(60));
        assert_eq!(cached.rate_limit_rph, Some(1000));
        assert_eq!(cached.id, vk.id);
    }

    #[test]
    fn upsert_preserves_model_policy_fields() {
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let vk = DbVirtualKey {
            allowed_models: Some(vec![
                "GPT-4".to_string(),
                "claude-3".to_string(),
            ]),
            blocked_models: Some(vec!["gpt-3.5-turbo".to_string()]),
            ..sample_vk("kh-mp", workspace_id)
        };
        upsert_virtual_key_entry(&mut map, vk.clone());

        let cached = map.get("kh-mp").expect("entry should be present");
        assert_eq!(
            cached.allowed_models.as_deref(),
            Some(vec!["GPT-4".to_string(), "claude-3".to_string()].as_slice())
        );
        assert_eq!(
            cached.blocked_models.as_deref(),
            Some(vec!["gpt-3.5-turbo".to_string()].as_slice())
        );
    }

    #[test]
    fn upsert_overwrites_policy_on_update() {
        // Simulates db_listener receiving an updated row for an existing key.
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let original = DbVirtualKey {
            rate_limit_rpm: Some(30),
            allowed_models: Some(vec!["gpt-4".to_string()]),
            ..sample_vk("kh-upd", workspace_id)
        };
        upsert_virtual_key_entry(&mut map, original);

        let updated = DbVirtualKey {
            rate_limit_rpm: Some(120),
            allowed_models: Some(vec![
                "gpt-4".to_string(),
                "claude-3-opus".to_string(),
            ]),
            ..sample_vk("kh-upd", workspace_id)
        };
        // Second upsert should overwrite (returns false = key existed).
        assert!(!upsert_virtual_key_entry(&mut map, updated));

        let cached = map.get("kh-upd").expect("entry should be present");
        assert_eq!(cached.rate_limit_rpm, Some(120));
        assert_eq!(
            cached.allowed_models.as_ref().map(std::vec::Vec::len),
            Some(2)
        );
    }

    #[test]
    fn upsert_update_can_clear_optional_policy_and_limit_fields() {
        // DB row update may set nullable fields back to NULL; cache must mirror
        // that by clearing previous values.
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let original = DbVirtualKey {
            rate_limit_rpm: Some(60),
            rate_limit_rph: Some(1200),
            allowed_models: Some(vec!["gpt-4".to_string()]),
            blocked_models: Some(vec!["gpt-3.5-turbo".to_string()]),
            ..sample_vk("kh-clear", workspace_id)
        };
        upsert_virtual_key_entry(&mut map, original);

        let cleared = DbVirtualKey {
            rate_limit_rpm: None,
            rate_limit_rph: None,
            allowed_models: None,
            blocked_models: None,
            ..sample_vk("kh-clear", workspace_id)
        };
        assert!(!upsert_virtual_key_entry(&mut map, cleared));

        let cached = map.get("kh-clear").expect("entry should be present");
        assert!(cached.rate_limit_rpm.is_none());
        assert!(cached.rate_limit_rph.is_none());
        assert!(cached.allowed_models.is_none());
        assert!(cached.blocked_models.is_none());
    }

    #[test]
    fn upsert_null_policy_fields_are_none_after_read() {
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        // rate_limit_* and model lists are NULL in DB => None in cache.
        let vk = sample_vk("kh-null", workspace_id);
        upsert_virtual_key_entry(&mut map, vk);

        let cached = map.get("kh-null").expect("entry should be present");
        assert!(cached.rate_limit_rpm.is_none());
        assert!(cached.rate_limit_rph.is_none());
        assert!(cached.allowed_models.is_none());
        assert!(cached.blocked_models.is_none());
    }

    #[test]
    fn removed_key_no_longer_accessible() {
        // After soft-delete: db_listener calls remove_virtual_key_entry;
        // subsequent auth/policy lookups must see None.
        let mut map = HashMap::default();
        let workspace_id = Uuid::new_v4();
        let vk = DbVirtualKey {
            rate_limit_rpm: Some(60),
            allowed_models: Some(vec!["gpt-4".to_string()]),
            ..sample_vk("kh-del", workspace_id)
        };
        upsert_virtual_key_entry(&mut map, vk);
        remove_virtual_key_entry(&mut map, "kh-del");

        assert!(!map.contains_key("kh-del"));
    }

    #[tokio::test]
    async fn provider_bootstrap_fingerprint_is_empty_by_default() {
        let app = build_test_app(Config::default()).await.expect("build app");

        assert_eq!(app.state.provider_bootstrap_fingerprint(), None);
    }

    #[tokio::test]
    async fn mark_providers_bootstrapped_records_latest_fingerprint() {
        let app = build_test_app(Config::default()).await.expect("build app");

        app.state
            .mark_providers_bootstrapped("fingerprint-a".to_string());
        assert_eq!(
            app.state.provider_bootstrap_fingerprint(),
            Some("fingerprint-a".to_string())
        );

        app.state
            .mark_providers_bootstrapped("fingerprint-b".to_string());
        assert_eq!(
            app.state.provider_bootstrap_fingerprint(),
            Some("fingerprint-b".to_string())
        );
    }

    #[tokio::test]
    async fn providers_config_initializes_from_config_snapshot() {
        let config = Config::default();
        let expected = config.providers.clone();
        let app = build_test_app(config).await.expect("build app");

        assert_eq!(app.state.get_providers_config(), expected);
    }

    #[tokio::test]
    async fn set_providers_config_updates_runtime_snapshot_only() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let mut updated = app.state.get_providers_config();
        let openai_cfg = updated
            .get_mut(&crate::types::provider::InferenceProvider::OpenAI)
            .expect("openai provider exists");
        openai_cfg.base_url = url::Url::parse("https://runtime.override.test")
            .expect("valid url");

        let original_static = app.state.config().providers.clone();
        app.state.set_providers_config(updated.clone());

        assert_eq!(app.state.get_providers_config(), updated);
        assert_eq!(app.state.config().providers, original_static);
        assert_ne!(app.state.get_providers_config(), original_static);
    }

    #[tokio::test]
    async fn get_providers_config_returns_detached_snapshot() {
        let app = build_test_app(Config::default()).await.expect("build app");
        let before = app.state.get_providers_config();

        let mut local = app.state.get_providers_config();
        local
            .get_mut(&crate::types::provider::InferenceProvider::OpenAI)
            .expect("openai provider exists")
            .base_url = url::Url::parse("https://local-only-change.test")
            .expect("valid url");

        // Caller mutations on a returned snapshot must never mutate app state.
        assert_eq!(app.state.get_providers_config(), before);
    }
}
