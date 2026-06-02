use serde::{Deserialize, Serialize};
use url::Url;

use crate::types::secret::Secret;

#[derive(
    Default, Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash,
)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum AlephantFeatures {
    /// No features enabled
    ///
    /// **Note:** this means no authentication checks, so any request to the
    /// gateway will be able to use your provider API keys!
    #[default]
    None,
    /// Authentication only.
    Auth,
    /// Authentication and observability.
    Observability,
    /// Authentication and prompts.
    #[serde(rename = "__prompts")]
    Prompts,
    /// Authentication and observability (does not include prompts).
    All,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AlephantConfig {
    /// The API key to authenticate the AI Gateway to the Alephant control
    /// plane.
    #[serde(default = "default_api_key")]
    pub api_key: Secret<String>,
    /// The URL of the downstream log-collector service that receives request
    /// logs (also used for prompt queries).
    #[serde(default = "default_log_collector_url")]
    pub log_collector_url: Url,
    /// Bearer token used only for x402 payment-log HTTP fallback.
    #[serde(default = "default_logs_collector_x402_http_auth_token")]
    pub logs_collector_x402_http_auth_token: Secret<String>,
    /// The mode of Alephant features to enable.
    #[serde(default)]
    pub features: AlephantFeatures,
}

impl AlephantConfig {
    #[must_use]
    pub fn is_auth_enabled(&self) -> bool {
        self.features != AlephantFeatures::None
    }

    #[must_use]
    pub fn is_auth_disabled(&self) -> bool {
        self.features == AlephantFeatures::None
    }

    #[must_use]
    pub fn is_observability_enabled(&self) -> bool {
        self.features == AlephantFeatures::All
            || self.features == AlephantFeatures::Observability
    }

    #[must_use]
    pub fn is_prompts_enabled(&self) -> bool {
        self.features == AlephantFeatures::All
            || self.features == AlephantFeatures::Prompts
    }
}

impl Default for AlephantConfig {
    fn default() -> Self {
        Self {
            api_key: default_api_key(),
            log_collector_url: default_log_collector_url(),
            logs_collector_x402_http_auth_token:
                default_logs_collector_x402_http_auth_token(),
            features: AlephantFeatures::None,
        }
    }
}

fn default_api_key() -> Secret<String> {
    // ALEPHANT_CONTROL_PLANE_API_KEY takes priority.
    const LEGACY_CONTROL_PLANE_API_KEY_ENV: &str =
        concat!("HELI", "CONE_CONTROL_PLANE_API_KEY");
    let val = std::env::var("ALEPHANT_CONTROL_PLANE_API_KEY")
        .or_else(|_| std::env::var(LEGACY_CONTROL_PLANE_API_KEY_ENV))
        .unwrap_or_else(|_| "sk-alephant-...".to_string());
    Secret::from(val)
}

fn default_log_collector_url() -> Url {
    const LOG_COLLECTOR_URL_ENV: &str =
        "AI_GATEWAY__ALEPHANT__LOG_COLLECTOR_URL";
    const DEFAULT_LOG_COLLECTOR_URL: &str = "https://api.alephant.io";
    std::env::var(LOG_COLLECTOR_URL_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            DEFAULT_LOG_COLLECTOR_URL.parse().expect("valid URL")
        })
}

fn default_logs_collector_x402_http_auth_token() -> Secret<String> {
    Secret::from(
        std::env::var("LOGS_COLLECTOR_X402_HTTP_AUTH_TOKEN")
            .unwrap_or_else(|_| String::new()),
    )
}

#[cfg(feature = "testing")]
impl crate::tests::TestDefault for AlephantConfig {
    fn test_default() -> Self {
        Self {
            log_collector_url: "http://localhost:8585".parse().unwrap(),
            features: AlephantFeatures::All,
            api_key: default_api_key(),
            logs_collector_x402_http_auth_token:
                default_logs_collector_x402_http_auth_token(),
        }
    }
}

// This manual deserialize impl is only required for backwards compatibility so
// that we can support the old `authentication` and `observability` boolean
// fields.
#[allow(clippy::too_many_lines)]
impl<'de> Deserialize<'de> for AlephantConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use std::fmt;

        use serde::de::{self, MapAccess, Visitor};

        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "kebab-case")]
        enum Field {
            ApiKey,
            LogCollectorUrl,
            BaseUrl,
            LogsCollectorX402HttpAuthToken,
            WebsocketUrl,
            Features,
            Authentication,
            Observability,
            #[serde(rename = "__prompts")]
            Prompts,
        }

        struct AlephantConfigVisitor;

        impl<'de> Visitor<'de> for AlephantConfigVisitor {
            type Value = AlephantConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct AlephantConfig")
            }

            fn visit_map<V>(
                self,
                mut map: V,
            ) -> Result<AlephantConfig, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut api_key = None;
                let mut log_collector_url = None;
                let mut legacy_base_url = None;
                let mut logs_collector_x402_http_auth_token = None;
                let mut features = None;
                let mut authentication = None;
                let mut observability = None;
                let mut prompts = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::ApiKey => {
                            if api_key.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "api_key",
                                ));
                            }
                            api_key = Some(map.next_value()?);
                        }
                        Field::LogCollectorUrl => {
                            if log_collector_url.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "log_collector_url",
                                ));
                            }
                            log_collector_url = Some(map.next_value()?);
                        }
                        Field::BaseUrl => {
                            if legacy_base_url.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "base_url",
                                ));
                            }
                            legacy_base_url = Some(map.next_value()?);
                        }
                        Field::LogsCollectorX402HttpAuthToken => {
                            if logs_collector_x402_http_auth_token.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "logs_collector_x402_http_auth_token",
                                ));
                            }
                            logs_collector_x402_http_auth_token =
                                Some(map.next_value()?);
                        }
                        Field::WebsocketUrl => {
                            // Accepted for backwards compat; value is ignored.
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                        Field::Features => {
                            if features.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "features",
                                ));
                            }
                            features = Some(map.next_value()?);
                        }
                        Field::Authentication => {
                            if authentication.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "authentication",
                                ));
                            }
                            authentication = Some(map.next_value()?);
                        }
                        Field::Observability => {
                            if observability.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "observability",
                                ));
                            }
                            observability = Some(map.next_value()?);
                        }
                        Field::Prompts => {
                            if prompts.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "prompts",
                                ));
                            }
                            prompts = Some(map.next_value()?);
                        }
                    }
                }

                // Determine features precedence:
                // 1. If features is set, use it.
                // 2. Otherwise, use authentication/observability/prompts
                //    booleans.
                // 3. Otherwise, default to None.

                let features = if let Some(f) = features {
                    f
                } else {
                    match (authentication, observability, prompts) {
                        (_, Some(true), Some(true)) => AlephantFeatures::All,
                        (_, Some(true), Some(false) | None) => {
                            AlephantFeatures::Observability
                        }
                        (_, Some(false) | None, Some(true)) => {
                            AlephantFeatures::Prompts
                        }
                        (
                            Some(true),
                            Some(false) | None,
                            Some(false) | None,
                        ) => AlephantFeatures::Auth,
                        _ => AlephantFeatures::None,
                    }
                };

                Ok(AlephantConfig {
                    api_key: api_key.unwrap_or_else(default_api_key),
                    log_collector_url: log_collector_url
                        .or(legacy_base_url)
                        .unwrap_or_else(default_log_collector_url),
                    logs_collector_x402_http_auth_token:
                        logs_collector_x402_http_auth_token.unwrap_or_else(
                            default_logs_collector_x402_http_auth_token,
                        ),
                    features,
                })
            }
        }

        const FIELDS: &[&str] = &[
            "api_key",
            "log_collector_url",
            "base_url",
            "logs_collector_x402_http_auth_token",
            "websocket_url",
            "features",
            "authentication",
            "observability",
            "__prompts",
        ];
        deserializer.deserialize_struct(
            "AlephantConfig",
            FIELDS,
            AlephantConfigVisitor,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use super::*;

    fn log_collector_url_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn log_collector_url_defaults_to_production_api_without_env() {
        let _guard = log_collector_url_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = "AI_GATEWAY__ALEPHANT__LOG_COLLECTOR_URL";
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }

        let yaml = r#"api-key: "sk-test""#;
        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(
            config.log_collector_url,
            "https://api.alephant.io".parse().unwrap()
        );

        match previous {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn logs_collector_x402_http_auth_token_defaults_from_flat_env() {
        let _guard = log_collector_url_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = "LOGS_COLLECTOR_X402_HTTP_AUTH_TOKEN";
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, "x402-token");
        }

        let yaml = r#"api-key: "sk-test""#;
        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(
            config.logs_collector_x402_http_auth_token.expose(),
            "x402-token"
        );

        match previous {
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn test_deserialize_features_field_only() {
        let yaml = r#"
api-key: "sk-test-key"
log-collector-url: "https://example.com"
websocket-url: "wss://example.com/ws"
features: "all"
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::All);
    }

    #[test]
    fn base_url_deserializes_as_legacy_log_collector_url_alias() {
        let yaml = r#"
api-key: "sk-test-key"
base-url: "https://legacy.example.com"
features: "all"
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();

        assert_eq!(
            config.log_collector_url,
            "https://legacy.example.com".parse().unwrap()
        );
    }

    #[test]
    fn log_collector_url_takes_precedence_over_legacy_base_url() {
        let yaml = r#"
api-key: "sk-test-key"
base-url: "https://legacy.example.com"
log-collector-url: "https://logs.example.com"
features: "all"
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();

        assert_eq!(
            config.log_collector_url,
            "https://logs.example.com".parse().unwrap()
        );
    }

    #[test]
    fn test_deserialize_all_flags_true() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: true
observability: true
__prompts: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::All);
    }

    #[test]
    fn test_deserialize_auth_true_others_false() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: true
observability: false
__prompts: false
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::Auth);
    }

    #[test]
    fn test_deserialize_observability_true_others_false() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: false
observability: true
__prompts: false
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::Observability);
    }

    #[test]
    fn test_deserialize_prompts_true_others_false() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: false
observability: false
__prompts: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::Prompts);
    }

    #[test]
    fn test_deserialize_all_flags_false() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: false
observability: false
__prompts: false
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::None);
    }

    #[test]
    fn test_deserialize_auth_true_only() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::Auth);
    }

    #[test]
    fn test_deserialize_observability_true_only() {
        let yaml = r#"
api-key: "sk-test-key"
observability: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::Observability);
    }

    #[test]
    fn test_deserialize_prompts_true_only() {
        let yaml = r#"
api-key: "sk-test-key"
__prompts: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::Prompts);
    }

    #[test]
    fn test_deserialize_auth_false_only() {
        let yaml = r#"
api-key: "sk-test-key"
authentication: false
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::None);
    }

    #[test]
    fn test_deserialize_observability_false_only() {
        let yaml = r#"
api-key: "sk-test-key"
observability: false
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::None);
    }

    #[test]
    fn test_deserialize_prompts_false_only() {
        let yaml = r#"
api-key: "sk-test-key"
__prompts: false
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::None);
    }

    #[test]
    fn test_deserialize_no_feature_fields() {
        let yaml = r#"
api-key: "sk-test-key"
log-collector-url: "https://example.com"
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        assert_eq!(config.features, AlephantFeatures::None);
    }

    #[test]
    fn test_deserialize_features_takes_precedence() {
        let yaml = r#"
api-key: "sk-test-key"
features: "auth"
authentication: true
observability: true
__prompts: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        // features field should take precedence over
        // auth/observability/__prompts
        assert_eq!(config.features, AlephantFeatures::Auth);
    }

    #[test]
    fn test_deserialize_features_none_with_legacy_fields() {
        let yaml = r#"
api-key: "sk-test-key"
features: "none"
authentication: true
observability: true
__prompts: true
"#;

        let config: AlephantConfig = serde_yml::from_str(yaml).unwrap();
        // features field should take precedence
        assert_eq!(config.features, AlephantFeatures::None);
    }

    #[test]
    fn test_deserialize_all_features_variants() {
        let test_cases = vec![
            ("none", AlephantFeatures::None),
            ("auth", AlephantFeatures::Auth),
            ("observability", AlephantFeatures::Observability),
            ("__prompts", AlephantFeatures::Prompts),
            ("all", AlephantFeatures::All),
        ];

        for (feature_str, expected_feature) in test_cases {
            let yaml = format!(
                r#"
api-key: "sk-test-key"
features: "{feature_str}"
"#
            );

            let config: AlephantConfig = serde_yml::from_str(&yaml).unwrap();
            assert_eq!(
                config.features, expected_feature,
                "Failed for feature: {feature_str}"
            );
        }
    }

    #[test]
    fn test_helper_methods() {
        let auth_config = AlephantConfig {
            features: AlephantFeatures::Auth,
            ..Default::default()
        };
        assert!(auth_config.is_auth_enabled());
        assert!(!auth_config.is_auth_disabled());
        assert!(!auth_config.is_observability_enabled());

        let all_config = AlephantConfig {
            features: AlephantFeatures::All,
            ..Default::default()
        };
        assert!(all_config.is_auth_enabled());
        assert!(!all_config.is_auth_disabled());
        assert!(all_config.is_observability_enabled());

        let none_config = AlephantConfig {
            features: AlephantFeatures::None,
            ..Default::default()
        };
        assert!(!none_config.is_auth_enabled());
        assert!(none_config.is_auth_disabled());
        assert!(!none_config.is_observability_enabled());
    }
}
