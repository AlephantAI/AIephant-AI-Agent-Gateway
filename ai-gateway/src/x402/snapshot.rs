use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    error::{api::ApiError, internal::InternalError},
    x402::types::{
        ResolvedSnapshot, SnapshotSource, X402EndpointSnapshot, X402OriginAuthSnapshot,
        X402PolicySnapshot, X402TargetHeaderPolicyItem, X402TargetSnapshot,
    },
};

#[derive(Debug, FromRow)]
pub struct DbX402EndpointSnapshotRow {
    pub endpoint_id: Uuid,
    pub workspace_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub status: String,
    pub name: String,
    pub slug: String,
    pub endpoint_type: Option<String>,
    pub method: String,
    pub path: String,
    pub pricing_model: String,
    pub price_amount: String,
    pub asset: String,
    pub network: String,
    pub receive_wallet_address: String,
    pub body_schema: serde_json::Value,
    pub target_kind: String,
    pub original_target_url: String,
    pub target_forward_method: String,
    pub target_path_rewrite: serde_json::Value,
    pub target_headers_policy: serde_json::Value,
    pub origin_signature_required: bool,
    pub active_secret_version: i32,
    pub policy_id: Uuid,
    pub buyer_access: String,
    pub rate_limit_rpm: i32,
    pub max_request_size: i32,
    pub timeout_seconds: i32,
    pub payment_retry_attempts: i32,
    pub schema_validation_required: bool,
    pub facilitator: Option<String>,
    pub cache_billing_mode: String,
    pub cache_hit_discount_bps: i32,
    pub snapshot_revision: i64,
    pub updated_at: DateTime<Utc>,
}

#[must_use]
pub fn endpoint_snapshot_redis_key(slug: &str, method: &str) -> String {
    format!(
        "x402:endpoint:snapshot:{}:{}",
        slug.to_ascii_lowercase(),
        method.to_ascii_uppercase()
    )
}

pub fn parse_snapshot_json(raw: &str) -> Result<X402EndpointSnapshot, serde_json::Error> {
    serde_json::from_str(raw)
}

pub(crate) fn normalize_endpoint_type(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
    })
}

pub fn snapshot_from_db_row(
    row: DbX402EndpointSnapshotRow,
) -> Result<X402EndpointSnapshot, serde_json::Error> {
    let headers_policy =
        serde_json::from_value::<Vec<X402TargetHeaderPolicyItem>>(row.target_headers_policy)?;

    Ok(X402EndpointSnapshot {
        endpoint_id: row.endpoint_id,
        workspace_id: row.workspace_id,
        agent_id: row.agent_id,
        status: row.status,
        name: row.name,
        slug: row.slug,
        endpoint_type: normalize_endpoint_type(row.endpoint_type),
        method: row.method,
        path: row.path,
        pricing_model: row.pricing_model,
        price_amount: row.price_amount,
        asset: row.asset,
        network: row.network,
        receive_wallet_address: row.receive_wallet_address,
        body_schema: row.body_schema,
        fee_bps: 0,
        target: X402TargetSnapshot {
            kind: row.target_kind,
            original_target_url: row.original_target_url,
            forward_method: row.target_forward_method,
            path_rewrite: row.target_path_rewrite,
            headers_policy,
            origin_signature_required: row.origin_signature_required,
        },
        origin_auth: X402OriginAuthSnapshot {
            active_secret_version: row.active_secret_version,
        },
        policy: X402PolicySnapshot {
            policy_id: row.policy_id,
            buyer_access: row.buyer_access,
            rate_limit_rpm: row.rate_limit_rpm,
            max_request_size: row.max_request_size,
            timeout_seconds: row.timeout_seconds,
            payment_retry_attempts: row.payment_retry_attempts,
            schema_validation_required: row.schema_validation_required,
            facilitator: row.facilitator,
            cache_billing_mode: row.cache_billing_mode,
            cache_hit_discount_bps: row.cache_hit_discount_bps,
        },
        snapshot_revision: row.snapshot_revision,
        config_revision: Some(row.snapshot_revision),
        compiled_at: Some(row.updated_at),
    })
}

async fn fill_missing_endpoint_type_from_db(
    app_state: &AppState,
    snapshot: &mut X402EndpointSnapshot,
    slug: &str,
    method: &str,
) -> Result<(), ApiError> {
    snapshot.endpoint_type = normalize_endpoint_type(snapshot.endpoint_type.take());
    if snapshot.endpoint_type.is_some() {
        return Ok(());
    }

    let Some(store) = app_state.router_store() else {
        return Ok(());
    };

    snapshot.endpoint_type = normalize_endpoint_type(
        store
            .fetch_active_x402_endpoint_type(slug, method)
            .await
            .map_err(InternalError::DatabaseError)?,
    );

    Ok(())
}

pub async fn resolve_snapshot(
    app_state: &AppState,
    slug: &str,
    method: &str,
) -> Result<Option<ResolvedSnapshot>, ApiError> {
    let key = endpoint_snapshot_redis_key(slug, method);

    if let Some(redis) = app_state.redis() {
        match redis.get_string(&key).await {
            Ok(Some(raw)) => {
                let mut snapshot = parse_snapshot_json(&raw).map_err(|error| {
                    tracing::error!(
                        error = %error,
                        key = %key,
                        "x402 snapshot JSON parse failed"
                    );
                    InternalError::Deserialize {
                        ty: "X402EndpointSnapshot",
                        error,
                    }
                })?;
                fill_missing_endpoint_type_from_db(app_state, &mut snapshot, slug, method).await?;
                return Ok(Some(ResolvedSnapshot {
                    snapshot,
                    source: SnapshotSource::Redis,
                }));
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    key = %key,
                    "x402 snapshot Redis lookup failed; falling back to DB"
                );
            }
        }
    }

    let Some(store) = app_state.router_store() else {
        return Ok(None);
    };

    let Some(row) = store
        .fetch_active_x402_endpoint_snapshot(slug, method)
        .await
        .map_err(InternalError::DatabaseError)?
    else {
        return Ok(None);
    };

    let snapshot = snapshot_from_db_row(row).map_err(|error| {
        tracing::error!(
            error = %error,
            "x402 DB snapshot mapping failed"
        );
        InternalError::Deserialize {
            ty: "Vec<X402TargetHeaderPolicyItem>",
            error,
        }
    })?;

    Ok(Some(ResolvedSnapshot {
        snapshot,
        source: SnapshotSource::Db,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_key_lowercases_slug_and_method() {
        assert_eq!(
            endpoint_snapshot_redis_key("Test-Agent", "POST"),
            "x402:endpoint:snapshot:test-agent:POST"
        );
    }

    #[test]
    fn redis_snapshot_example_deserializes() {
        let raw = r#"{
          "endpoint_id": "67b3982d-c4df-4ca4-b7eb-cde5db870db2",
          "workspace_id": "7660e625-6e98-4bb3-93be-44fce119fbc0",
          "status": "active",
          "name": "Test Agent",
          "slug": "test-agent",
          "endpoint_type": "agent",
          "method": "POST",
          "path": "/test-agent",
          "pricing_model": "per_call",
          "price_amount": "1.00000000",
          "asset": "USDC",
          "network": "base",
          "receive_wallet_address": "0xtesttesttesttesttesttesttest",
          "fee_bps": 500,
          "body_schema": {
            "type": "object",
            "required": ["wallet_address", "chain"],
            "properties": {
              "chain": {
                "enum": ["base", "ethereum", "solana"],
                "type": "string",
                "description": "The blockchain network"
              },
              "wallet_address": {
                "type": "string",
                "description": "The wallet address to analyze"
              }
            }
          },
          "target": {
            "kind": "http_proxy",
            "original_target_url": "https://www.baidu.com",
            "forward_method": "preserve",
            "path_rewrite": {"prefix": "/", "strip_public_slug": true},
            "headers_policy": [
              {"name": "X-API-Key", "value": "secret"},
              {"name": "X-Client-Id", "value": "abc123"}
            ],
            "origin_signature_required": true
          },
          "origin_auth": {"active_secret_version": 1},
          "policy": {
            "policy_id": "60c3208e-5137-4a22-9e55-0094a458732e",
            "buyer_access": "Public",
            "rate_limit_rpm": 100,
            "max_request_size": 1000000,
            "timeout_seconds": 6,
            "payment_retry_attempts": 3,
            "schema_validation_required": true,
            "facilitator": "coinbase",
            "cache_billing_mode": "disabled",
            "cache_hit_discount_bps": 0
          },
          "snapshot_revision": 4,
          "config_revision": 4,
          "compiled_at": "2026-05-11T08:05:18Z"
        }"#;

        let snap = parse_snapshot_json(raw).unwrap();

        assert_eq!(snap.slug, "test-agent");
        assert_eq!(snap.endpoint_type.as_deref(), Some("agent"));
        assert_eq!(snap.target.original_target_url, "https://www.baidu.com");
        assert_eq!(snap.policy.timeout_seconds, 6);
        assert_eq!(snap.body_schema["required"][0], "wallet_address");
        assert_eq!(snap.target.headers_policy.len(), 2);
        assert_eq!(snap.target.headers_policy[0].name, "X-API-Key");
        assert_eq!(
            snap.target.headers_policy[0].value.as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn redis_snapshot_without_endpoint_type_stays_compatible() {
        let raw = r#"{
          "endpoint_id": "67b3982d-c4df-4ca4-b7eb-cde5db870db2",
          "workspace_id": "7660e625-6e98-4bb3-93be-44fce119fbc0",
          "status": "active",
          "name": "Test Agent",
          "slug": "test-agent",
          "method": "POST",
          "path": "/test-agent",
          "pricing_model": "per_call",
          "price_amount": "1.00000000",
          "asset": "USDC",
          "network": "base",
          "receive_wallet_address": "0xtesttesttesttesttesttesttest",
          "body_schema": null,
          "target": {
            "kind": "http_proxy",
            "original_target_url": "https://www.baidu.com",
            "forward_method": "preserve",
            "path_rewrite": {},
            "headers_policy": [],
            "origin_signature_required": true
          },
          "origin_auth": {"active_secret_version": 1},
          "policy": {
            "policy_id": "60c3208e-5137-4a22-9e55-0094a458732e",
            "buyer_access": "Public",
            "rate_limit_rpm": 100,
            "max_request_size": 1000000,
            "timeout_seconds": 6,
            "payment_retry_attempts": 3,
            "schema_validation_required": true,
            "facilitator": "coinbase",
            "cache_billing_mode": "disabled",
            "cache_hit_discount_bps": 0
          },
          "snapshot_revision": 4
        }"#;

        let snap = parse_snapshot_json(raw).unwrap();

        assert_eq!(snap.endpoint_type, None);
    }

    #[test]
    fn db_snapshot_defaults_fee_bps_when_schema_has_no_column() {
        let row = DbX402EndpointSnapshotRow {
            endpoint_id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            agent_id: None,
            status: "active".to_string(),
            name: "Test Agent".to_string(),
            slug: "test-agent".to_string(),
            endpoint_type: Some("agent".to_string()),
            method: "POST".to_string(),
            path: "/test-agent".to_string(),
            pricing_model: "per_call".to_string(),
            price_amount: "1.00000000".to_string(),
            asset: "USDC".to_string(),
            network: "base".to_string(),
            receive_wallet_address: "0xtesttesttesttesttesttesttest".to_string(),
            body_schema: serde_json::json!({
                "type": "object",
                "required": ["wallet_address", "chain"],
            }),
            target_kind: "http_proxy".to_string(),
            original_target_url: "https://example.com".to_string(),
            target_forward_method: "preserve".to_string(),
            target_path_rewrite: serde_json::json!({}),
            target_headers_policy: serde_json::json!([
                {"name": "X-API-Key", "value": "secret"},
                {"name": "X-Client-Id", "value": "abc123"}
            ]),
            origin_signature_required: true,
            active_secret_version: 1,
            policy_id: Uuid::new_v4(),
            buyer_access: "Public".to_string(),
            rate_limit_rpm: 100,
            max_request_size: 1_000_000,
            timeout_seconds: 6,
            payment_retry_attempts: 3,
            schema_validation_required: true,
            facilitator: Some("coinbase".to_string()),
            cache_billing_mode: "disabled".to_string(),
            cache_hit_discount_bps: 0,
            snapshot_revision: 4,
            updated_at: Utc::now(),
        };

        let snap = snapshot_from_db_row(row).unwrap();

        assert_eq!(snap.fee_bps, 0);
        assert_eq!(snap.endpoint_type.as_deref(), Some("agent"));
        assert_eq!(snap.body_schema["required"][0], "wallet_address");
        assert_eq!(snap.target.headers_policy.len(), 2);
        assert_eq!(snap.target.headers_policy[0].name, "X-API-Key");
        assert_eq!(
            snap.target.headers_policy[0].value.as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn normalize_endpoint_type_trims_lowercases_and_removes_empty_values() {
        assert_eq!(
            normalize_endpoint_type(Some("agent".to_string())).as_deref(),
            Some("agent")
        );
        assert_eq!(
            normalize_endpoint_type(Some(" HTTP_API ".to_string())).as_deref(),
            Some("http_api")
        );
        assert_eq!(normalize_endpoint_type(Some("".to_string())), None);
        assert_eq!(normalize_endpoint_type(None), None);
    }
}
