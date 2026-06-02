use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use meltdown::Token;
use rustc_hash::{FxHashMap, FxHashMap as HashMap};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgListener;
use tokio::time::{Duration, MissedTickBehavior, interval};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::enrichment_touch::EnrichmentTouchScope;
use crate::{
    app_state::AppState,
    discover::router::provider_db_config::build_from_db,
    error::{init::InitError, internal::InternalError, runtime::RuntimeError},
    store::{
        enrichment_redis::enrichment_cache_key,
        router::{DbVirtualKey, RouterStore},
    },
    types::{org::OrgId, user::UserId},
    virtual_key::legacy_key::Key,
};

/// A database listener service that handles LISTEN/NOTIFY functionality.
/// This service runs in the background and can be registered with meltdown.
#[derive(Debug)]
pub struct DatabaseListener {
    app_state: AppState,
    pg_listener: PgListener,
    router_store: RouterStore,
    /// Track last seen API key `created_at` timestamps to detect missed events
    last_api_key_created_at: HashMap<String, DateTime<Utc>>,
    /// Track last seen `updated_at` timestamps for `virtual_keys` rows
    last_virtual_key_updated_at: HashMap<String, DateTime<Utc>>,
    /// Track last seen `updated_at` timestamps for `master_keys` rows
    last_master_key_updated_at: HashMap<Uuid, DateTime<Utc>>,
    /// Polling interval for database queries
    poll_interval: Duration,
    /// Last time we polled the database
    last_poll_time: Option<DateTime<Utc>>,
    /// Interval for reconnecting the listener
    listener_reconnect_interval: Duration,
    /// Last known fingerprint of the enabled provider gateway catalog.
    /// When present, provider polling compares fingerprints so hard deletes
    /// are visible even though they do not update surviving rows.
    provider_fingerprint_baseline: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
enum Op {
    #[serde(rename = "INSERT")]
    Insert,
    #[serde(rename = "UPDATE")]
    Update,
    #[serde(rename = "DELETE")]
    Delete,
    #[serde(rename = "TRUNCATE")]
    Truncate,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum ConnectedCloudGatewaysNotification {
    /// Legacy: routes are now derived from `providers/provider_models`; this
    /// variant is deserialized but immediately ignored.
    RouterConfigUpdated {
        #[serde(flatten)]
        _data: serde_json::Value,
    },
    ApiKeyUpdated {
        owner_id: UserId,
        organization_id: OrgId,
        api_key_hash: String,
        soft_delete: bool,
        op: Op,
    },
    EnrichmentTouch {
        scope: EnrichmentTouchScope,
        id: Uuid,
    },
    Unknown {
        #[serde(flatten)]
        data: serde_json::Value,
    },
}

/// Service state to correctly handle cancellation safety
enum ServiceState {
    Idle,
    PollingDatabase,
    Reconnecting,
    HandlingNotification(sqlx::postgres::PgNotification),
}

enum VirtualKeyUpdateAction {
    Upsert(Box<DbVirtualKey>),
    Remove(String),
    Skip,
}

impl DatabaseListener {
    pub async fn new(
        database_url: &str,
        app_state: AppState,
    ) -> Result<Self, InitError> {
        let pg_listener =
            PgListener::connect(database_url).await.map_err(|e| {
                error!(error = %e, "failed to create database listener");
                InitError::DatabaseConnection(e)
            })?;

        let db_poll_interval =
            app_state.config().deployment_target.db_poll_interval;
        let listener_reconnect_interval = app_state
            .config()
            .deployment_target
            .listener_reconnect_interval;

        let router_store = app_state
            .0
            .router_store
            .as_ref()
            .ok_or(InitError::StoreNotConfigured("router_store"))?
            .clone();
        let provider_fingerprint_baseline =
            app_state.provider_bootstrap_fingerprint();

        Ok(Self {
            app_state,
            pg_listener,
            router_store,
            last_api_key_created_at: HashMap::default(),
            last_virtual_key_updated_at: HashMap::default(),
            last_master_key_updated_at: HashMap::default(),
            poll_interval: db_poll_interval,
            last_poll_time: None,
            listener_reconnect_interval,
            provider_fingerprint_baseline,
        })
    }

    /// Poll the database for changes since last poll
    #[allow(clippy::too_many_lines)]
    async fn poll_database(&mut self) -> Result<(), RuntimeError> {
        let start = Utc::now();
        info!("polling database for changes");

        // API key (alephant) updates: not polled — we use virtual_keys only.
        // Legacy upstream api-keys table does not exist in Alephant schema;
        // NOTIFY ApiKeyUpdated is still handled below for backward compat.

        // -----------------------------------------------------------
        // Poll virtual_keys for incremental changes.
        //
        // `get_db_virtual_keys_updated_after` projects the full `DbVirtualKey`
        // struct including `rate_limit_rpm`, `rate_limit_rph`,
        // `allowed_models`, and `blocked_models`.  Any update to these fields
        // in the DB is therefore automatically picked up here and upserted into
        // the in-process `virtual_keys_cache` via `set_virtual_key`.  No
        // secondary refresh mechanism is needed for model-policy or rate-limit
        // configuration.
        // -----------------------------------------------------------
        let new_virtual_keys = if let Some(last_poll) = self.last_poll_time {
            self.router_store
                .get_db_virtual_keys_updated_after(last_poll)
                .await
                .inspect(|vks| {
                    debug!("polling found {} updated virtual_keys", vks.len());
                })
                .inspect_err(|e| {
                    error!(error = %e, "failed to poll virtual_keys");
                })?
        } else {
            Vec::new()
        };

        for vk in new_virtual_keys {
            let last_seen = self.last_virtual_key_updated_at.get(&vk.key_hash);
            match classify_virtual_key_update(last_seen, vk) {
                VirtualKeyUpdateAction::Upsert(vk) => {
                    let key_hash = vk.key_hash.clone();
                    let updated_at = vk.updated_at;
                    self.app_state.set_virtual_key(*vk).await;
                    self.last_virtual_key_updated_at
                        .insert(key_hash, updated_at);
                }
                VirtualKeyUpdateAction::Remove(key_hash) => {
                    self.app_state.remove_virtual_key(&key_hash).await;
                    self.last_virtual_key_updated_at.remove(&key_hash);
                }
                VirtualKeyUpdateAction::Skip => {}
            }
        }

        // -----------------------------------------------------------
        // Poll master_keys for incremental changes (cache invalidation)
        // -----------------------------------------------------------
        let updated_master_keys = if let Some(last_poll) = self.last_poll_time {
            self.router_store
                .get_master_keys_updated_after(last_poll)
                .await
                .inspect(|rows| {
                    debug!("polling found {} updated master_keys", rows.len());
                })
                .inspect_err(|e| {
                    error!(error = %e, "failed to poll master_keys");
                })?
        } else {
            Vec::new()
        };

        for row in updated_master_keys {
            let should_process =
                match self.last_master_key_updated_at.get(&row.id) {
                    None => true,
                    Some(last_seen) => row.updated_at > *last_seen,
                };

            if should_process {
                if let Some(cache) = self.app_state.0.master_key_cache.as_ref()
                {
                    cache.invalidate(row.id);
                }
                self.last_master_key_updated_at
                    .insert(row.id, row.updated_at);
            }
        }

        // -----------------------------------------------------------
        // Poll providers / provider_models for Cloud route hot-updates
        // -----------------------------------------------------------
        let mut current_provider_fingerprint = None;
        let provider_probe = if let Some(provider_fingerprint_baseline) =
            self.provider_fingerprint_baseline.as_deref()
        {
            match self.router_store.provider_gateway_fingerprint().await {
                Ok(current) => {
                    let changed = current != provider_fingerprint_baseline;
                    current_provider_fingerprint = Some(current);
                    Some(changed)
                }
                Err(e) => {
                    error!(
                        error = %e,
                        "failed to fingerprint provider gateway catalog; forcing provider reload"
                    );
                    Some(true)
                }
            }
        } else if let Some(last_poll) = self.last_poll_time {
            Some(
                self.router_store
                    .has_providers_updated_since(last_poll)
                    .await
                    .inspect_err(|e| {
                        error!(error = %e, "failed to check provider updates");
                    })
                    .unwrap_or(false),
            )
        } else {
            None
        };
        let should_reload_providers = should_reload_providers(
            self.last_poll_time,
            provider_probe,
            self.provider_fingerprint_baseline.is_some(),
        );

        if should_reload_providers {
            match self.reload_providers().await {
                Ok(()) => {
                    if let Some(current) = current_provider_fingerprint.take() {
                        self.provider_fingerprint_baseline = Some(current);
                    } else if self.provider_fingerprint_baseline.is_some() {
                        match self
                            .router_store
                            .provider_gateway_fingerprint()
                            .await
                        {
                            Ok(current) => {
                                self.provider_fingerprint_baseline =
                                    Some(current);
                            }
                            Err(e) => {
                                error!(
                                    error = %e,
                                    "failed to refresh provider gateway fingerprint after reload"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "failed to reload providers from DB");
                }
            }
        } else if let Some(current) = current_provider_fingerprint.take() {
            self.provider_fingerprint_baseline = Some(current);
        }

        // -----------------------------------------------------------
        // Poll provider_configs for workspace allowlist hot-updates (F-10)
        // -----------------------------------------------------------
        let provider_configs_changed = if let Some(last_poll) =
            self.last_poll_time
        {
            self.router_store
                .has_provider_configs_updated_since(last_poll)
                .await
                .inspect_err(|e| {
                    error!(error = %e, "failed to check provider_configs updates");
                })
                .unwrap_or(false)
        } else {
            // First poll: always reload.
            true
        };

        if provider_configs_changed {
            self.reload_workspace_provider_allowlist().await;
        }

        let end = Utc::now();
        self.last_poll_time = Some(end);
        info!(
            poll_duration_ms = (end - start).num_milliseconds(),
            "database polling complete"
        );
        Ok(())
    }

    /// Runs the database listener service.
    /// This includes listening for notifications and handling
    /// connection health.
    async fn run_service(&mut self) -> Result<(), RuntimeError> {
        info!("performing initial database poll");
        // Do an initial poll to populate the state
        if let Err(e) = self.poll_database().await {
            error!(error = %e, "error during initial database poll");
        }

        if !self.app_state.is_cache_warmed() {
            self.app_state.mark_cache_warmed();
        }

        self.pg_listener
            .listen("connected_cloud_gateways")
            .await
            .map_err(|e| {
                error!(error = %e, "failed to listen on database notification channel");
                InitError::DatabaseConnection(e)
            })?;

        let mut poll_interval = interval(self.poll_interval);
        poll_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut reconnect_interval = interval(self.listener_reconnect_interval);
        reconnect_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let mut state = ServiceState::Idle;

        // Process notifications and polls
        loop {
            match state {
                ServiceState::Idle => {
                    tokio::select! {
                        biased;
                        notification_result = self.pg_listener.recv() => {
                            match notification_result {
                                Ok(notification) => {
                                    state = ServiceState::HandlingNotification(notification);
                                }
                                Err(e) => {
                                    error!(error = %e, "error receiving from listener, continuing");
                                    // we will continue to receive updates as the next call to recv() will
                                    // reconnect for us eagerly, additionally we have the db polling and
                                    // the periodic reconnection that will catch up on any missed events
                                }
                            }
                        }

                        _ = poll_interval.tick() => {
                            state = ServiceState::PollingDatabase;
                        }

                        _ = reconnect_interval.tick() => {
                            state = ServiceState::Reconnecting;
                        }
                    }
                }
                ServiceState::PollingDatabase => {
                    // This runs outside select!, so it can't be cancelled by
                    // other branches
                    if let Err(e) = self.poll_database().await {
                        error!(error = %e, "error polling database");
                    }
                    state = ServiceState::Idle;
                }
                ServiceState::HandlingNotification(notification) => {
                    // This runs outside select!, so it can't be cancelled by
                    // other branches
                    if let Err(e) =
                        self.handle_notification(&notification).await
                    {
                        error!(error = %e, "failed to handle db listener notification, continuing");
                    }
                    state = ServiceState::Idle;
                }
                ServiceState::Reconnecting => {
                    info!("periodic reconnection");
                    // This runs outside select!, so it can't be cancelled by
                    // other branches
                    if let Err(e) = self.pg_listener.unlisten_all().await {
                        error!(error = %e, "failed to unlisten all channels");
                    }
                    if let Err(e) = self
                        .pg_listener
                        .listen("connected_cloud_gateways")
                        .await
                    {
                        error!(error = %e, "failed to listen on channel after reconnection");
                    } else {
                        info!(
                            "successfully reconnected and listening on channel"
                        );
                    }
                    state = ServiceState::Idle;
                }
            }
        }
    }

    /// Re-fetches `providers` + `provider_models` from DB and updates the
    /// provider catalog in AppState.
    async fn reload_providers(&mut self) -> Result<(), RuntimeError> {
        let db_providers = self
            .router_store
            .get_all_providers_for_gateway()
            .await
            .map_err(|e| {
                error!(error = %e, "reload_providers: failed to fetch providers");
                RuntimeError::Internal(InternalError::Internal)
            })?;
        let db_models = self
            .router_store
            .get_all_provider_models_for_gateway()
            .await
            .map_err(|e| {
                error!(error = %e, "reload_providers: failed to fetch provider_models");
                RuntimeError::Internal(InternalError::Internal)
            })?;

        let (providers_config, bare_model_expand) =
            build_from_db(&db_providers, &db_models);

        let flags: FxHashMap<String, bool> = db_providers
            .iter()
            .map(|p| (p.code.clone(), p.is_router))
            .collect();
        self.app_state.set_provider_is_router_flags(flags);

        // Update the live ProvidersConfig in AppState.
        self.app_state.set_providers_config(providers_config);
        self.app_state
            .set_bare_model_expand_index(bare_model_expand);

        info!(
            providers = db_providers.len(),
            models = db_models.len(),
            "reload providers catalog succeed"
        );
        Ok(())
    }

    /// Re-fetches the workspace provider allowlist from `provider_configs` and
    /// updates the in-memory snapshot in `AppState`.
    async fn reload_workspace_provider_allowlist(&mut self) {
        match self.router_store.get_workspace_provider_allowlist().await {
            Ok(allowlist) => {
                info!(
                    workspaces = allowlist.len(),
                    "reload_workspace_provider_allowlist: updated"
                );
                self.app_state.set_workspace_provider_allowlist(allowlist);
            }
            Err(e) => {
                error!(
                    error = %e,
                    "reload_workspace_provider_allowlist: failed to fetch, \
                     keeping existing snapshot"
                );
            }
        }
    }

    /// Handles incoming database notifications.
    #[allow(clippy::too_many_lines)]
    async fn handle_notification(
        &mut self,
        notification: &sqlx::postgres::PgNotification,
    ) -> Result<(), RuntimeError> {
        info!(channel = notification.channel(), "processing notification");

        if notification.channel() == "connected_cloud_gateways" {
            let payload = serde_json::from_str::<
                ConnectedCloudGatewaysNotification,
            >(notification.payload()).map_err(|e| {
                error!(error = %e, "failed to parse connected_cloud_gateways payload");
                InternalError::Deserialize {
                    ty: "ConnectedCloudGatewaysNotification",
                    error: e,
                }
            })?;

            match payload {
                ConnectedCloudGatewaysNotification::RouterConfigUpdated {
                    ..
                } => {
                    // Cloud provider catalog changes are detected on the next
                    // poll cycle via has_providers_updated_since(). The
                    // routers table is no longer used for provider catalog
                    // updates, so RouterConfigUpdated notifications are
                    // ignored here.
                    debug!(
                        "RouterConfigUpdated NOTIFY ignored (provider catalog \
                         changes are detected by polling)"
                    );
                    Ok(())
                }
                ConnectedCloudGatewaysNotification::ApiKeyUpdated {
                    owner_id,
                    organization_id,
                    api_key_hash,
                    soft_delete,
                    op,
                } => match op {
                    Op::Insert => {
                        self.app_state
                            .set_alephant_api_key(Key {
                                key_hash: api_key_hash.clone(),
                                owner_id,
                                organization_id,
                            })
                            .await
                            .map_err(|e| {
                                error!(error = %e, "failed to set alephant api key");
                                e
                            })?;
                        info!(
                            owner_id = %owner_id,
                            organization_id = %organization_id,
                            "alephant api key inserted"
                        );
                        // Update state tracking
                        self.last_api_key_created_at
                            .insert(api_key_hash, Utc::now());
                        Ok(())
                    }
                    Op::Delete => {
                        // This case should theoretically never happen, since we
                        // update the soft delete flag when we delete an api
                        // key. However, we'll handle it
                        // just in case.
                        self.app_state
                            .remove_alephant_api_key(api_key_hash.clone())
                            .await
                            .map_err(|e| {
                                error!(error = %e, "failed to remove alephant api key");
                                e
                            })?;
                        info!(
                            owner_id = %owner_id,
                            organization_id = %organization_id,
                            "alephant api key removed"
                        );
                        // Remove from state tracking
                        self.last_api_key_created_at.remove(&api_key_hash);
                        Ok(())
                    }
                    Op::Update => {
                        if soft_delete {
                            self.app_state
                                .remove_alephant_api_key(api_key_hash.clone())
                                .await
                                .map_err(|e| {
                                    error!(error = %e, "failed to remove alephant api key");
                                    e
                                })?;
                            info!(
                                owner_id = %owner_id,
                                organization_id = %organization_id,
                                "alephant api key soft deleted"
                            );
                            // Remove from state tracking when soft deleted
                            self.last_api_key_created_at.remove(&api_key_hash);
                        } else {
                            // Update state tracking for non-soft-delete updates
                            self.last_api_key_created_at
                                .insert(api_key_hash, Utc::now());
                        }
                        Ok(())
                    }
                    Op::Truncate => {
                        debug!("skipping alephant api key truncate");
                        Ok(())
                    }
                },
                ConnectedCloudGatewaysNotification::EnrichmentTouch {
                    scope,
                    id,
                } => {
                    let ids = self
                        .router_store
                        .list_virtual_key_ids_for_enrichment_touch(scope, id)
                        .await
                        .map_err(|e| {
                            error!(
                                error = %e,
                                ?scope,
                                %id,
                                "enrichment_touch: failed to list virtual key ids"
                            );
                            RuntimeError::Internal(e)
                        })?;
                    if let Some(redis) = self.app_state.redis() {
                        for vk_id in ids {
                            let key = enrichment_cache_key(vk_id);
                            if let Err(e) = redis.del(&key).await {
                                warn!(
                                    error = %e,
                                    %vk_id,
                                    "enrichment_touch: redis del failed"
                                );
                            }
                        }
                    }
                    Ok(())
                }
                ConnectedCloudGatewaysNotification::Unknown { data } => {
                    debug!("Unknown notification event");
                    debug!("data: {:?}", data);
                    // TODO: Handle unknown event
                    Ok(())
                }
            }
        } else {
            debug!("received unknown notification");
            Ok(())
        }
    }
}

fn classify_virtual_key_update(
    last_seen: Option<&DateTime<Utc>>,
    vk: DbVirtualKey,
) -> VirtualKeyUpdateAction {
    let should_process = match last_seen {
        None => true,
        Some(last_seen) => vk.updated_at > *last_seen,
    };
    if !should_process {
        return VirtualKeyUpdateAction::Skip;
    }

    if vk.deleted_at.is_some() || vk.status != "active" {
        return VirtualKeyUpdateAction::Remove(vk.key_hash);
    }

    VirtualKeyUpdateAction::Upsert(Box::new(vk))
}

fn should_reload_providers(
    last_poll_time: Option<DateTime<Utc>>,
    provider_probe: Option<bool>,
    provider_bootstrapped: bool,
) -> bool {
    if last_poll_time.is_some() {
        return provider_probe.unwrap_or(false);
    }

    if provider_bootstrapped {
        return provider_probe.unwrap_or(false);
    }

    // First poll without initial provider discovery remains the safe fallback.
    true
}

impl meltdown::Service for DatabaseListener {
    type Future = BoxFuture<'static, Result<(), RuntimeError>>;

    fn run(mut self, mut token: Token) -> Self::Future {
        Box::pin(async move {
            tokio::select! {
                biased;
                result = self.run_service() => {
                    if let Err(e) = result {
                        error!(error = %e, "database listener service encountered error, shutting down");
                    } else {
                        debug!("database listener service shut down successfully");
                    }
                    token.trigger();
                }
                () = &mut token => {
                    debug!("database listener service shutdown signal received");
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::store::router::DbVirtualKey;

    fn sample_vk(key_hash: &str, updated_at: DateTime<Utc>) -> DbVirtualKey {
        DbVirtualKey {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            master_key_id: Uuid::new_v4(),
            key_hash: key_hash.to_string(),
            key_prefix: "vk-test".to_string(),
            label: "vk-label".to_string(),
            entity_type: Some("user".to_string()),
            entity_id: Some(Uuid::new_v4()),
            status: "active".to_string(),
            expires_at: None,
            deleted_at: None,
            updated_at,
            rate_limit_rpm: None,
            rate_limit_rph: None,
            allowed_models: None,
            blocked_models: None,
            subscription_log_limit: 90,
        }
    }

    #[test]
    fn should_reload_providers_true_on_first_poll_without_bootstrap() {
        assert!(should_reload_providers(None, None, false));
        assert!(should_reload_providers(None, Some(false), false));
        assert!(should_reload_providers(None, Some(true), false));
    }

    #[test]
    fn should_reload_providers_uses_bootstrap_probe_on_first_poll() {
        assert!(should_reload_providers(None, Some(true), true));
        assert!(!should_reload_providers(None, Some(false), true));
    }

    #[test]
    fn should_reload_providers_defaults_false_on_first_poll_with_bootstrap_and_missing_probe()
     {
        assert!(!should_reload_providers(None, None, true));
    }

    #[test]
    fn should_reload_providers_follows_last_poll_probe_after_first_poll() {
        let last_poll = Some(
            DateTime::from_timestamp(1_700_000_100, 0)
                .expect("valid timestamp"),
        );
        assert!(should_reload_providers(last_poll, Some(true), true));
        assert!(should_reload_providers(last_poll, Some(true), false));
        assert!(!should_reload_providers(last_poll, Some(false), true));
        assert!(!should_reload_providers(last_poll, Some(false), false));
    }

    #[test]
    fn should_reload_providers_defaults_false_after_first_poll_when_probe_missing()
     {
        let last_poll = Some(
            DateTime::from_timestamp(1_700_000_100, 0)
                .expect("valid timestamp"),
        );
        assert!(!should_reload_providers(last_poll, None, true));
        assert!(!should_reload_providers(last_poll, None, false));
    }

    #[test]
    fn classify_virtual_key_update_upserts_newer_active_row_with_fields_intact()
    {
        let now = Utc::now();
        let last_seen = now - chrono::Duration::seconds(1);
        let mut vk = sample_vk("kh-upsert", now);
        vk.rate_limit_rpm = Some(120);
        vk.rate_limit_rph = Some(5000);
        vk.allowed_models = Some(vec!["gpt-4".to_string()]);
        vk.blocked_models = Some(vec!["gpt-3.5-turbo".to_string()]);

        match classify_virtual_key_update(Some(&last_seen), vk) {
            VirtualKeyUpdateAction::Upsert(vk) => {
                assert_eq!(vk.rate_limit_rpm, Some(120));
                assert_eq!(vk.rate_limit_rph, Some(5000));
                assert_eq!(
                    vk.allowed_models.as_deref(),
                    Some(vec!["gpt-4".to_string()].as_slice())
                );
                assert_eq!(
                    vk.blocked_models.as_deref(),
                    Some(vec!["gpt-3.5-turbo".to_string()].as_slice())
                );
            }
            _ => panic!("expected upsert action"),
        }
    }

    #[test]
    fn classify_virtual_key_update_removes_deleted_or_inactive_rows() {
        let now = Utc::now();
        let mut deleted = sample_vk("kh-del", now);
        deleted.deleted_at = Some(now);
        match classify_virtual_key_update(None, deleted) {
            VirtualKeyUpdateAction::Remove(key) => assert_eq!(key, "kh-del"),
            _ => panic!("expected remove action for deleted row"),
        }

        let mut inactive = sample_vk("kh-inactive", now);
        inactive.status = "inactive".to_string();
        match classify_virtual_key_update(None, inactive) {
            VirtualKeyUpdateAction::Remove(key) => {
                assert_eq!(key, "kh-inactive");
            }
            _ => panic!("expected remove action for inactive row"),
        }
    }

    #[test]
    fn classify_virtual_key_update_skips_stale_or_equal_timestamps() {
        let now = Utc::now();
        let older = now - chrono::Duration::seconds(5);
        let equal = now;
        let last_seen = now;

        let vk_older = sample_vk("kh-old", older);
        assert!(matches!(
            classify_virtual_key_update(Some(&last_seen), vk_older),
            VirtualKeyUpdateAction::Skip
        ));

        let vk_equal = sample_vk("kh-eq", equal);
        assert!(matches!(
            classify_virtual_key_update(Some(&last_seen), vk_equal),
            VirtualKeyUpdateAction::Skip
        ));
    }

    #[test]
    fn deserialize_enrichment_touch_notification() {
        let j = r#"{"event":"enrichment_touch","scope":"agent","id":"550e8400-e29b-41d4-a716-446655440000"}"#;
        let n: ConnectedCloudGatewaysNotification =
            serde_json::from_str(j).unwrap();
        match n {
            ConnectedCloudGatewaysNotification::EnrichmentTouch {
                scope,
                id,
            } => {
                assert_eq!(
                    scope,
                    crate::store::enrichment_touch::EnrichmentTouchScope::Agent
                );
                assert_eq!(
                    id,
                    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000")
                        .unwrap()
                );
            }
            _ => panic!("wrong variant"),
        }
    }
}
