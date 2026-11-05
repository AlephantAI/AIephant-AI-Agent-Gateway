use std::collections::HashSet;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::init::InitError, store::router::DbVirtualKey, types::provider::InferenceProvider,
    virtual_key::legacy_key::hash_key,
};

/// Explicit local identities; credential values are read only from the environment.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LocalAuthConfig {
    pub keys: Vec<LocalKeyConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LocalKeyConfig {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub key_env: String,
    pub provider: InferenceProvider,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub blocked_models: Option<Vec<String>>,
}

impl LocalAuthConfig {
    pub fn validate(&self) -> Result<(), InitError> {
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        if self.keys.is_empty() {
            return Err(invalid("keys must not be empty"));
        }
        for key in &self.keys {
            if key.id.is_nil() || key.workspace_id.is_nil() || !ids.insert(key.id) {
                return Err(invalid(
                    "key and workspace IDs must be non-nil; key IDs must be unique",
                ));
            }
            if key.key_env.is_empty()
                || key.key_env.starts_with(|c: char| c.is_ascii_digit())
                || !key
                    .key_env
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !names.insert(&key.key_env)
            {
                return Err(invalid(
                    "key-env must be a unique environment variable name",
                ));
            }
        }
        Ok(())
    }

    pub fn load(&self) -> Result<Vec<DbVirtualKey>, InitError> {
        self.load_keys(|name| std::env::var(name).ok())
    }

    fn load_keys(
        &self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<DbVirtualKey>, InitError> {
        self.validate()?;
        let mut hashes = HashSet::new();
        self.keys
            .iter()
            .map(|key| {
                let value = lookup(&key.key_env)
                    .ok_or_else(|| invalid("a key-env credential is missing"))?;
                if value.len() < 32 || !value.bytes().all(|c| c.is_ascii_graphic()) {
                    return Err(invalid(
                        "credentials must contain at least 32 non-whitespace ASCII bytes",
                    ));
                }
                let key_hash = hash_key(&value);
                if !hashes.insert(key_hash.clone()) {
                    return Err(invalid(
                        "credentials must be unique across local identities",
                    ));
                }
                Ok(DbVirtualKey {
                    id: key.id,
                    workspace_id: key.workspace_id,
                    master_key_id: key.id,
                    key_hash,
                    key_prefix: "local".into(),
                    label: "local".into(),
                    entity_type: None,
                    entity_id: None,
                    status: "active".into(),
                    expires_at: None,
                    deleted_at: None,
                    updated_at: Utc::now(),
                    rate_limit_rpm: None,
                    rate_limit_rph: None,
                    allowed_models: key.allowed_models.clone(),
                    blocked_models: key.blocked_models.clone(),
                    subscription_log_limit: 90,
                })
            })
            .collect()
    }
}

fn invalid(message: &str) -> InitError {
    InitError::InvalidLocalAuth(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LocalAuthConfig {
        LocalAuthConfig {
            keys: vec![LocalKeyConfig {
                id: Uuid::from_u128(1),
                workspace_id: Uuid::from_u128(2),
                key_env: "LOCAL_KEY".into(),
                provider: InferenceProvider::OpenAI,
                allowed_models: Some(vec!["openai/gpt-4o".into()]),
                blocked_models: None,
            }],
        }
    }

    #[test]
    fn credentials_are_hashed_and_preserve_trusted_scope_and_model_policy() {
        let rows = config()
            .load_keys(|_| Some("a-secure-local-credential-with-32-bytes".into()))
            .unwrap();
        assert_eq!(rows[0].id, Uuid::from_u128(1));
        assert_eq!(rows[0].workspace_id, Uuid::from_u128(2));
        assert_ne!(rows[0].key_hash, "a-secure-local-credential-with-32-bytes");
        assert_eq!(rows[0].allowed_models, Some(vec!["openai/gpt-4o".into()]));
    }

    #[test]
    fn missing_weak_and_duplicate_credentials_fail_without_echoing_values() {
        assert!(config().load_keys(|_| None).is_err());
        let error = config().load_keys(|_| Some("secret".into())).unwrap_err();
        assert!(!error.to_string().contains("secret"));
        let mut duplicate = config();
        let mut second = duplicate.keys[0].clone();
        second.id = Uuid::from_u128(3);
        second.key_env = "OTHER_KEY".into();
        duplicate.keys.push(second);
        assert!(
            duplicate
                .load_keys(|_| Some("a-secure-local-credential-with-32-bytes".into()))
                .is_err()
        );
    }

    #[test]
    fn empty_duplicate_and_nil_identity_configuration_is_rejected() {
        assert!(LocalAuthConfig { keys: vec![] }.validate().is_err());
        let mut duplicate = config();
        duplicate.keys.push(duplicate.keys[0].clone());
        assert!(duplicate.validate().is_err());
        let mut nil = config();
        nil.keys[0].workspace_id = Uuid::nil();
        assert!(nil.validate().is_err());
    }
}
