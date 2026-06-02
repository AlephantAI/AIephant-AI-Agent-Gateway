//! `global.gateway-in-flight-limit` — max in-flight HTTP requests for the whole
//! gateway (not per-IP buckets).

use serde::{Deserialize, Serialize};

use crate::error::init::InitError;

/// Validate `global.gateway-in-flight-limit`: if `enabled` and `max-concurrent
/// == 0`, fail startup.
pub fn validate_global_gateway_in_flight_limit(
    global: &super::MiddlewareConfig,
) -> Result<(), InitError> {
    let Some(ref c) = global.gateway_in_flight_limit else {
        return Ok(());
    };
    if !c.enabled {
        return Ok(());
    }
    if c.max_concurrent == 0 {
        return Err(InitError::InvalidGatewayInFlightLimitConfig(
            "global.gateway-in-flight-limit: max-concurrent must be >= 1 when \
             enabled"
                .to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct GatewayInFlightLimitConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default)]
    pub backend: GatewayInFlightBackend,
    #[serde(default = "default_redis_key_prefix")]
    pub redis_key_prefix: String,
}

impl Default for GatewayInFlightLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent: default_max_concurrent(),
            backend: GatewayInFlightBackend::default(),
            redis_key_prefix: default_redis_key_prefix(),
        }
    }
}

#[derive(
    Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, Hash,
)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayInFlightBackend {
    #[default]
    Memory,
    Redis,
}

const fn default_max_concurrent() -> u32 {
    0
}

fn default_redis_key_prefix() -> String {
    "gw:gifl:".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MiddlewareConfig;

    #[test]
    fn validate_rejects_enabled_with_zero_max() {
        let global = MiddlewareConfig {
            gateway_in_flight_limit: Some(GatewayInFlightLimitConfig {
                enabled: true,
                max_concurrent: 0,
                ..GatewayInFlightLimitConfig::default()
            }),
            ..Default::default()
        };
        assert!(validate_global_gateway_in_flight_limit(&global).is_err());
    }
}
