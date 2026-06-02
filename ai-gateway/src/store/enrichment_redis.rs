//! Redis key layout and cached payload for VK department enrichment (auth).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ENRICHMENT_CACHE_KEY_PREFIX: &str = "enrichment:vk:";
/// Safety-net TTL when NOTIFY is missed; near-real-time invalidation is
/// NOTIFY-driven.
pub const ENRICHMENT_CACHE_TTL_SECS: u64 = 86400 * 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDepartmentEnrichment {
    pub workspace_type: Option<String>,
    pub department_id: Option<Uuid>,
}

#[must_use]
pub fn enrichment_cache_key(virtual_key_id: Uuid) -> String {
    format!("{ENRICHMENT_CACHE_KEY_PREFIX}{virtual_key_id}")
}
