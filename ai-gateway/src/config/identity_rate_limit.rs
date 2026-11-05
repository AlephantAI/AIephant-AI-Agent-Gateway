use serde::{Deserialize, Serialize};

use crate::{
    config::{Config, client_ip_rate_limit::ClientIpRateLimitBackend},
    error::init::InitError,
    types::extensions::RequestKind,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum IdentityRateLimitScope {
    Models,
    Tools,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct IdentityRateLimitConfig {
    pub enabled: bool,
    pub requests: u32,
    pub window_seconds: u32,
    pub backend: ClientIpRateLimitBackend,
    pub redis_key_prefix: String,
    pub scopes: Vec<IdentityRateLimitScope>,
}

impl Default for IdentityRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            requests: 100,
            window_seconds: 60,
            backend: ClientIpRateLimitBackend::Memory,
            redis_key_prefix: "gw:identityrl:".into(),
            scopes: vec![
                IdentityRateLimitScope::Models,
                IdentityRateLimitScope::Tools,
            ],
        }
    }
}

impl IdentityRateLimitConfig {
    pub fn applies_to(&self, kind: Option<RequestKind>) -> bool {
        let scope = match kind {
            Some(
                RequestKind::Router
                | RequestKind::UnifiedApi
                | RequestKind::DirectProxy
                | RequestKind::CustomProvider,
            ) => IdentityRateLimitScope::Models,
            Some(RequestKind::AgentTools) => IdentityRateLimitScope::Tools,
            _ => return false,
        };
        self.enabled && self.scopes.contains(&scope)
    }
}

pub fn validate_identity_rate_limit(config: &Config) -> Result<(), InitError> {
    let Some(limit) = config
        .global
        .identity_rate_limit
        .as_ref()
        .filter(|limit| limit.enabled)
    else {
        return Ok(());
    };
    if limit.requests == 0
        || !(1..=86400).contains(&limit.window_seconds)
        || limit.redis_key_prefix.is_empty()
        || limit.scopes.is_empty()
    {
        return Err(InitError::InvalidRateLimitConfig(
            "identity-rate-limit requires positive requests, a 1..86400 second window, a key prefix and at least one scope",
        ));
    }
    if config.alephant.is_auth_disabled() {
        return Err(InitError::InvalidRateLimitConfig(
            "identity-rate-limit requires authenticated identities",
        ));
    }
    if matches!(limit.backend, ClientIpRateLimitBackend::Redis)
        && config.request_log.log_queue_redis_url.is_none()
    {
        return Err(InitError::InvalidRateLimitConfig(
            "identity-rate-limit Redis backend requires REDIS_URL",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_apply_only_to_configured_authenticated_model_and_tool_paths() {
        let mut config = IdentityRateLimitConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(config.applies_to(Some(RequestKind::UnifiedApi)));
        assert!(config.applies_to(Some(RequestKind::AgentTools)));
        assert!(!config.applies_to(Some(RequestKind::AgentEvents)));
        assert!(!config.applies_to(Some(RequestKind::X402Agent)));
        assert!(!config.applies_to(None));
        config.scopes = vec![IdentityRateLimitScope::Tools];
        assert!(!config.applies_to(Some(RequestKind::UnifiedApi)));
        assert!(config.applies_to(Some(RequestKind::AgentTools)));
        config.enabled = false;
        assert!(!config.applies_to(Some(RequestKind::AgentTools)));
    }

    #[test]
    fn invalid_limits_missing_identity_and_missing_redis_fail_startup_validation() {
        let mut config = Config::default();
        config.global.identity_rate_limit = Some(IdentityRateLimitConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(validate_identity_rate_limit(&config).is_err());
        config.alephant.features = crate::config::alephant::AlephantFeatures::Auth;
        assert!(validate_identity_rate_limit(&config).is_ok());
        config.global.identity_rate_limit.as_mut().unwrap().requests = 0;
        assert!(validate_identity_rate_limit(&config).is_err());
        let limit = config.global.identity_rate_limit.as_mut().unwrap();
        limit.requests = 1;
        limit.backend = ClientIpRateLimitBackend::Redis;
        assert!(validate_identity_rate_limit(&config).is_err());
        config.request_log.log_queue_redis_url = Some("redis://127.0.0.1:6379".parse().unwrap());
        assert!(validate_identity_rate_limit(&config).is_ok());
    }
}
