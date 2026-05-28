use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SemanticCacheConfig {
    pub default_threshold_millis: u16,
    pub default_ttl_seconds: u64,
    pub qdrant: QdrantConfig,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            default_threshold_millis: 900,
            default_ttl_seconds: 3600,
            qdrant: QdrantConfig::default(),
        }
    }
}

impl SemanticCacheConfig {
    #[must_use]
    pub fn default_threshold(&self) -> f32 {
        f32::from(self.default_threshold_millis) / 1000.0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[derive(Default)]
pub struct QdrantConfig {
    pub url: String,
    /// Deprecated: semantic cache now derives final Qdrant collection names
    /// from embedding provider, model, and vector dimension at runtime.
    pub collection: Option<String>,
    pub api_key: Option<String>,
}
