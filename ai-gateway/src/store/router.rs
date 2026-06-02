use std::collections::HashSet;

use chrono::{DateTime, Utc};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{error, warn};
use uuid::Uuid;

use crate::{
    default_model::POLICY_NAME_MODEL_ALLOWLIST,
    error::{init::InitError, internal::InternalError},
    store::enrichment_touch::EnrichmentTouchScope,
    types::{
        org::OrgId,
        provider::{InferenceProvider, ProviderKey, ProviderKeyMap},
        secret::Secret,
        user::UserId,
    },
    virtual_key::legacy_key::Key,
};

// ---------------------------------------------------------------------------
// DbMasterKeyRow — minimal projection from master_keys JOIN providers
// ---------------------------------------------------------------------------

/// Row returned by `get_master_key_row`, carrying the encrypted key material
/// and the provider code needed to map to [`InferenceProvider`].
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbMasterKeyRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// Ciphertext bytes (PG `bytea`).
    pub key_ciphertext: Vec<u8>,
    /// Nonce bytes (PG `bytea`, 12 bytes for AES-256-GCM).
    pub key_nonce: Vec<u8>,
    /// Optional custom base URL override for this provider key.
    pub base_url: Option<String>,
    /// `master_key_status_enum` mapped to text.
    pub status: String,
    pub updated_at: DateTime<Utc>,
    /// `providers.code` — used to map to [`InferenceProvider`].
    pub provider_code: String,
}

/// Minimal row for polling master key updates.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbMasterKeyUpdateRow {
    pub id: Uuid,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// DbVirtualKey — projection from the `virtual_keys` table (Cloud cache)
// ---------------------------------------------------------------------------

/// Row fetched from the `virtual_keys` table.
///
/// Includes auth / master-key fields plus policy and per-key rate limits used
/// by Cloud mode (`allowed_models`, `blocked_models`, `rate_limit_*`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbVirtualKey {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub master_key_id: Uuid,
    pub key_hash: String,
    /// `virtual_keys.key_prefix` (varchar, NOT NULL in PG).
    pub key_prefix: String,
    /// Human-readable key label (e.g. `member:…`); maps to logs `entity_name`.
    pub label: String,
    /// `vk_entity_type_enum` mapped to text by the query.
    #[sqlx(default)]
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
    /// `virtual_key_status_enum` mapped to text by the query.
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub rate_limit_rpm: Option<i32>,
    pub rate_limit_rph: Option<i32>,
    pub allowed_models: Option<Vec<String>>,
    pub blocked_models: Option<Vec<String>>,
    /// Active workspace `subscriptions.log_limit`, for RMT `body_ttl_days`
    /// (clamped when applied). Defaults to 90 when no active subscription row.
    pub subscription_log_limit: i32,
}

#[derive(Debug, Clone)]
pub struct RouterStore {
    pub pool: PgPool,
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DbX402PaymentActivityLogFields {
    pub ale_receive_wallet_address: Option<String>,
    pub fee_wallet_address: Option<String>,
    pub ai_cost: Option<f64>,
    pub trace_status: Option<String>,
    pub settled_at: Option<DateTime<Utc>>,
    pub available_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbRouterConfig {
    pub router_hash: String,
    pub organization_id: Uuid,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// DbGatewayProvider / DbGatewayProviderModel
// — rows from the new providers / provider_models tables used in Cloud mode
// ---------------------------------------------------------------------------

/// Row from `providers` table, used to build `ProvidersConfig` at runtime.
#[derive(Debug, sqlx::FromRow)]
pub struct DbGatewayProvider {
    pub id: Uuid,
    pub code: String,
    /// Optional override for the provider's base URL.  When `None`, the
    /// embedded YAML default is used.
    pub default_base_url: Option<String>,
    /// Optional Mainland China endpoint for regional endpoint retry.
    pub cn_base_url: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// DB `providers.is_router`: aggregator targets skip runtime catalog
    /// mapping.
    pub is_router: bool,
}

/// Row from `provider_models` table.
#[derive(Debug, sqlx::FromRow)]
pub struct DbGatewayProviderModel {
    pub provider_id: Uuid,
    pub model_id: String,
}

/// One enabled model row with non-null `info` (for queries/tools). Column
/// semantics: see DB COMMENT.
#[derive(Debug, sqlx::FromRow)]
pub struct DbGatewayModelInfoWarmupRow {
    pub provider_code: String,
    pub model_id: String,
    pub info: sqlx::types::Json<serde_json::Value>,
}

#[derive(Debug, sqlx::FromRow)]
struct DbGatewayModelInfoRow {
    info: Option<sqlx::types::Json<serde_json::Value>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DbGatewayModelSelectionInfoRow {
    pub provider_code: String,
    pub model_id: String,
    pub info: Option<sqlx::types::Json<serde_json::Value>>,
}

#[derive(Debug, sqlx::FromRow)]
struct DbGatewayModelSelectionInfoBatchRow {
    ord: i64,
    provider_code: String,
    model_id: String,
    info: Option<sqlx::types::Json<serde_json::Value>>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbApiKey {
    pub key_hash: String,
    pub owner_id: Uuid,
    pub organization_id: Uuid,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[sqlx(default)]
    pub soft_delete: Option<bool>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct DbProviderKey {
    pub provider_name: String,
    pub decrypted_provider_key: String,
    pub org_id: Uuid,
    pub config: Option<serde_json::Value>,
}

/// `policy_configs` row (`name = 'Model Allowlist'`, see
/// `default_model::POLICY_NAME_MODEL_ALLOWLIST`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbPolicyConfigModelAccessRow {
    pub config: sqlx::types::Json<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

/// `policy_overrides` row. Production DB scopes by `(workspace_id,
/// department_id)` (see `alephant_dev`, etc.;
/// `docs/sql/migrations/20260423_policy_overrides.sql` is a compatibility
/// migration for nullable `virtual_key_id`; pick one alignment with this query
/// path).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbPolicyOverrideRow {
    pub overrides: sqlx::types::Json<serde_json::Value>,
    pub updated_at: DateTime<Utc>,
}

const GET_MASTER_KEY_IDS_BY_WORKSPACE_AND_PROVIDER_SQL: &str = r"SELECT mk.id
              FROM master_keys mk
              JOIN providers p ON p.id = mk.provider_id AND p.code = $2
              WHERE mk.workspace_id = $1
                AND mk.deleted_at IS NULL
                AND mk.status = 'active'";

const GET_ALL_DB_VIRTUAL_KEYS_SQL: &str = r"SELECT
                vk.id,
                vk.workspace_id,
                vk.master_key_id,
                vk.key_hash,
                vk.key_prefix,
                vk.label,
                vk.entity_type::text  AS entity_type,
                vk.entity_id,
                vk.status::text       AS status,
                vk.expires_at,
                vk.deleted_at,
                vk.updated_at,
                vk.rate_limit_rpm,
                vk.rate_limit_rph,
                vk.allowed_models,
                vk.blocked_models,
                COALESCE(
                  (
                    SELECT s.log_limit
                    FROM subscriptions s
                    WHERE s.workspace_id = vk.workspace_id
                      AND s.status = 'active'
                    ORDER BY s.current_period_end DESC
                    LIMIT 1
                  ),
                  90
                )                     AS subscription_log_limit
              FROM virtual_keys vk
              WHERE vk.deleted_at IS NULL
                AND vk.status = 'active'";

const GET_DB_VIRTUAL_KEY_BY_KEY_HASH_SQL: &str = r"SELECT
                vk.id,
                vk.workspace_id,
                vk.master_key_id,
                vk.key_hash,
                vk.key_prefix,
                vk.label,
                vk.entity_type::text  AS entity_type,
                vk.entity_id,
                vk.status::text       AS status,
                vk.expires_at,
                vk.deleted_at,
                vk.updated_at,
                vk.rate_limit_rpm,
                vk.rate_limit_rph,
                vk.allowed_models,
                vk.blocked_models,
                COALESCE(
                  (
                    SELECT s.log_limit
                    FROM subscriptions s
                    WHERE s.workspace_id = vk.workspace_id
                      AND s.status = 'active'
                    ORDER BY s.current_period_end DESC
                    LIMIT 1
                  ),
                  90
                )                     AS subscription_log_limit
              FROM virtual_keys vk
              WHERE vk.key_hash = $1
                AND vk.deleted_at IS NULL
                AND vk.status = 'active'
              LIMIT 1";

const GET_DB_VIRTUAL_KEYS_UPDATED_AFTER_SQL: &str = r"SELECT
                vk.id,
                vk.workspace_id,
                vk.master_key_id,
                vk.key_hash,
                vk.key_prefix,
                vk.label,
                vk.entity_type::text  AS entity_type,
                vk.entity_id,
                vk.status::text       AS status,
                vk.expires_at,
                vk.deleted_at,
                vk.updated_at,
                vk.rate_limit_rpm,
                vk.rate_limit_rph,
                vk.allowed_models,
                vk.blocked_models,
                COALESCE(
                  (
                    SELECT s.log_limit
                    FROM subscriptions s
                    WHERE s.workspace_id = vk.workspace_id
                      AND s.status = 'active'
                    ORDER BY s.current_period_end DESC
                    LIMIT 1
                  ),
                  90
                )                     AS subscription_log_limit
              FROM virtual_keys vk
              WHERE vk.updated_at > $1";

const GET_POLICY_CONFIG_MODEL_ACCESS_SQL: &str = r"
SELECT config, updated_at
FROM policy_configs
WHERE workspace_id = $1
  AND name = $2
  AND enabled = true
ORDER BY priority DESC, updated_at DESC
LIMIT 1";

/// `$1` = `workspace_id`, `$2` = `virtual_keys.id`; resolve `department_id`
/// from VK agent/member rows, then join `policy_overrides` (same resolution
/// rules as `FETCH_REQUEST_LOG_DEPARTMENT_ENRICHMENT_SQL`).
const GET_POLICY_OVERRIDES_BY_VIRTUAL_KEY_SQL: &str = r"
SELECT po.overrides, po.updated_at
FROM policy_overrides po
INNER JOIN virtual_keys vk
  ON vk.id = $2
  AND vk.workspace_id = $1
  AND vk.deleted_at IS NULL
LEFT JOIN agents a
  ON vk.entity_type = 'agent'::vk_entity_type_enum
  AND vk.entity_id = a.id
  AND a.deleted_at IS NULL
LEFT JOIN members m
  ON vk.entity_type = 'member'::vk_entity_type_enum
  AND vk.entity_id = m.id
  AND m.deleted_at IS NULL
WHERE po.workspace_id = $1
  AND (
    (vk.entity_type = 'agent'::vk_entity_type_enum
      AND a.department_id IS NOT NULL
      AND po.department_id = a.department_id)
    OR
    (vk.entity_type = 'member'::vk_entity_type_enum
      AND m.department_id IS NOT NULL
      AND po.department_id = m.department_id)
  )
ORDER BY po.updated_at DESC
LIMIT 1";

/// Workspace fallback when no per-VK/department match: latest
/// `policy_overrides` row for the workspace (for DBs without `virtual_key_id IS
/// NULL` semantics).
const GET_POLICY_OVERRIDES_WORKSPACE_DEFAULT_SQL: &str = r"
SELECT overrides, updated_at
FROM policy_overrides
WHERE workspace_id = $1
ORDER BY updated_at DESC
LIMIT 1";

/// Exact `policy_overrides` lookup by `workspace_id` + `department_id` (matches
/// `uq_policy_overrides_workspace_department`).
const GET_POLICY_OVERRIDES_BY_WORKSPACE_AND_DEPARTMENT_SQL: &str = r"
SELECT overrides, updated_at
FROM policy_overrides
WHERE workspace_id = $1
  AND department_id = $2
ORDER BY updated_at DESC
LIMIT 1";

/// One-row enrichment for [`crate::types::logger::RequestLog`]: department
/// resolved from `virtual_keys` → `workspaces` → `agents` / `members` →
/// `departments` in a single query (any workspace `type`; not limited to
/// enterprise).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DbRequestLogDepartmentEnrichment {
    /// `workspaces.type::text` (e.g. `personal`, `team`, `enterprise`).
    pub workspace_type: String,
    /// `agents.department_id` or `members.department_id` when the VK row
    /// targets that entity.
    pub department_id: Option<Uuid>,
    /// `departments.name` when a row matches.
    pub department_name: Option<String>,
}

const FETCH_REQUEST_LOG_DEPARTMENT_ENRICHMENT_SQL: &str = r"
SELECT
  ws.type::text AS workspace_type,
  CASE
    WHEN vk.entity_type::text = 'agent' THEN a.department_id
    WHEN vk.entity_type::text = 'member' THEN m.department_id
    ELSE NULL
  END AS department_id,
  (
    SELECT dep.name
    FROM departments dep
    WHERE dep.id = (
      CASE
        WHEN vk.entity_type::text = 'agent' THEN a.department_id
        WHEN vk.entity_type::text = 'member' THEN m.department_id
        ELSE NULL
      END
    )
    ORDER BY (dep.deleted_at IS NULL) DESC, dep.updated_at DESC
    LIMIT 1
  ) AS department_name
FROM virtual_keys vk
INNER JOIN workspaces ws
  ON ws.id = vk.workspace_id AND ws.deleted_at IS NULL
LEFT JOIN agents a
  ON vk.entity_type = 'agent'::vk_entity_type_enum
  AND vk.entity_id = a.id
  AND a.deleted_at IS NULL
LEFT JOIN members m
  ON vk.entity_type = 'member'::vk_entity_type_enum
  AND vk.entity_id = m.id
  AND m.deleted_at IS NULL
WHERE vk.id = $1 AND vk.deleted_at IS NULL";

const FETCH_ACTIVE_X402_ENDPOINT_SECRET_CIPHERTEXT_SQL: &str = r"
            SELECT secret_ciphertext
            FROM x402_endpoint_secrets
            WHERE endpoint_id = $1
              AND status::text = 'active'
            LIMIT 1
            ";

const FETCH_X402_PAYMENT_ACTIVITY_LOG_FIELDS_SQL: &str = r"
            SELECT
                ale_receive_wallet_address,
                fee_wallet_address,
                ai_cost::float8 AS ai_cost,
                trace_status::text AS trace_status,
                settled_at,
                available_at,
                verified_at
            FROM x402_payment_activities
            WHERE id = $1
            LIMIT 1
            ";

impl RouterStore {
    pub fn new(pool: PgPool) -> Result<Self, InitError> {
        Ok(Self { pool })
    }

    pub async fn fetch_active_x402_endpoint_snapshot(
        &self,
        slug: &str,
        method: &str,
    ) -> Result<
        Option<crate::x402::snapshot::DbX402EndpointSnapshotRow>,
        sqlx::Error,
    > {
        sqlx::query_as::<_, crate::x402::snapshot::DbX402EndpointSnapshotRow>(
            r"
            SELECT
                e.id AS endpoint_id,
                e.workspace_id,
                e.agent_id,
                e.status::text AS status,
                e.name,
                e.slug,
                NULLIF(e.endpoint_type::text, '') AS endpoint_type,
                e.method::text AS method,
                e.path,
                e.pricing_model::text AS pricing_model,
                e.price_amount::text AS price_amount,
                e.asset::text AS asset,
                e.network::text AS network,
                w.address AS receive_wallet_address,
                COALESCE(e.body_schema, 'null'::jsonb) AS body_schema,
                e.target_kind::text AS target_kind,
                e.original_target_url,
                e.target_forward_method::text AS target_forward_method,
                e.target_path_rewrite,
                e.target_headers_policy,
                e.origin_signature_required,
                e.active_secret_version,
                p.id AS policy_id,
                p.buyer_access::text AS buyer_access,
                p.rate_limit_rpm,
                p.max_request_size,
                p.timeout_seconds,
                p.payment_retry_attempts,
                p.schema_validation_required,
                p.facilitator::text AS facilitator,
                p.cache_billing_mode::text AS cache_billing_mode,
                p.cache_hit_discount_bps,
                e.snapshot_revision,
                e.updated_at
            FROM x402_endpoints e
            JOIN x402_endpoint_policies p ON p.id = e.policy_id
            JOIN x402_receive_wallets w ON w.id = e.receive_wallet_id
            WHERE lower(e.slug) = lower($1)
              AND lower(e.method::text) = lower($2)
              AND e.status::text = 'active'
              AND e.deleted_at IS NULL
              AND p.deleted_at IS NULL
              AND w.deleted_at IS NULL
            LIMIT 1
            ",
        )
        .bind(slug)
        .bind(method)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn fetch_active_x402_endpoint_type(
        &self,
        slug: &str,
        method: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar::<_, Option<String>>(
            r"
            SELECT NULLIF(e.endpoint_type::text, '') AS endpoint_type
            FROM x402_endpoints e
            WHERE lower(e.slug) = lower($1)
              AND lower(e.method::text) = lower($2)
              AND e.status::text = 'active'
              AND e.deleted_at IS NULL
            LIMIT 1
            ",
        )
        .bind(slug)
        .bind(method)
        .fetch_optional(&self.pool)
        .await
        .map(Option::flatten)
    }

    pub async fn fetch_active_x402_endpoint_secret_ciphertext(
        &self,
        endpoint_id: uuid::Uuid,
    ) -> Result<Option<Vec<u8>>, sqlx::Error> {
        sqlx::query_scalar::<_, Vec<u8>>(
            FETCH_ACTIVE_X402_ENDPOINT_SECRET_CIPHERTEXT_SQL,
        )
        .bind(endpoint_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn fetch_x402_payment_activity_log_fields(
        &self,
        activity_id: uuid::Uuid,
    ) -> Result<Option<DbX402PaymentActivityLogFields>, sqlx::Error> {
        sqlx::query_as::<_, DbX402PaymentActivityLogFields>(
            FETCH_X402_PAYMENT_ACTIVITY_LOG_FIELDS_SQL,
        )
        .bind(activity_id)
        .fetch_optional(&self.pool)
        .await
    }

    // ------------------------------------------------------------------
    // providers / provider_models — Cloud-mode gateway configuration
    // ------------------------------------------------------------------

    /// Returns all enabled providers from the `providers` table.
    ///
    /// Used in Cloud mode to build `ProvidersConfig` at startup and on hot
    /// update.
    pub async fn get_all_providers_for_gateway(
        &self,
    ) -> Result<Vec<DbGatewayProvider>, InternalError> {
        let rows = sqlx::query_as::<_, DbGatewayProvider>(
            r"SELECT id, code, default_base_url, cn_base_url, updated_at, is_router
              FROM providers
              WHERE enabled = true",
        )
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get providers for gateway");
        })?;
        Ok(rows)
    }

    /// Returns all enabled provider models from the `provider_models` table.
    ///
    /// Used alongside [`get_all_providers_for_gateway`] to build the full
    /// `ProvidersConfig`.
    pub async fn get_all_provider_models_for_gateway(
        &self,
    ) -> Result<Vec<DbGatewayProviderModel>, InternalError> {
        let rows = sqlx::query_as::<_, DbGatewayProviderModel>(
            r"SELECT provider_id, model_id
              FROM provider_models
              WHERE enabled = true",
        )
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get provider models for gateway");
        })?;
        Ok(rows)
    }

    /// Returns `info` JSON for an enabled provider model, or `None` if
    /// missing / disabled / null column.
    pub async fn get_model_info_for_gateway_model(
        &self,
        provider_code: &str,
        model_id: &str,
    ) -> Result<Option<serde_json::Value>, InternalError> {
        let row = sqlx::query_as::<_, DbGatewayModelInfoRow>(
            r#"SELECT pm.info AS "info"
              FROM provider_models pm
              INNER JOIN providers p ON p.id = pm.provider_id
              WHERE lower(p.code) = lower($1)
                AND pm.model_id = $2
                AND p.enabled = true
                AND pm.enabled = true"#,
        )
        .bind(provider_code)
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get model info for gateway model");
        })?;
        Ok(row.and_then(|r| r.info.map(|j| j.0)))
    }

    /// Returns enabled requested gateway model rows in input order, including
    /// optional `info`. Missing / disabled candidates are omitted.
    pub async fn get_gateway_model_selection_info_batch(
        &self,
        models: &[(String, String)],
    ) -> Result<Vec<DbGatewayModelSelectionInfoRow>, InternalError> {
        if models.is_empty() {
            return Ok(vec![]);
        }
        let provider_codes: Vec<String> = models
            .iter()
            .map(|(provider, _)| provider.clone())
            .collect();
        let model_ids: Vec<String> =
            models.iter().map(|(_, model)| model.clone()).collect();

        let rows = sqlx::query_as::<_, DbGatewayModelSelectionInfoBatchRow>(
            r#"SELECT req.ord,
                      p.code AS provider_code,
                      pm.model_id,
                      pm.info AS "info"
                 FROM unnest($1::text[], $2::text[]) WITH ORDINALITY
                      AS req(provider_code, model_id, ord)
                 INNER JOIN providers p
                         ON lower(p.code) = lower(req.provider_code)
                        AND p.enabled = true
                 INNER JOIN provider_models pm
                         ON pm.provider_id = p.id
                        AND lower(pm.model_id) = lower(req.model_id)
                        AND pm.enabled = true
                ORDER BY req.ord"#,
        )
        .bind(&provider_codes)
        .bind(&model_ids)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to batch load model selection info");
        })?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let _ = row.ord;
                DbGatewayModelSelectionInfoRow {
                    provider_code: row.provider_code,
                    model_id: row.model_id,
                    info: row.info,
                }
            })
            .collect())
    }

    /// `true` when an enabled `(providers.code, provider_models.model_id)` row
    /// exists (case-insensitive on both columns).
    pub async fn gateway_model_pair_supported(
        &self,
        provider_code: &str,
        model_id: &str,
    ) -> Result<bool, InternalError> {
        let exists = sqlx::query_scalar::<_, bool>(
            r"SELECT EXISTS (
                SELECT 1
                  FROM provider_models pm
                  INNER JOIN providers p ON p.id = pm.provider_id
                 WHERE lower(p.code) = lower($1)
                   AND lower(pm.model_id) = lower($2)
                   AND p.enabled = true
                   AND pm.enabled = true
              )",
        )
        .bind(provider_code)
        .bind(model_id)
        .fetch_one(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                "failed gateway_model_pair_supported lookup"
            );
        })?;
        Ok(exists)
    }

    /// Find all enabled `(provider_code, model_id)` pairs where `model_id`
    /// matches the given bare model id (case-insensitive). Used as a DB
    /// fallback when `BareModelExpandIndex` returns no results.
    pub async fn find_providers_for_bare_model(
        &self,
        bare_model_id: &str,
    ) -> Result<Vec<(String, String)>, InternalError> {
        let rows = sqlx::query_as::<_, (String, String)>(
            r"SELECT p.code, pm.model_id
                FROM provider_models pm
                INNER JOIN providers p ON p.id = pm.provider_id
               WHERE lower(pm.model_id) = lower($1)
                 AND p.enabled = true
                 AND pm.enabled = true",
        )
        .bind(bare_model_id)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                bare_model_id = %bare_model_id,
                "failed find_providers_for_bare_model lookup"
            );
        })?;
        Ok(rows)
    }

    /// All enabled models that have non-null `info`.
    pub async fn list_gateway_models_with_info(
        &self,
    ) -> Result<Vec<DbGatewayModelInfoWarmupRow>, InternalError> {
        let rows = sqlx::query_as::<_, DbGatewayModelInfoWarmupRow>(
            r"SELECT p.code AS provider_code, pm.model_id, pm.info
              FROM provider_models pm
              INNER JOIN providers p ON p.id = pm.provider_id
              WHERE p.enabled = true
                AND pm.enabled = true
                AND pm.info IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to list gateway models with info");
        })?;
        Ok(rows)
    }

    /// Write a single-row `info` (JSON must already pass gateway schema
    /// validation).
    ///
    /// Updates only if the row still belongs to the `providers.code` matching
    /// `provider_code` (case-insensitive), avoiding wrong writes when
    /// `provider_id` disagrees with the price source.
    pub async fn update_provider_model_info(
        &self,
        provider_model_id: Uuid,
        provider_code: &str,
        info: &serde_json::Value,
    ) -> Result<(), InternalError> {
        let n = sqlx::query(
            r"UPDATE provider_models pm
                 SET info = $1,
                     updated_at = now()
                FROM providers p
               WHERE pm.id = $2
                 AND pm.provider_id = p.id
                 AND lower(p.code) = lower($3)",
        )
        .bind(sqlx::types::Json(info))
        .bind(provider_model_id)
        .bind(provider_code)
        .execute(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to update provider_models.info");
        })?
        .rows_affected();
        if n == 0 {
            warn!(
                id = %provider_model_id,
                provider_code = %provider_code,
                "update_provider_model_info affected 0 rows (check provider_models.provider_id vs providers.code)"
            );
        }
        Ok(())
    }

    /// Insert or upsert one `providers` row (on `code` conflict) and return
    /// `id`.
    ///
    /// For `OpenRouter` and similar catalog sync: `default_base_url` is filled
    /// from remote only when empty in DB.
    pub async fn ensure_provider_row(
        &self,
        code: &str,
        name: &str,
        default_base_url: Option<&str>,
        sort_order: i32,
    ) -> Result<Uuid, InternalError> {
        let id = sqlx::query_scalar::<_, Uuid>(
            r"INSERT INTO providers (code, name, default_base_url, sort_order, enabled)
              VALUES ($1, $2, $3, $4, true)
              ON CONFLICT (code) DO UPDATE SET
                name = EXCLUDED.name,
                default_base_url = COALESCE(
                    NULLIF(TRIM(providers.default_base_url), ''),
                    EXCLUDED.default_base_url
                ),
                updated_at = now()
              RETURNING id",
        )
        .bind(code)
        .bind(name)
        .bind(default_base_url)
        .bind(sort_order)
        .fetch_one(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, code = %code, "ensure_provider_row failed");
        })?;
        Ok(id)
    }

    /// Upsert catalog row by `(provider_id, model_id)`: always refresh
    /// `display_name`; keep existing `info` when `fill_null_info_only` is
    /// true and DB already has non-null JSON.
    pub async fn upsert_provider_model_catalog(
        &self,
        provider_id: Uuid,
        model_id: &str,
        display_name: &str,
        info: Option<&serde_json::Value>,
        fill_null_info_only: bool,
    ) -> Result<(), InternalError> {
        sqlx::query(
            r"INSERT INTO provider_models (provider_id, model_id, display_name, enabled, info)
              VALUES ($1, $2, $3, true, $4)
              ON CONFLICT (provider_id, model_id) DO UPDATE SET
                display_name = EXCLUDED.display_name,
                enabled = true,
                info = CASE
                  WHEN $5 AND provider_models.info IS NOT NULL
                  THEN provider_models.info
                  ELSE EXCLUDED.info
                END,
                updated_at = now()",
        )
        .bind(provider_id)
        .bind(model_id)
        .bind(display_name)
        .bind(info.map(sqlx::types::Json))
        .bind(fill_null_info_only)
        .execute(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                model_id = %model_id,
                "upsert_provider_model_catalog failed"
            );
        })?;
        Ok(())
    }

    /// Returns `true` if any `providers` or `provider_models` row has been
    /// updated after `since`.  Used by `db_listener` for cheap incremental
    /// change detection before triggering a full re-fetch.
    pub async fn has_providers_updated_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<bool, InternalError> {
        let found = sqlx::query_scalar::<_, bool>(
            r"SELECT
                EXISTS (SELECT 1 FROM providers WHERE updated_at > $1)
                OR
                EXISTS (SELECT 1 FROM provider_models WHERE updated_at > $1)",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to check for provider updates");
        })?;
        Ok(found)
    }

    /// Returns a stable fingerprint for the enabled provider gateway catalog.
    ///
    /// Used during startup to decide whether the DB listener's first poll can
    /// skip a redundant full provider reload. The fingerprint includes enabled
    /// providers and enabled provider_models so hard deletes and enable/disable
    /// changes are visible without relying on application clock watermarks.
    pub async fn provider_gateway_fingerprint(
        &self,
    ) -> Result<String, InternalError> {
        let catalog = sqlx::query_scalar::<_, String>(
            r"SELECT COALESCE(string_agg(entry, E'\n' ORDER BY entry), '')
                FROM (
                    SELECT concat_ws(
                        '|',
                        'p',
                        id::text,
                        code,
                        COALESCE(default_base_url, ''),
                        COALESCE(cn_base_url, ''),
                        is_router::text,
                        updated_at::text
                    ) AS entry
                    FROM providers
                    WHERE enabled = true
                    UNION ALL
                    SELECT concat_ws(
                        '|',
                        'm',
                        id::text,
                        provider_id::text,
                        model_id,
                        updated_at::text
                    ) AS entry
                    FROM provider_models
                    WHERE enabled = true
                ) provider_gateway_entries",
        )
        .fetch_one(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to fingerprint provider gateway catalog");
        })?;

        let mut hasher = Sha256::new();
        hasher.update(catalog.as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());
        Ok(fingerprint)
    }

    /// Returns a map of `workspace_id → set of allowed InferenceProviders`
    /// built from every row in `provider_configs` (joined with `providers`)
    /// where both records are enabled.
    ///
    /// Called once at startup and on hot-reload when `provider_configs`
    /// changes. Callers must NOT invoke this on the hot request path — read
    /// the in-memory snapshot from `AppState` instead.
    pub async fn get_workspace_provider_allowlist(
        &self,
    ) -> Result<
        rustc_hash::FxHashMap<Uuid, HashSet<InferenceProvider>>,
        InternalError,
    > {
        #[derive(sqlx::FromRow)]
        struct Row {
            workspace_id: Uuid,
            provider_code: String,
        }

        let rows = sqlx::query_as::<_, Row>(
            r"SELECT pc.workspace_id, p.code AS provider_code
              FROM provider_configs pc
              JOIN providers p ON p.id = pc.provider_id
              WHERE pc.enabled = true AND p.enabled = true",
        )
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to load workspace provider allowlist");
        })?;

        let mut map: rustc_hash::FxHashMap<Uuid, HashSet<InferenceProvider>> =
            rustc_hash::FxHashMap::default();
        for row in rows {
            if let Ok(provider) =
                InferenceProvider::from_provider_code(&row.provider_code)
            {
                map.entry(row.workspace_id).or_default().insert(provider);
            } else {
                warn!(
                    workspace_id = %row.workspace_id,
                    provider_code = %row.provider_code,
                    "provider_configs: unknown provider code, skipping"
                );
            }
        }
        Ok(map)
    }

    /// Returns `true` if any `provider_configs` row has been updated after
    /// `since`.  Used by `db_listener` for cheap change detection before
    /// triggering a full re-fetch of the workspace allowlist.
    pub async fn has_provider_configs_updated_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<bool, InternalError> {
        let found = sqlx::query_scalar::<_, bool>(
            r"SELECT EXISTS (SELECT 1 FROM provider_configs WHERE updated_at > $1)",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to check for provider_configs updates");
        })?;
        Ok(found)
    }

    // ------------------------------------------------------------------
    // routers / router_config_versions  (legacy Cloud discovery)
    // These methods are deprecated; Cloud mode now derives routes from
    // providers/provider_models.
    // ------------------------------------------------------------------

    #[deprecated(
        since = "0.2.0",
        note = "Cloud routing now uses providers/provider_models. Will be \
                removed once the routers table is dropped."
    )]
    pub async fn get_all_routers(
        &self,
    ) -> Result<Vec<DbRouterConfig>, InternalError> {
        let res = sqlx::query_as::<_, DbRouterConfig>(
            r"SELECT DISTINCT ON (routers.id)
                     routers.hash as router_hash,
                     routers.organization_id as organization_id,
                     router_config_versions.config,
                     router_config_versions.created_at
             FROM router_config_versions
             INNER JOIN routers ON router_config_versions.router_id = routers.id
             ORDER BY routers.id, router_config_versions.created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get all routers");
        })?;
        Ok(res)
    }

    #[deprecated(
        since = "0.2.0",
        note = "Cloud routing now uses providers/provider_models. Will be \
                removed once the routers table is dropped."
    )]
    pub async fn get_routers_created_after(
        &self,
        created_at: DateTime<Utc>,
    ) -> Result<Vec<DbRouterConfig>, InternalError> {
        let res = sqlx::query_as::<_, DbRouterConfig>(
            r"SELECT DISTINCT ON (routers.id)
                     routers.hash as router_hash,
                     routers.organization_id as organization_id,
                     router_config_versions.config,
                     router_config_versions.created_at
             FROM router_config_versions
             INNER JOIN routers ON router_config_versions.router_id = routers.id
             WHERE router_config_versions.created_at > $1
             ORDER BY routers.id, router_config_versions.created_at DESC",
        )
        .bind(created_at)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get routers created after");
        })?;
        Ok(res)
    }

    // ------------------------------------------------------------------
    // virtual_keys — new methods (Task C)
    // ------------------------------------------------------------------

    /// Fetch all active, non-deleted virtual keys for in-memory cache load.
    pub async fn get_all_db_virtual_keys(
        &self,
    ) -> Result<Vec<DbVirtualKey>, InternalError> {
        let res =
            sqlx::query_as::<_, DbVirtualKey>(GET_ALL_DB_VIRTUAL_KEYS_SQL)
                .fetch_all(&self.pool)
                .await
                .inspect_err(|e| {
                    error!(error = %e, "failed to get all virtual keys");
                })?;
        Ok(res)
    }

    /// Single-row lookup for auth PG fallback (active, not soft-deleted).
    pub async fn get_db_virtual_key_by_key_hash(
        &self,
        key_hash: &str,
    ) -> Result<Option<DbVirtualKey>, InternalError> {
        let row = sqlx::query_as::<_, DbVirtualKey>(
            GET_DB_VIRTUAL_KEY_BY_KEY_HASH_SQL,
        )
        .bind(key_hash)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                key_hash_len = key_hash.len(),
                "failed to load virtual key by hash",
            );
        })?;
        Ok(row)
    }

    /// Incremental fetch: returns every `virtual_key` row whose `updated_at` is
    /// strictly after `since`.  Rows with `deleted_at IS NOT NULL` (soft
    /// deletes) are included so callers can evict them from the in-memory
    /// cache.
    pub async fn get_db_virtual_keys_updated_after(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<DbVirtualKey>, InternalError> {
        let res = sqlx::query_as::<_, DbVirtualKey>(
            GET_DB_VIRTUAL_KEYS_UPDATED_AFTER_SQL,
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get virtual keys updated after");
        })?;
        Ok(res)
    }

    /// Fetch enabled `policy_configs` with `name = 'Model Allowlist'` (highest
    /// priority first).
    pub async fn get_policy_config_model_access_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<DbPolicyConfigModelAccessRow>, InternalError> {
        let row = sqlx::query_as::<_, DbPolicyConfigModelAccessRow>(
            GET_POLICY_CONFIG_MODEL_ACCESS_SQL,
        )
        .bind(workspace_id)
        .bind(POLICY_NAME_MODEL_ALLOWLIST)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                workspace_id = %workspace_id,
                "failed to get policy_config Model Allowlist",
            );
        })?;
        Ok(row)
    }

    /// After resolving `department_id` from VK, fetch one `policy_overrides`
    /// row (table must have `workspace_id` + `department_id`; same as
    /// `alephant_dev`).
    pub async fn get_policy_overrides_for_virtual_key(
        &self,
        workspace_id: Uuid,
        virtual_key_id: Uuid,
    ) -> Result<Option<DbPolicyOverrideRow>, InternalError> {
        let row = sqlx::query_as::<_, DbPolicyOverrideRow>(
            GET_POLICY_OVERRIDES_BY_VIRTUAL_KEY_SQL,
        )
        .bind(workspace_id)
        .bind(virtual_key_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                workspace_id = %workspace_id,
                virtual_key_id = %virtual_key_id,
                "failed to get policy_overrides for virtual key",
            );
        })?;
        Ok(row)
    }

    /// Fetch `policy_overrides` by workspace + department (caller parses
    /// `modelWhitelist.models` inside `overrides`).
    pub async fn get_policy_overrides_by_workspace_and_department(
        &self,
        workspace_id: Uuid,
        department_id: Uuid,
    ) -> Result<Option<DbPolicyOverrideRow>, InternalError> {
        let row = sqlx::query_as::<_, DbPolicyOverrideRow>(
            GET_POLICY_OVERRIDES_BY_WORKSPACE_AND_DEPARTMENT_SQL,
        )
        .bind(workspace_id)
        .bind(department_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                workspace_id = %workspace_id,
                department_id = %department_id,
                "failed to get policy_overrides by workspace and department",
            );
        })?;
        Ok(row)
    }

    /// Workspace-level fallback: most recent `policy_overrides` when no
    /// department/VK match.
    pub async fn get_policy_overrides_workspace_default(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<DbPolicyOverrideRow>, InternalError> {
        let row = sqlx::query_as::<_, DbPolicyOverrideRow>(
            GET_POLICY_OVERRIDES_WORKSPACE_DEFAULT_SQL,
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                workspace_id = %workspace_id,
                "failed to get policy_overrides workspace default",
            );
        })?;
        Ok(row)
    }

    /// Loads workspace type and department id/name for request logging, using
    /// one round-trip (`virtual_keys` → `workspaces` → `agents` / `members` →
    /// `departments`).
    ///
    /// Returns `Ok(None)` when the virtual key row is missing or soft-deleted.
    pub async fn fetch_request_log_department_enrichment(
        &self,
        virtual_key_id: Uuid,
    ) -> Result<Option<DbRequestLogDepartmentEnrichment>, InternalError> {
        let row = sqlx::query_as::<_, DbRequestLogDepartmentEnrichment>(
            FETCH_REQUEST_LOG_DEPARTMENT_ENRICHMENT_SQL,
        )
        .bind(virtual_key_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                virtual_key_id = %virtual_key_id,
                "failed to fetch request log department enrichment"
            );
        })?;
        Ok(row)
    }

    /// Lists `virtual_keys.id` rows whose department enrichment may depend on
    /// the touched row (`scope` + `id` from `enrichment_touch` NOTIFY).
    pub async fn list_virtual_key_ids_for_enrichment_touch(
        &self,
        scope: EnrichmentTouchScope,
        id: Uuid,
    ) -> Result<Vec<Uuid>, InternalError> {
        let rows = match scope {
            EnrichmentTouchScope::VirtualKey => {
                sqlx::query_scalar::<_, Uuid>(
                    r"SELECT id FROM virtual_keys
                      WHERE id = $1 AND deleted_at IS NULL",
                )
                .bind(id)
                .fetch_all(&self.pool)
                .await
            }
            EnrichmentTouchScope::Workspace => {
                sqlx::query_scalar::<_, Uuid>(
                    r"SELECT id FROM virtual_keys
                      WHERE workspace_id = $1 AND deleted_at IS NULL",
                )
                .bind(id)
                .fetch_all(&self.pool)
                .await
            }
            EnrichmentTouchScope::Agent => {
                sqlx::query_scalar::<_, Uuid>(
                    r"SELECT id FROM virtual_keys
                      WHERE entity_type = 'agent'::vk_entity_type_enum
                        AND entity_id = $1
                        AND deleted_at IS NULL",
                )
                .bind(id)
                .fetch_all(&self.pool)
                .await
            }
            EnrichmentTouchScope::Member => {
                sqlx::query_scalar::<_, Uuid>(
                    r"SELECT id FROM virtual_keys
                      WHERE entity_type = 'member'::vk_entity_type_enum
                        AND entity_id = $1
                        AND deleted_at IS NULL",
                )
                .bind(id)
                .fetch_all(&self.pool)
                .await
            }
            EnrichmentTouchScope::Department => {
                sqlx::query_scalar::<_, Uuid>(
                    r"SELECT vk.id
                       FROM virtual_keys vk
                      WHERE vk.deleted_at IS NULL
                        AND (
                          (vk.entity_type = 'agent'::vk_entity_type_enum
                           AND EXISTS (
                             SELECT 1 FROM agents a
                              WHERE a.id = vk.entity_id
                                AND a.department_id = $1
                                AND a.deleted_at IS NULL))
                          OR
                          (vk.entity_type = 'member'::vk_entity_type_enum
                           AND EXISTS (
                             SELECT 1 FROM members m
                              WHERE m.id = vk.entity_id
                                AND m.department_id = $1
                                AND m.deleted_at IS NULL))
                        )",
                )
                .bind(id)
                .fetch_all(&self.pool)
                .await
            }
        }
        .inspect_err(|e| {
            error!(
                error = %e,
                ?scope,
                %id,
                "list_virtual_key_ids_for_enrichment_touch failed"
            );
        })?;
        Ok(rows)
    }

    /// Resolves `departments.name` for request logs when `department_id` is
    /// already known (populated at VK auth when `router_store` is available).
    ///
    /// Row choice matches the `departments` subquery in
    /// `fetch_request_log_department_enrichment` (prefer non-deleted, then
    /// latest `updated_at`).
    pub async fn fetch_department_name_by_id(
        &self,
        department_id: Uuid,
    ) -> Result<Option<String>, InternalError> {
        let name = sqlx::query_scalar::<_, String>(
            r"SELECT dep.name::text
              FROM departments dep
              WHERE dep.id = $1
              ORDER BY (dep.deleted_at IS NULL) DESC, dep.updated_at DESC
              LIMIT 1",
        )
        .bind(department_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                %department_id,
                "failed to fetch department name by id"
            );
        })?;
        Ok(name)
    }

    /// Convenience wrapper: loads all active virtual keys and converts them
    /// into the legacy `HashSet<Key>` used by `AppState`.
    ///
    /// `workspace_id` → `organization_id`, `entity_id` → `owner_id`
    /// (falls back to `workspace_id` when `entity_id` is `NULL`).
    pub async fn get_all_virtual_keys(
        &self,
    ) -> Result<HashSet<Key>, InternalError> {
        let rows = self.get_all_db_virtual_keys().await?;
        let keys = rows
            .into_iter()
            .map(virtual_key_row_to_legacy_key)
            .collect();
        Ok(keys)
    }

    // ------------------------------------------------------------------
    // alephant_api_keys (legacy API-keys table) — deprecated methods kept for
    // reference
    // ------------------------------------------------------------------

    #[deprecated(
        since = "0.0.0",
        note = "replaced by get_all_virtual_keys(); will be removed after \
                Task J"
    )]
    pub async fn get_all_alephant_api_keys(
        &self,
    ) -> Result<HashSet<Key>, InternalError> {
        let res = self.get_all_db_alephant_api_keys().await?;
        let keys = res
            .into_iter()
            .map(|k| Key {
                key_hash: k.key_hash,
                owner_id: UserId::new(k.owner_id),
                organization_id: OrgId::new(k.organization_id),
            })
            .collect();

        Ok(keys)
    }

    /// Returns all active, non-deleted virtual keys as `DbApiKey` for
    /// compatibility with NOTIFY and legacy callers. Uses `virtual_keys` table;
    /// `owner_id` = `COALESCE(entity_id, workspace_id)`, `organization_id` =
    /// `workspace_id`.
    pub async fn get_all_db_alephant_api_keys(
        &self,
    ) -> Result<Vec<DbApiKey>, InternalError> {
        let res = sqlx::query_as::<_, DbApiKey>(
            r"SELECT
                key_hash,
                COALESCE(entity_id, workspace_id) AS owner_id,
                workspace_id AS organization_id,
                created_at,
                updated_at,
                false AS soft_delete
              FROM virtual_keys
              WHERE deleted_at IS NULL
                AND status = 'active'",
        )
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                "failed to get all alephant api keys (virtual_keys)"
            );
        })?;
        Ok(res)
    }

    /// Returns `virtual_keys` rows updated or created after `updated_at`, as
    /// `DbApiKey`. `soft_delete` is true when the row is deleted or
    /// inactive. Uses `virtual_keys` table.
    pub async fn get_all_db_alephant_api_keys_updated_after(
        &self,
        updated_at: DateTime<Utc>,
    ) -> Result<Vec<DbApiKey>, InternalError> {
        let res = sqlx::query_as::<_, DbApiKey>(
            r"SELECT
                key_hash,
                COALESCE(entity_id, workspace_id) AS owner_id,
                workspace_id AS organization_id,
                created_at,
                updated_at,
                (deleted_at IS NOT NULL OR status::text <> 'active') AS soft_delete
              FROM virtual_keys
              WHERE updated_at > $1
                 OR created_at > $1",
        )
        .bind(updated_at)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                "failed to get alephant api keys updated after (virtual_keys)"
            );
        })?;
        Ok(res)
    }

    // ------------------------------------------------------------------
    // master_keys — Task F
    // ------------------------------------------------------------------

    /// Fetch one master key row (JOIN providers) by `master_key_id`.
    /// Returns `None` when the key is soft-deleted, inactive, or not found.
    pub async fn get_master_key_row(
        &self,
        master_key_id: Uuid,
    ) -> Result<Option<DbMasterKeyRow>, InternalError> {
        let res = sqlx::query_as::<_, DbMasterKeyRow>(
            r"SELECT
                mk.id,
                mk.workspace_id,
                mk.key_ciphertext,
                mk.key_nonce,
                mk.base_url,
                mk.status::text       AS status,
                mk.updated_at,
                p.code                AS provider_code
              FROM master_keys mk
              JOIN providers p ON p.id = mk.provider_id
              WHERE mk.id = $1
                AND mk.deleted_at IS NULL
                AND mk.status = 'active'",
        )
        .bind(master_key_id)
        .fetch_optional(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, %master_key_id, "failed to fetch master_key row");
        })?;
        Ok(res)
    }

    /// Incremental fetch for `master_keys` updates.
    ///
    /// Includes all rows updated after `since` (active/inactive/deleted);
    /// callers can decide how to invalidate in-memory state.
    pub async fn get_master_keys_updated_after(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<DbMasterKeyUpdateRow>, InternalError> {
        let res = sqlx::query_as::<_, DbMasterKeyUpdateRow>(
            r"SELECT id, updated_at
              FROM master_keys
              WHERE updated_at > $1",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(error = %e, "failed to get updated master_keys");
        })?;
        Ok(res)
    }

    /// Returns IDs of active, non-deleted master keys in the given workspace
    /// for the given provider (matched by `providers.code`).
    ///
    /// Used for workspace-level fallback when the primary master key is
    /// unavailable.
    pub async fn get_master_key_ids_by_workspace_and_provider(
        &self,
        workspace_id: Uuid,
        provider_code: &str,
    ) -> Result<Vec<Uuid>, InternalError> {
        let rows = sqlx::query_scalar::<_, Uuid>(
            GET_MASTER_KEY_IDS_BY_WORKSPACE_AND_PROVIDER_SQL,
        )
        .bind(workspace_id)
        .bind(provider_code)
        .fetch_all(&self.pool)
        .await
        .inspect_err(|e| {
            error!(
                error = %e,
                workspace_id = %workspace_id,
                provider_code = %provider_code,
                "failed to get master_key ids by workspace and provider"
            );
        })?;
        Ok(rows)
    }

    pub async fn get_all_provider_keys(
        &self,
    ) -> Result<FxHashMap<OrgId, ProviderKeyMap>, InitError> {
        let res = sqlx::query_as::<_, DbProviderKey>(
            "SELECT decrypted_provider_keys.provider_name, \
             decrypted_provider_keys.decrypted_provider_key, \
             decrypted_provider_keys.org_id, decrypted_provider_keys.config \
             FROM decrypted_provider_keys WHERE soft_delete = false AND \
             provider_key IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to get all provider keys");
            InitError::DatabaseConnection(e)
        })?;
        let mut provider_keys: FxHashMap<
            OrgId,
            FxHashMap<InferenceProvider, ProviderKey>,
        > = FxHashMap::default();
        for key in res {
            let provider_key =
                ProviderKey::Secret(Secret::from(key.decrypted_provider_key));
            let Ok(inference_provider) =
                InferenceProvider::from_provider_code(&key.provider_name)
            else {
                continue;
            };
            let existing_provider_keys =
                provider_keys.entry(OrgId::new(key.org_id)).or_default();
            existing_provider_keys.insert(inference_provider, provider_key);
        }

        let mut final_provider_keys = FxHashMap::default();
        for (org_id, provider_keys) in provider_keys.drain() {
            let provider_key_map =
                ProviderKeyMap::from_db(provider_keys.clone());
            final_provider_keys.insert(org_id, provider_key_map);
        }

        Ok(final_provider_keys)
    }

    pub async fn get_org_provider_keys(
        &self,
        org_id: OrgId,
    ) -> Result<ProviderKeyMap, InitError> {
        let res = sqlx::query_as::<_, DbProviderKey>(
            "SELECT decrypted_provider_keys.provider_name, \
             decrypted_provider_keys.decrypted_provider_key, \
             decrypted_provider_keys.org_id, decrypted_provider_keys.config \
             FROM decrypted_provider_keys WHERE org_id = $1 AND soft_delete = \
             false AND provider_key IS NOT NULL",
        )
        .bind(org_id.as_ref())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            error!(error = %e, "failed to get organization provider keys");
            InitError::DatabaseConnection(e)
        })?;
        let mut provider_keys = FxHashMap::default();
        let mut unknown_providers = HashSet::new();

        for key in res {
            let provider_key =
                ProviderKey::Secret(Secret::from(key.decrypted_provider_key));
            let inference_provider =
                match InferenceProvider::from_provider_code(&key.provider_name)
                {
                    Ok(provider) => provider,
                    Err(_e) => {
                        unknown_providers.insert(key.provider_name);
                        continue;
                    }
                };
            provider_keys.insert(inference_provider, provider_key);
        }
        if !unknown_providers.is_empty() {
            warn!(unknown_providers = ?unknown_providers, "unknown providers found in organization provider keys");
        }
        Ok(ProviderKeyMap::from_db(provider_keys))
    }
}

fn virtual_key_row_to_legacy_key(r: DbVirtualKey) -> Key {
    let owner = r.entity_id.unwrap_or(r.workspace_id);
    Key {
        key_hash: r.key_hash,
        owner_id: UserId::new(owner),
        organization_id: OrgId::new(r.workspace_id),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use chrono::{Duration, Utc};

    use super::*;

    fn preferred_test_db_url(default_url: &str) -> String {
        std::env::var("POSTGRES_DATABASE_URL")
            .or_else(|_| std::env::var("AI_GATEWAY__DATABASE__URL"))
            .unwrap_or_else(|_| default_url.to_string())
    }

    fn sample_vk(
        workspace_id: Uuid,
        entity_id: Option<Uuid>,
        key_hash: &str,
    ) -> DbVirtualKey {
        DbVirtualKey {
            id: Uuid::new_v4(),
            workspace_id,
            master_key_id: Uuid::new_v4(),
            key_hash: key_hash.to_string(),
            key_prefix: "vk-test".to_string(),
            label: "test-vk-label".to_string(),
            entity_type: Some("user".to_string()),
            entity_id,
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
    fn virtual_key_mapping_prefers_entity_id_for_owner() {
        let workspace_id = Uuid::new_v4();
        let entity_id = Uuid::new_v4();
        let vk = sample_vk(workspace_id, Some(entity_id), "kh-1");
        let key = virtual_key_row_to_legacy_key(vk);
        assert_eq!(key.key_hash, "kh-1");
        assert_eq!(key.owner_id.as_ref(), &entity_id);
        assert_eq!(key.organization_id.as_ref(), &workspace_id);
    }

    #[test]
    fn virtual_key_mapping_falls_back_to_workspace_for_owner() {
        let workspace_id = Uuid::new_v4();
        let vk = sample_vk(workspace_id, None, "kh-2");
        let key = virtual_key_row_to_legacy_key(vk);
        assert_eq!(key.key_hash, "kh-2");
        assert_eq!(key.owner_id.as_ref(), &workspace_id);
        assert_eq!(key.organization_id.as_ref(), &workspace_id);
    }

    #[test]
    fn master_key_ids_query_has_required_filters() {
        let sql = GET_MASTER_KEY_IDS_BY_WORKSPACE_AND_PROVIDER_SQL;
        assert!(sql.contains("SELECT mk.id"));
        assert!(sql.contains("FROM master_keys mk"));
        assert!(sql.contains("JOIN providers p"));
        assert!(sql.contains("p.code = $2"));
        assert!(sql.contains("mk.workspace_id = $1"));
        assert!(sql.contains("mk.deleted_at IS NULL"));
        assert!(sql.contains("mk.status = 'active'"));
    }

    #[test]
    fn all_virtual_keys_query_projects_policy_and_rate_limit_fields() {
        let sql = GET_ALL_DB_VIRTUAL_KEYS_SQL;
        assert!(sql.contains("FROM virtual_keys vk"));
        assert!(sql.contains("subscription_log_limit"));
        assert!(sql.contains("FROM subscriptions s"));
        assert!(sql.contains("label"));
        assert!(sql.contains("rate_limit_rpm"));
        assert!(sql.contains("rate_limit_rph"));
        assert!(sql.contains("allowed_models"));
        assert!(sql.contains("blocked_models"));
        assert!(sql.contains("WHERE vk.deleted_at IS NULL"));
        assert!(sql.contains("AND vk.status = 'active'"));
    }

    #[test]
    fn virtual_key_by_hash_query_matches_full_load_filters() {
        let sql = GET_DB_VIRTUAL_KEY_BY_KEY_HASH_SQL;
        assert!(sql.contains("FROM virtual_keys vk"));
        assert!(sql.contains("WHERE vk.key_hash = $1"));
        assert!(sql.contains("vk.deleted_at IS NULL"));
        assert!(sql.contains("vk.status = 'active'"));
        assert!(sql.contains("LIMIT 1"));
        assert!(sql.contains("subscription_log_limit"));
        assert!(sql.contains("rate_limit_rpm"));
        assert!(sql.contains("allowed_models"));
        assert!(sql.contains("blocked_models"));
    }

    #[test]
    fn updated_virtual_keys_query_projects_policy_and_rate_limit_fields() {
        let sql = GET_DB_VIRTUAL_KEYS_UPDATED_AFTER_SQL;
        assert!(sql.contains("FROM virtual_keys vk"));
        assert!(sql.contains("subscription_log_limit"));
        assert!(sql.contains("rate_limit_rpm"));
        assert!(sql.contains("rate_limit_rph"));
        assert!(sql.contains("allowed_models"));
        assert!(sql.contains("blocked_models"));
        assert!(sql.contains("WHERE vk.updated_at > $1"));
    }

    #[test]
    fn request_log_department_enrichment_query_joins_core_tables() {
        let sql = FETCH_REQUEST_LOG_DEPARTMENT_ENRICHMENT_SQL;
        assert!(sql.contains("FROM virtual_keys vk"));
        assert!(sql.contains("INNER JOIN workspaces ws"));
        assert!(sql.contains("LEFT JOIN agents a"));
        assert!(sql.contains("LEFT JOIN members m"));
        assert!(sql.contains("FROM departments dep"));
        assert!(sql.contains("WHERE vk.id = $1"));
        assert!(sql.contains("vk.entity_type::text = 'agent'"));
        assert!(sql.contains("vk.entity_type::text = 'member'"));
    }

    #[test]
    fn x402_endpoint_secret_query_matches_current_table_shape() {
        let sql = FETCH_ACTIVE_X402_ENDPOINT_SECRET_CIPHERTEXT_SQL;
        assert!(sql.contains("FROM x402_endpoint_secrets"));
        assert!(sql.contains("endpoint_id = $1"));
        assert!(sql.contains("status::text = 'active'"));
        assert!(!sql.contains("deleted_at"));
        assert!(!sql.contains("updated_at"));
    }

    #[test]
    fn x402_payment_activity_log_fields_query_projects_downstream_fields() {
        let sql = FETCH_X402_PAYMENT_ACTIVITY_LOG_FIELDS_SQL;
        assert!(sql.contains("FROM x402_payment_activities"));
        assert!(sql.contains("WHERE id = $1"));
        assert!(sql.contains("ale_receive_wallet_address"));
        assert!(sql.contains("fee_wallet_address"));
        assert!(sql.contains("ai_cost::float8 AS ai_cost"));
        assert!(sql.contains("trace_status::text AS trace_status"));
        assert!(sql.contains("settled_at"));
        assert!(sql.contains("available_at"));
        assert!(sql.contains("verified_at"));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn get_master_key_ids_by_workspace_and_provider_filters_correctly() {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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

        let store = RouterStore::new(pool.clone()).expect("router store init");

        // Reuse a seeded workspace to avoid creating more FK fixtures.
        let workspace_id =
            Uuid::parse_str("08a417ef-0f6a-4e8b-8819-5dea8608be72").unwrap();
        let other_workspace_id =
            Uuid::parse_str("f9e87d88-39f3-42ef-b485-4991737db6cf").unwrap();
        let provider_id = Uuid::new_v4();
        let other_provider_id = Uuid::new_v4();
        let provider_code = format!("it{}", Uuid::new_v4().simple());
        let other_provider_code = format!("io{}", Uuid::new_v4().simple());

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, $3, true), ($4, $5, $6, true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .bind("Integration Test Provider")
        .bind(other_provider_id)
        .bind(&other_provider_code)
        .bind("Integration Test Provider Other")
        .execute(&pool)
        .await
        .expect("insert test providers");

        let valid_1 = Uuid::new_v4();
        let valid_2 = Uuid::new_v4();
        let wrong_workspace = Uuid::new_v4();
        let wrong_provider = Uuid::new_v4();
        let deleted_key = Uuid::new_v4();
        let inactive_key = Uuid::new_v4();

        let ciphertext = vec![1_u8, 2, 3];
        let nonce = vec![4_u8; 12];

        sqlx::query(
            r"INSERT INTO master_keys
               (id, workspace_id, label, provider_id, key_ciphertext, key_nonce, masked_key)
               VALUES
               ($1, $2, 'valid_1', $3, $4, $5, 'sk-...valid1'),
               ($6, $2, 'valid_2', $3, $4, $5, 'sk-...valid2'),
               ($7, $8, 'wrong_workspace', $3, $4, $5, 'sk-...wrongws'),
               ($9, $2, 'wrong_provider', $10, $4, $5, 'sk-...wrongpv'),
               ($11, $2, 'deleted_key', $3, $4, $5, 'sk-...deleted'),
               ($12, $2, 'inactive_key', $3, $4, $5, 'sk-...inactive')",
        )
        .bind(valid_1)
        .bind(workspace_id)
        .bind(provider_id)
        .bind(&ciphertext)
        .bind(&nonce)
        .bind(valid_2)
        .bind(wrong_workspace)
        .bind(other_workspace_id)
        .bind(wrong_provider)
        .bind(other_provider_id)
        .bind(deleted_key)
        .bind(inactive_key)
        .execute(&pool)
        .await
        .expect("insert test master keys");

        sqlx::query(
            r"UPDATE master_keys
               SET deleted_at = now()
               WHERE id = $1",
        )
        .bind(deleted_key)
        .execute(&pool)
        .await
        .expect("mark deleted test key");

        sqlx::query(
            r"UPDATE master_keys
               SET status = 'inactive'::api_key_status_enum
               WHERE id = $1",
        )
        .bind(inactive_key)
        .execute(&pool)
        .await
        .expect("mark inactive test key");

        let actual = store
            .get_master_key_ids_by_workspace_and_provider(
                workspace_id,
                &provider_code,
            )
            .await
            .expect("query master key ids");

        let actual_set: HashSet<_> = actual.into_iter().collect();
        let expected_set: HashSet<_> = [valid_1, valid_2].into_iter().collect();
        assert_eq!(actual_set, expected_set);

        sqlx::query(
            r"DELETE FROM master_keys
               WHERE id IN ($1, $2, $3, $4, $5, $6)",
        )
        .bind(valid_1)
        .bind(valid_2)
        .bind(wrong_workspace)
        .bind(wrong_provider)
        .bind(deleted_key)
        .bind(inactive_key)
        .execute(&pool)
        .await
        .expect("cleanup test master keys");

        sqlx::query(r"DELETE FROM providers WHERE id IN ($1, $2)")
            .bind(provider_id)
            .bind(other_provider_id)
            .execute(&pool)
            .await
            .expect("cleanup test providers");
    }

    #[tokio::test]
    async fn get_master_key_ids_by_workspace_and_provider_returns_empty_for_unknown_provider_code()
     {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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

        let store = RouterStore::new(pool.clone()).expect("router store init");
        let workspace_id =
            Uuid::parse_str("08a417ef-0f6a-4e8b-8819-5dea8608be72").unwrap();

        let provider_id = Uuid::new_v4();
        let provider_code = format!("it{}", Uuid::new_v4().simple());
        let unknown_provider_code = format!("zz{}", Uuid::new_v4().simple());
        let key_id = Uuid::new_v4();
        let ciphertext = vec![9_u8, 8, 7];
        let nonce = vec![6_u8; 12];

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, $3, true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .bind("Integration Test Unknown Provider")
        .execute(&pool)
        .await
        .expect("insert test provider");

        sqlx::query(
            r"INSERT INTO master_keys
               (id, workspace_id, label, provider_id, key_ciphertext, key_nonce, masked_key)
               VALUES ($1, $2, 'valid_for_other_code', $3, $4, $5, 'sk-...valid')",
        )
        .bind(key_id)
        .bind(workspace_id)
        .bind(provider_id)
        .bind(&ciphertext)
        .bind(&nonce)
        .execute(&pool)
        .await
        .expect("insert test master key");

        let actual = store
            .get_master_key_ids_by_workspace_and_provider(
                workspace_id,
                &unknown_provider_code,
            )
            .await
            .expect("query master key ids");

        assert!(actual.is_empty());

        sqlx::query(r"DELETE FROM master_keys WHERE id = $1")
            .bind(key_id)
            .execute(&pool)
            .await
            .expect("cleanup test master key");

        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("cleanup test provider");
    }

    #[tokio::test]
    async fn get_all_providers_for_gateway_returns_only_enabled_rows() {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let enabled_id = Uuid::new_v4();
        let disabled_id = Uuid::new_v4();
        let enabled_code = format!("it{}", Uuid::new_v4().simple());
        let disabled_code = format!("id{}", Uuid::new_v4().simple());

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES
               ($1, $2, 'Enabled Provider', true),
               ($3, $4, 'Disabled Provider', false)",
        )
        .bind(enabled_id)
        .bind(&enabled_code)
        .bind(disabled_id)
        .bind(&disabled_code)
        .execute(&pool)
        .await
        .expect("insert test providers");

        let rows = store
            .get_all_providers_for_gateway()
            .await
            .expect("query providers");
        let ids: HashSet<_> = rows.into_iter().map(|row| row.id).collect();

        assert!(ids.contains(&enabled_id));
        assert!(!ids.contains(&disabled_id));

        sqlx::query(r"DELETE FROM providers WHERE id IN ($1, $2)")
            .bind(enabled_id)
            .bind(disabled_id)
            .execute(&pool)
            .await
            .expect("cleanup test providers");
    }

    #[tokio::test]
    async fn get_all_provider_models_for_gateway_returns_only_enabled_rows() {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let provider_id = Uuid::new_v4();
        let model_enabled_id = Uuid::new_v4();
        let model_disabled_id = Uuid::new_v4();
        let provider_code = format!("im{}", Uuid::new_v4().simple());

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, 'Provider For Models', true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .execute(&pool)
        .await
        .expect("insert provider");

        sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled)
               VALUES
               ($1, $2, 'model-enabled', true),
               ($3, $2, 'model-disabled', false)",
        )
        .bind(model_enabled_id)
        .bind(provider_id)
        .bind(model_disabled_id)
        .execute(&pool)
        .await
        .expect("insert provider models");

        let rows = store
            .get_all_provider_models_for_gateway()
            .await
            .expect("query provider models");
        let model_ids: HashSet<_> =
            rows.into_iter().map(|row| row.model_id).collect();

        assert!(model_ids.contains("model-enabled"));
        assert!(!model_ids.contains("model-disabled"));

        sqlx::query(r"DELETE FROM provider_models WHERE id IN ($1, $2)")
            .bind(model_enabled_id)
            .bind(model_disabled_id)
            .execute(&pool)
            .await
            .expect("cleanup provider models");

        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("cleanup provider");
    }

    #[tokio::test]
    async fn has_providers_updated_since_detects_provider_and_model_changes() {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let provider_id = Uuid::new_v4();
        let provider_code = format!("iu{}", Uuid::new_v4().simple());
        let provider_model_id = Uuid::new_v4();

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, 'Provider For Update Check', true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .execute(&pool)
        .await
        .expect("insert provider");

        sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled)
               VALUES ($1, $2, 'model-update-check', true)",
        )
        .bind(provider_model_id)
        .bind(provider_id)
        .execute(&pool)
        .await
        .expect("insert provider model");

        let since_past = Utc::now() - Duration::minutes(5);
        let since_future = Utc::now() + Duration::minutes(5);

        assert!(
            store
                .has_providers_updated_since(since_past)
                .await
                .expect("updated since past should query")
        );
        assert!(
            !store
                .has_providers_updated_since(since_future)
                .await
                .expect("updated since future should query")
        );

        sqlx::query(r"DELETE FROM provider_models WHERE id = $1")
            .bind(provider_model_id)
            .execute(&pool)
            .await
            .expect("cleanup provider model");

        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("cleanup provider");
    }

    #[tokio::test]
    async fn provider_gateway_fingerprint_changes_for_effective_catalog_changes()
     {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let provider_id = Uuid::new_v4();
        let provider_code = format!("fp{}", Uuid::new_v4().simple());
        let provider_model_id = Uuid::new_v4();

        sqlx::query(
            r"INSERT INTO providers
                (id, code, name, default_base_url, enabled)
              VALUES ($1, $2, 'Provider Fingerprint Check', $3, true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .bind("https://fingerprint.example/v1")
        .execute(&pool)
        .await
        .expect("insert provider");

        sqlx::query(
            r"INSERT INTO provider_models
                (id, provider_id, model_id, enabled)
              VALUES ($1, $2, 'model-fingerprint-check', true)",
        )
        .bind(provider_model_id)
        .bind(provider_id)
        .execute(&pool)
        .await
        .expect("insert provider model");

        let base = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint base");

        sqlx::query(
            r"UPDATE providers
                 SET default_base_url = $2,
                     updated_at = now()
               WHERE id = $1",
        )
        .bind(provider_id)
        .bind("https://fingerprint.example/v2")
        .execute(&pool)
        .await
        .expect("update provider url");
        let after_provider_update = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint after provider update");
        assert_ne!(after_provider_update, base);

        sqlx::query(
            r"UPDATE providers
                 SET enabled = false,
                     updated_at = now()
               WHERE id = $1",
        )
        .bind(provider_id)
        .execute(&pool)
        .await
        .expect("disable provider");
        let after_provider_disable = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint after provider disable");
        assert_ne!(after_provider_disable, after_provider_update);

        sqlx::query(
            r"UPDATE providers
                 SET enabled = true,
                     updated_at = now()
               WHERE id = $1",
        )
        .bind(provider_id)
        .execute(&pool)
        .await
        .expect("enable provider");

        sqlx::query(
            r"UPDATE provider_models
                 SET enabled = false,
                     updated_at = now()
               WHERE id = $1",
        )
        .bind(provider_model_id)
        .execute(&pool)
        .await
        .expect("disable provider model");
        let after_model_disable = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint after model disable");
        assert_ne!(after_model_disable, after_provider_disable);

        sqlx::query(
            r"UPDATE provider_models
                 SET enabled = true,
                     updated_at = now()
               WHERE id = $1",
        )
        .bind(provider_model_id)
        .execute(&pool)
        .await
        .expect("enable provider model");

        let before_model_delete = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint before model delete");
        sqlx::query(r"DELETE FROM provider_models WHERE id = $1")
            .bind(provider_model_id)
            .execute(&pool)
            .await
            .expect("delete provider model");
        let after_model_delete = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint after model delete");
        assert_ne!(after_model_delete, before_model_delete);

        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("delete provider");
        let after_provider_delete = store
            .provider_gateway_fingerprint()
            .await
            .expect("fingerprint after provider delete");
        assert_ne!(after_provider_delete, after_model_delete);
    }

    #[tokio::test]
    async fn get_model_info_for_gateway_model_returns_json_when_set() {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let provider_id = Uuid::new_v4();
        let model_row_id = Uuid::new_v4();
        let provider_code = format!("ip{}", Uuid::new_v4().simple());
        let price_json = serde_json::json!({
            "schema_version": 1,
            "prompt_uncached_per_million": 2.5
        });

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, 'Provider For Price', true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .execute(&pool)
        .await
        .expect("insert provider");

        let insert_model = sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled, info)
               VALUES ($1, $2, 'priced-model', true, $3)",
        )
        .bind(model_row_id)
        .bind(provider_id)
        .bind(sqlx::types::Json(&price_json))
        .execute(&pool)
        .await;

        if insert_model.is_err() {
            tracing::info!(
                "skip model info test: provider_models.info column missing? \
                 run docs/sql/migrations/20260328_provider_models_price_info.\
                 sql then docs/sql/migrations/\
                 20260414_provider_models_rename_price_info_to_info.sql"
            );
            sqlx::query(r"DELETE FROM providers WHERE id = $1")
                .bind(provider_id)
                .execute(&pool)
                .await
                .expect("cleanup provider");
            return;
        }

        let got = store
            .get_model_info_for_gateway_model(&provider_code, "priced-model")
            .await
            .expect("get model info");
        assert_eq!(got.as_ref(), Some(&price_json));

        let list = store
            .list_gateway_models_with_info()
            .await
            .expect("list with info");
        assert!(list.iter().any(|r| {
            r.provider_code == provider_code
                && r.model_id == "priced-model"
                && r.info.0 == price_json
        }));

        sqlx::query(r"DELETE FROM provider_models WHERE id = $1")
            .bind(model_row_id)
            .execute(&pool)
            .await
            .expect("cleanup provider model");

        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("cleanup provider");
    }

    #[tokio::test]
    async fn get_model_info_for_gateway_model_returns_none_when_provider_model_disabled()
     {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let enabled_provider_id = Uuid::new_v4();
        let disabled_provider_id = Uuid::new_v4();
        let disabled_model_row_id = Uuid::new_v4();
        let disabled_provider_model_row_id = Uuid::new_v4();
        let enabled_provider_code = format!("pm{}", Uuid::new_v4().simple());
        let disabled_provider_code = format!("pd{}", Uuid::new_v4().simple());
        let info_json = serde_json::json!({
            "schema_version": 1,
            "prompt": 1.0,
            "completion": 2.0
        });

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, 'Enabled Provider', true)",
        )
        .bind(enabled_provider_id)
        .bind(&enabled_provider_code)
        .execute(&pool)
        .await
        .expect("insert enabled provider");

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, 'Disabled Provider', false)",
        )
        .bind(disabled_provider_id)
        .bind(&disabled_provider_code)
        .execute(&pool)
        .await
        .expect("insert disabled provider");

        sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled, info)
               VALUES ($1, $2, 'disabled-model-row', false, $3)",
        )
        .bind(disabled_model_row_id)
        .bind(enabled_provider_id)
        .bind(sqlx::types::Json(&info_json))
        .execute(&pool)
        .await
        .expect("insert disabled provider model");

        sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled, info)
               VALUES ($1, $2, 'disabled-provider-row', true, $3)",
        )
        .bind(disabled_provider_model_row_id)
        .bind(disabled_provider_id)
        .bind(sqlx::types::Json(&info_json))
        .execute(&pool)
        .await
        .expect("insert row under disabled provider");

        let disabled_model_result = store
            .get_model_info_for_gateway_model(
                &enabled_provider_code,
                "disabled-model-row",
            )
            .await
            .expect("query disabled model row");
        assert_eq!(disabled_model_result, None);

        let disabled_provider_result = store
            .get_model_info_for_gateway_model(
                &disabled_provider_code,
                "disabled-provider-row",
            )
            .await
            .expect("query disabled provider row");
        assert_eq!(disabled_provider_result, None);

        sqlx::query(r"DELETE FROM provider_models WHERE id = $1")
            .bind(disabled_model_row_id)
            .execute(&pool)
            .await
            .expect("cleanup disabled model row");
        sqlx::query(r"DELETE FROM provider_models WHERE id = $1")
            .bind(disabled_provider_model_row_id)
            .execute(&pool)
            .await
            .expect("cleanup disabled provider row");
        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(enabled_provider_id)
            .execute(&pool)
            .await
            .expect("cleanup enabled provider");
        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(disabled_provider_id)
            .execute(&pool)
            .await
            .expect("cleanup disabled provider");
    }

    #[tokio::test]
    async fn get_gateway_model_selection_info_batch_returns_only_requested_enabled_rows()
     {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let provider_id = Uuid::new_v4();
        let provider_code = format!("bp{}", Uuid::new_v4().simple());
        let enabled_model_id = "batch-enabled-model";
        let null_info_model_id = "batch-null-info-model";
        let disabled_model_id = "batch-disabled-model";
        let enabled_info = serde_json::json!({
            "schema_version": 1,
            "model_interaction_type": "chat",
            "prompt_uncached_per_million": 1.5
        });

        sqlx::query(
            r"INSERT INTO providers (id, code, name, enabled)
               VALUES ($1, $2, 'Batch Provider', true)",
        )
        .bind(provider_id)
        .bind(&provider_code)
        .execute(&pool)
        .await
        .expect("insert provider");

        let insert_enabled = sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled, info)
               VALUES ($1, $2, $3, true, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(provider_id)
        .bind(enabled_model_id)
        .bind(sqlx::types::Json(&enabled_info))
        .execute(&pool)
        .await;
        if insert_enabled.is_err() {
            tracing::info!(
                "skip batch model selection info test: provider_models.info \
                 column missing? run \
                 docs/sql/migrations/20260328_provider_models_price_info.sql \
                 then docs/sql/migrations/\
                 20260414_provider_models_rename_price_info_to_info.sql"
            );
            sqlx::query(r"DELETE FROM providers WHERE id = $1")
                .bind(provider_id)
                .execute(&pool)
                .await
                .expect("cleanup provider");
            return;
        }

        sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled, info)
               VALUES ($1, $2, $3, true, NULL)",
        )
        .bind(Uuid::new_v4())
        .bind(provider_id)
        .bind(null_info_model_id)
        .execute(&pool)
        .await
        .expect("insert null info model");

        sqlx::query(
            r"INSERT INTO provider_models (id, provider_id, model_id, enabled, info)
               VALUES ($1, $2, $3, false, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(provider_id)
        .bind(disabled_model_id)
        .bind(sqlx::types::Json(&enabled_info))
        .execute(&pool)
        .await
        .expect("insert disabled model");

        let rows = store
            .get_gateway_model_selection_info_batch(&[
                (provider_code.clone(), enabled_model_id.to_string()),
                (provider_code.clone(), null_info_model_id.to_string()),
                (provider_code.clone(), disabled_model_id.to_string()),
                (provider_code.clone(), "missing-model".to_string()),
            ])
            .await
            .expect("load batch selection info");

        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().any(|row| {
            row.provider_code == provider_code
                && row.model_id == enabled_model_id
                && row.info.as_ref().map(|j| &j.0) == Some(&enabled_info)
        }));
        assert!(rows.iter().any(|row| {
            row.provider_code == provider_code
                && row.model_id == null_info_model_id
                && row.info.is_none()
        }));
        assert!(
            rows.iter().all(|row| row.model_id != disabled_model_id),
            "{rows:?}"
        );

        sqlx::query(r"DELETE FROM provider_models WHERE provider_id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("cleanup provider models");
        sqlx::query(r"DELETE FROM providers WHERE id = $1")
            .bind(provider_id)
            .execute(&pool)
            .await
            .expect("cleanup provider");
    }

    #[tokio::test]
    async fn get_db_virtual_key_by_key_hash_finds_active_row() {
        const DEFAULT_TEST_DB_URL: &str =
            "postgres://postgres:postgres@localhost:54322/postgres";
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
        let store = RouterStore::new(pool.clone()).expect("router store init");

        let anchor: Option<(Uuid, Uuid)> = sqlx::query_as(
            r"SELECT mk.id, mk.workspace_id
               FROM master_keys mk
               WHERE mk.deleted_at IS NULL
                 AND mk.status = 'active'
               LIMIT 1",
        )
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();

        let Some((master_key_id, workspace_id)) = anchor else {
            tracing::info!(
                "skip get_db_virtual_key_by_key_hash: no active master_key"
            );
            return;
        };

        let vk_id = Uuid::new_v4();
        let key_hash =
            format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        assert_eq!(key_hash.len(), 64);

        sqlx::query(
            r"INSERT INTO virtual_keys
                (id, workspace_id, master_key_id, label, key_hash, key_prefix,
                 status, period_spend_cents, period_request_count, period_start,
                 deleted_at, created_at, updated_at)
              VALUES
                ($1, $2, $3, 'itest vk by hash', $4, 'vk-itest-bh',
                 'active'::virtual_key_status_enum, 0, 0, CURRENT_DATE,
                 NULL, now(), now())",
        )
        .bind(vk_id)
        .bind(workspace_id)
        .bind(master_key_id)
        .bind(&key_hash)
        .execute(&pool)
        .await
        .expect("insert test virtual_keys row");

        let found = store
            .get_db_virtual_key_by_key_hash(&key_hash)
            .await
            .expect("by-hash query")
            .expect("row should match active filters");
        assert_eq!(found.id, vk_id);
        assert_eq!(found.key_hash, key_hash);

        sqlx::query(
            r"UPDATE virtual_keys SET deleted_at = now() WHERE id = $1",
        )
        .bind(vk_id)
        .execute(&pool)
        .await
        .expect("soft-delete test vk");

        assert!(
            store
                .get_db_virtual_key_by_key_hash(&key_hash)
                .await
                .expect("by-hash after soft-delete")
                .is_none()
        );

        sqlx::query(r"DELETE FROM virtual_keys WHERE id = $1")
            .bind(vk_id)
            .execute(&pool)
            .await
            .expect("cleanup test virtual_keys row");
    }
}
