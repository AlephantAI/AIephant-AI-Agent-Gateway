//! Map bare `model_id` (no `provider/` prefix) to one or more registered
//! `"{providers.code}/{model_id}"` full names. Matches rows successfully parsed
//! as [`ModelId`] in [`super::provider_db_config::build_from_db`],
//! rebuilt/refreshed with `ProvidersConfig`.

use std::collections::HashMap;

/// Lowercased `model_id` → full names for that id in the enabled gateway
/// registry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BareModelExpandIndex {
    inner: HashMap<String, Vec<String>>,
}

impl BareModelExpandIndex {
    /// Register on rows where `ModelId::from_str_and_provider` succeeded; under
    /// the same `key`, skip if `code/model` already exists.
    pub fn push(&mut self, provider_code: &str, model_id_in_db: &str) {
        if model_id_in_db.is_empty() {
            return;
        }
        let key = model_id_in_db.to_ascii_lowercase();
        let full = format!("{provider_code}/{model_id_in_db}");
        let v = self.inner.entry(key).or_default();
        if !v.contains(&full) {
            v.push(full);
        }
    }

    /// `code/model` list for bare `model_id` from policy; empty `Vec` if none.
    #[must_use]
    pub fn gateway_models_for_bare_id(
        &self,
        bare_trimmed: &str,
    ) -> Vec<String> {
        self.inner
            .get(&bare_trimmed.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedupes_identical_code_model() {
        let mut idx = BareModelExpandIndex::default();
        idx.push("openai", "gpt-4o");
        idx.push("openai", "gpt-4o");
        let v = idx.gateway_models_for_bare_id("gpt-4o");
        assert_eq!(v, vec!["openai/gpt-4o".to_string()]);
    }

    #[test]
    fn same_model_id_two_providers_two_entries() {
        let mut idx = BareModelExpandIndex::default();
        idx.push("openai", "x");
        idx.push("anthropic", "x");
        let v = idx.gateway_models_for_bare_id("x");
        assert_eq!(v.len(), 2);
        assert!(v.contains(&"openai/x".to_string()));
        assert!(v.contains(&"anthropic/x".to_string()));
    }
}
