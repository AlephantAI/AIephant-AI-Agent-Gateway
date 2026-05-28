//! `global.client-ip-rate-limit` — global rate-limit config (design in
//! `docs/plans/`).

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::error::init::InitError;

/// Validate `global.client-ip-rate-limit`: if `enabled` and
/// `requests-per-second == 0`, fail startup.
pub fn validate_global_client_ip_rate_limit(
    global: &super::MiddlewareConfig,
) -> Result<(), InitError> {
    let Some(ref c) = global.client_ip_rate_limit else {
        return Ok(());
    };
    if !c.enabled {
        return Ok(());
    }
    if c.requests_per_second == 0 {
        return Err(InitError::InvalidRateLimitConfig(
            "global.client-ip-rate-limit: requests-per-second must be >= 1 \
             when enabled",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ClientIpRateLimitConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_requests_per_second")]
    pub requests_per_second: u32,
    #[serde(default)]
    pub backend: ClientIpRateLimitBackend,
    #[serde(default, deserialize_with = "deserialize_cidr_list")]
    pub trusted_proxy_cidrs: Vec<String>,
    #[serde(default = "default_redis_key_prefix")]
    pub redis_key_prefix: String,
}

impl Default for ClientIpRateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            requests_per_second: default_requests_per_second(),
            backend: ClientIpRateLimitBackend::default(),
            trusted_proxy_cidrs: Vec::new(),
            redis_key_prefix: default_redis_key_prefix(),
        }
    }
}

#[derive(
    Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq, Hash,
)]
#[serde(rename_all = "kebab-case")]
pub enum ClientIpRateLimitBackend {
    #[default]
    Memory,
    Redis,
}

const fn default_requests_per_second() -> u32 {
    0
}

fn default_redis_key_prefix() -> String {
    "gw:iprl:".to_string()
}

/// Accepts YAML arrays, JSON-array strings, or
/// `AI_GATEWAY__...__TRUSTED_PROXY_CIDRS__0`-style maps.
fn deserialize_cidr_list<'de, D>(
    deserializer: D,
) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .map(|v| {
                v.as_str().map(str::to_owned).ok_or_else(|| {
                    D::Error::custom(
                        "trusted_proxy_cidrs: array items must be strings",
                    )
                })
            })
            .collect(),
        serde_json::Value::String(s) => {
            serde_json::from_str(&s).map_err(D::Error::custom)
        }
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<(usize, String)> = map
                .into_iter()
                .filter_map(|(k, v)| {
                    let idx = k.parse::<usize>().ok()?;
                    Some((idx, v.as_str()?.to_owned()))
                })
                .collect();
            pairs.sort_by_key(|(i, _)| *i);
            if pairs.is_empty() {
                return Err(D::Error::custom(
                    "trusted_proxy_cidrs: expected non-empty indexed map",
                ));
            }
            Ok(pairs.into_iter().map(|(_, s)| s).collect())
        }
        _ => Err(D::Error::custom(
            "trusted_proxy_cidrs: expected array, JSON-array string, indexed \
             map, or null",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MiddlewareConfig;

    #[test]
    fn deserialize_cidr_list_accepts_json_array_string() {
        let v: Vec<String> =
            serde_json::from_str(r#"["10.0.0.0/8","192.168.0.0/16"]"#)
                .expect("fixture");
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn validate_rejects_enabled_with_zero_rps() {
        let global = MiddlewareConfig {
            client_ip_rate_limit: Some(ClientIpRateLimitConfig {
                enabled: true,
                requests_per_second: 0,
                ..ClientIpRateLimitConfig::default()
            }),
            ..Default::default()
        };
        assert!(validate_global_client_ip_rate_limit(&global).is_err());
    }

    #[test]
    fn serde_round_trip_kebab_case() {
        let yaml = r"
enabled: true
requests-per-second: 7
backend: redis
trusted-proxy-cidrs:
  - 10.0.0.0/8
redis-key-prefix: 'gw:test:'
";
        let c: ClientIpRateLimitConfig =
            serde_yml::from_str(yaml).expect("deserialize");
        assert!(c.enabled);
        assert_eq!(c.requests_per_second, 7);
        assert_eq!(c.backend, ClientIpRateLimitBackend::Redis);
        assert_eq!(c.trusted_proxy_cidrs, vec!["10.0.0.0/8".to_string()]);
        assert_eq!(c.redis_key_prefix, "gw:test:");
        let again: ClientIpRateLimitConfig =
            serde_json::from_value(serde_json::to_value(&c).expect("to json"))
                .expect("from json");
        assert_eq!(c, again);
    }
}
