use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotSource {
    Redis,
    Db,
}

impl SnapshotSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Redis => "redis",
            Self::Db => "db",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct X402EndpointSnapshot {
    pub endpoint_id: Uuid,
    pub workspace_id: Uuid,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    pub status: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub endpoint_type: Option<String>,
    pub method: String,
    pub path: String,
    pub pricing_model: String,
    pub price_amount: String,
    pub asset: String,
    pub network: String,
    pub receive_wallet_address: String,
    #[serde(default)]
    pub fee_bps: i32,
    #[serde(default)]
    pub body_schema: serde_json::Value,
    pub target: X402TargetSnapshot,
    pub origin_auth: X402OriginAuthSnapshot,
    pub policy: X402PolicySnapshot,
    pub snapshot_revision: i64,
    #[serde(default)]
    pub config_revision: Option<i64>,
    #[serde(default)]
    pub compiled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct X402TargetSnapshot {
    pub kind: String,
    pub original_target_url: String,
    pub forward_method: String,
    #[serde(default)]
    pub path_rewrite: serde_json::Value,
    #[serde(default)]
    pub headers_policy: Vec<X402TargetHeaderPolicyItem>,
    #[serde(default)]
    pub origin_signature_required: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct X402TargetHeaderPolicyItem {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct X402OriginAuthSnapshot {
    pub active_secret_version: i32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct X402PolicySnapshot {
    pub policy_id: Uuid,
    pub buyer_access: String,
    pub rate_limit_rpm: i32,
    pub max_request_size: i32,
    pub timeout_seconds: i32,
    pub payment_retry_attempts: i32,
    pub schema_validation_required: bool,
    #[serde(default)]
    pub facilitator: Option<String>,
    pub cache_billing_mode: String,
    pub cache_hit_discount_bps: i32,
}

#[derive(Debug, Clone)]
pub struct ResolvedSnapshot {
    pub snapshot: X402EndpointSnapshot,
    pub source: SnapshotSource,
}
