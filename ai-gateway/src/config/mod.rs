pub mod alephant;
pub mod balance;
pub mod client_ip_rate_limit;
pub mod cloudflare_kv;
pub mod database;
pub mod deployment_target;
pub mod discover;
pub mod dispatcher;
pub mod fallback_bridge;
pub mod fallback_policy;
pub mod gateway_in_flight_limit;
pub mod mapper_profiles;
pub mod model_mapping;
pub mod monitor;
pub mod policy;
pub mod providers;
pub mod redis;
pub mod request_log;
pub mod response_headers;
pub mod retry;
pub mod router;
pub mod s3;
pub mod semantic_cache;
pub mod server;
pub mod tikv_kv;
pub mod validation;
use std::path::PathBuf;

use config::ConfigError;
use displaydoc::Display;
use json_patch::merge;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{
    error::init::InitError,
    types::{provider::InferenceProvider, secret::Secret},
};

const ROUTER_ID_REGEX: &str = r"^[A-Za-z0-9_-]{1,12}$";
const DEFAULT_CONFIG_PATH: &str = "/etc/ai-gateway/config.yaml";
/// When no config file is passed on the CLI, this path is loaded after
/// [`DEFAULT_CONFIG_PATH`] (if present). Sources are ordered **`config.yaml` →
/// `alephant-cloud.yaml` → `AI_GATEWAY__*` env**, so environment variables
/// override YAML (including `cloudflare-kv`, `alephant.base-url`, etc.).
const DEFAULT_ALEPHANT_CLOUD_PATH: &str = "/etc/ai-gateway/alephant-cloud.yaml";
const LEGACY_CONTROL_PLANE_API_KEY_ENV: &str =
    concat!("HELI", "CONE_CONTROL_PLANE_API_KEY");

#[derive(Debug, Error, Display)]
pub enum Error {
    /// error collecting config sources: {0}
    Source(#[from] ConfigError),
    /// deserialization error for input config: {0}
    InputConfigDeserialization(#[from] serde_path_to_error::Error<ConfigError>),
    /// deserialization error for merged config: {0}
    MergedConfigDeserialization(
        #[from] serde_path_to_error::Error<serde_json::Error>,
    ),
    /// URL parsing error: {0}
    UrlParse(#[from] url::ParseError),
    /// invalid S3_URL_STYLE: {0} (expected path or virtual-host)
    InvalidS3UrlStyle(String),
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct MiddlewareConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries: Option<self::retry::RetryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip_rate_limit:
        Option<self::client_ip_rate_limit::ClientIpRateLimitConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_in_flight_limit:
        Option<self::gateway_in_flight_limit::GatewayInFlightLimitConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_event_broadcast: Option<HealthEventBroadcastConfig>,
}

#[derive(
    Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash, Default,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct HealthEventBroadcastConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_health_broadcast_channel")]
    pub channel: String,
    #[serde(default = "default_dedup_window_secs")]
    pub dedup_window_secs: u64,
}

fn default_health_broadcast_channel() -> String {
    "gw:health:events".to_string()
}

fn default_dedup_window_secs() -> u64 {
    10
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub telemetry: telemetry::Config,
    pub server: self::server::ServerConfig,
    pub s3: self::s3::Config,
    pub database: self::database::DatabaseConfig,
    pub dispatcher: self::dispatcher::DispatcherConfig,
    pub discover: self::discover::DiscoverConfig,
    pub response_headers: self::response_headers::ResponseHeadersConfig,
    pub deployment_target: self::deployment_target::DeploymentTarget,

    /// If a request is made with a model that is not in the `RouterConfig`
    /// model mapping, then we fallback to this.
    pub default_model_mapping: self::model_mapping::ModelMappingConfig,
    /// Alephant gateway feature flags (auth, observability, prompts).
    pub alephant: self::alephant::AlephantConfig,
    /// *ALL* supported providers, independent of router configuration.
    pub providers: self::providers::ProvidersConfig,

    /// Global middleware configuration, e.g. rate limiting, etc.
    ///
    /// This configuration will be for middleware that is applied to ALL
    /// routes on the application, and will run before any other
    /// router or unified-api-specific middleware.
    pub global: MiddlewareConfig,
    /// Middleware configuration for the unified API.
    ///
    /// This configuration will be for middleware that is applied to ALL
    /// requests to the unified API (`/ai`)
    pub unified_api: MiddlewareConfig,
    /// Unified fallback / failover policy (health, rate limits, retry).
    #[serde(default)]
    pub fallback_policy: self::fallback_policy::FallbackPolicyConfig,
    /// gRPC policy service (`policy.v1.PolicyService/Evaluate`).
    #[serde(default)]
    pub policy: self::policy::PolicyConfig,
    /// Local dev without PostgreSQL; see
    /// `docs/superpowers/specs/2026-04-03-compat-mode-design.md`.
    #[serde(default)]
    pub compat_mode: bool,
    /// Request/response log delivery (HTTP / dedicated Redis Stream).
    #[serde(default)]
    pub request_log: self::request_log::RequestLogConfig,
    /// LLM response KV cache (Cloudflare); required at startup with
    /// `--features external`.
    #[serde(default)]
    pub cloudflare_kv: Option<self::cloudflare_kv::CloudflareKvConfig>,
    /// LLM response KV cache (TiKV); `--features internal` only; defaults to
    /// stub.
    #[serde(default)]
    pub tikv_kv: Option<self::tikv_kv::TikvKvConfig>,
    /// LLM semantic cache (Qdrant + embeddings).
    #[serde(default)]
    pub semantic_cache: self::semantic_cache::SemanticCacheConfig,
    /// Legacy `openrouter-catalog-sync` config section: parsed then dropped;
    /// gateway ignores it.
    #[serde(
        default,
        rename = "openrouter-catalog-sync",
        skip_serializing_if = "Option::is_none"
    )]
    pub openrouter_catalog_sync_deprecated: Option<serde_json::Value>,
    /// Legacy `control-plane` YAML section: parsed then dropped; gateway
    /// ignores it.
    #[serde(
        default,
        rename = "control-plane",
        skip_serializing_if = "Option::is_none"
    )]
    pub control_plane_deprecated: Option<serde_json::Value>,
    pub routers: self::router::RouterConfigs,
}

/// [`config::Environment`] with [`config::Case::Kebab`] maps the `S3` segment
/// to the JSON key `s-3`. After [`merge`] with defaults / YAML `s3`, both keys
/// may exist; fold env-derived `s-3` into `s3` (deep merge) so serde never sees
/// two sources for the same field.
fn merge_s_dash_3_into_s3(merged: &mut serde_json::Value) {
    let Some(root) = merged.as_object_mut() else {
        return;
    };
    let Some(env_s3) = root.remove("s-3") else {
        return;
    };
    match root.get_mut("s3") {
        Some(s3) if s3.is_object() && env_s3.is_object() => {
            merge(s3, &env_s3);
        }
        _ => {
            root.insert("s3".to_string(), env_s3);
        }
    }
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_s3_url_style(raw: &str) -> Result<self::s3::UrlStyle, Box<Error>> {
    let s = raw.trim();
    match s {
        "path" => Ok(self::s3::UrlStyle::Path),
        "virtual-host" => Ok(self::s3::UrlStyle::VirtualHost),
        _ => Err(Box::new(Error::InvalidS3UrlStyle(s.to_string()))),
    }
}

fn apply_flat_s3_env_overrides(
    s3: &mut self::s3::Config,
) -> Result<(), Box<Error>> {
    if let Some(endpoint) = env_non_empty("S3_ENDPOINT") {
        s3.endpoint = Url::parse(&endpoint).map_err(Error::UrlParse)?;
    }
    if let Some(bucket) = env_non_empty("S3_BUCKET_NAME") {
        s3.bucket_name = bucket;
    }
    if let Some(region) = env_non_empty("S3_REGION") {
        s3.region = region;
    }
    if let Some(key) = env_non_empty("S3_ACCESS_KEY") {
        s3.access_key = Secret::from(key);
    }
    if let Some(key) = env_non_empty("S3_SECRET_KEY") {
        s3.secret_key = Secret::from(key);
    }
    if let Some(style) = env_non_empty("S3_URL_STYLE") {
        s3.url_style = parse_s3_url_style(&style)?;
    }
    Ok(())
}

impl Config {
    pub fn try_read(
        config_file_path: Option<&PathBuf>,
    ) -> Result<Self, Box<Error>> {
        let mut default_config = serde_json::to_value(Self::default())
            .expect("default config is serializable");
        let mut builder = config::Config::builder();
        if let Some(path) = config_file_path {
            builder = builder.add_source(config::File::from(path.clone()));
            builder = builder.add_source(
                config::Environment::with_prefix("AI_GATEWAY")
                    .try_parsing(true)
                    .separator("__")
                    .convert_case(config::Case::Kebab),
            );
        } else {
            // Files first, then env: matches the explicit `-c` branch so
            // `AI_GATEWAY__*` always overrides YAML (e.g. `cloudflare-kv` in
            // `/etc/ai-gateway/alephant-cloud.yaml`).
            if std::fs::exists(DEFAULT_CONFIG_PATH).unwrap_or_default() {
                builder = builder.add_source(config::File::from(
                    PathBuf::from(DEFAULT_CONFIG_PATH),
                ));
            }
            if std::fs::exists(DEFAULT_ALEPHANT_CLOUD_PATH).unwrap_or_default()
            {
                builder = builder.add_source(config::File::from(
                    PathBuf::from(DEFAULT_ALEPHANT_CLOUD_PATH),
                ));
            }
            builder = builder.add_source(
                config::Environment::with_prefix("AI_GATEWAY")
                    .try_parsing(true)
                    .separator("__")
                    .convert_case(config::Case::Kebab),
            );
        }
        let input_config: serde_json::Value = builder
            .build()
            .map_err(Error::from)
            .map_err(Box::new)?
            .try_deserialize()
            .map_err(Error::from)
            .map_err(Box::new)?;
        merge(&mut default_config, &input_config);
        merge_s_dash_3_into_s3(&mut default_config);

        let mut config: Config =
            serde_path_to_error::deserialize(default_config)
                .map_err(Error::from)
                .map_err(Box::new)?;

        // HACK: for secret fields in the **`Config`** struct that don't follow
        // the `AI_GATEWAY` prefix + the double underscore separator (`__`)
        // format.
        //
        // Re-read the control-plane API key from env after merge because
        // `serde_json::to_value(Self::default())` serialises Secret as "*****".
        // ALEPHANT_CONTROL_PLANE_API_KEY takes precedence.
        if let Ok(api_key) = std::env::var("ALEPHANT_CONTROL_PLANE_API_KEY")
            .or_else(|_| std::env::var(LEGACY_CONTROL_PLANE_API_KEY_ENV))
        {
            config.alephant.api_key = Secret::from(api_key);
        }

        if config.control_plane_deprecated.is_some() {
            tracing::warn!(
                "`control-plane` config block is deprecated and ignored; \
                 remove it from YAML"
            );
        }

        if let Ok(bedrock_region) = std::env::var("AWS_REGION")
            && let Some(bedrock_provider) =
                config.providers.get_mut(&InferenceProvider::Bedrock)
        {
            let bedrock_url = format!(
                "https://bedrock-runtime.{bedrock_region}.amazonaws.com"
            );
            bedrock_provider.base_url =
                Url::parse(&bedrock_url).map_err(Error::UrlParse)?;
        }

        if let Ok(key) = std::env::var("REDIS_STREAM_KEY_REQUEST_RESPONSE")
            && !key.trim().is_empty()
        {
            config.request_log.request_response_stream_key = key;
        }

        if let Some(db_url) = env_non_empty("POSTGRES_DATABASE_URL") {
            config.database.url = Secret::from(db_url);
        }

        if let Some(redis_url) = env_non_empty("REDIS_URL") {
            config.request_log.log_queue_redis_url =
                Some(Url::parse(&redis_url).map_err(Error::UrlParse)?);
        }

        apply_flat_s3_env_overrides(&mut config.s3)?;

        Ok(config)
    }

    pub fn validate(&self) -> Result<(), InitError> {
        let router_id_regex =
            Regex::new(ROUTER_ID_REGEX).expect("always valid if tests pass");
        for (router_id, router_config) in self.routers.as_ref() {
            router_config.validate()?;
            if !router_id_regex.is_match(router_id.as_ref()) {
                return Err(InitError::InvalidRouterId(router_id.to_string()));
            }
        }
        self.fallback_policy.validate()?;
        if self.policy.enabled && self.policy.grpc_endpoint.trim().is_empty() {
            return Err(InitError::PolicyGrpcConnect(
                "policy.grpc-endpoint must be set when policy.enabled is true"
                    .to_string(),
            ));
        }
        if self.compat_mode
            && self.alephant.features != self::alephant::AlephantFeatures::None
        {
            return Err(InitError::CompatModeAlephantFeatures(format!(
                "alephant.features must be `none` when compat_mode is true \
                 (got {:?})",
                self.alephant.features
            )));
        }
        // TODO: merged configs make this brittle. bring it back after we've
        // improved that self.validate_model_mappings()?;
        Ok(())
    }
}

#[cfg(feature = "testing")]
impl crate::tests::TestDefault for Config {
    fn test_default() -> Self {
        #[cfg(feature = "external")]
        let cloudflare_kv = Some(self::cloudflare_kv::CloudflareKvConfig {
            api_base: "https://api.cloudflare.com/client/v4".into(),
            account_id: "test".into(),
            namespace_id: "test".into(),
            api_token: crate::types::secret::Secret::from(
                "test-token".to_string(),
            ),
        });
        #[cfg(not(feature = "external"))]
        let cloudflare_kv = None;

        let telemetry = telemetry::Config {
            exporter: telemetry::Exporter::Stdout,
            level: "info,ai_gateway=trace".to_string(),
            ..Default::default()
        };
        Config {
            telemetry,
            server: self::server::ServerConfig::test_default(),
            s3: self::s3::Config::test_default(),
            database: self::database::DatabaseConfig::test_default(),
            dispatcher: self::dispatcher::DispatcherConfig::test_default(),
            default_model_mapping:
                self::model_mapping::ModelMappingConfig::default(),
            global: MiddlewareConfig::default(),
            unified_api: MiddlewareConfig::default(),
            providers: self::providers::ProvidersConfig::default(),
            alephant: self::alephant::AlephantConfig::test_default(),
            deployment_target: self::deployment_target::DeploymentTarget::new(
                std::time::Duration::from_secs(30),
                std::time::Duration::from_secs(300),
                self::deployment_target::MasterKeyResolution::PrimaryThenWorkspaceFallback,
            ),
            discover: self::discover::DiscoverConfig::test_default(),
            routers: self::router::RouterConfigs::test_default(),
            response_headers:
                self::response_headers::ResponseHeadersConfig::default(),
            fallback_policy:
                self::fallback_policy::FallbackPolicyConfig::test_default(),
            openrouter_catalog_sync_deprecated: None,
            control_plane_deprecated: None,
            policy: self::policy::PolicyConfig::default(),
            compat_mode: false,
            request_log: self::request_log::RequestLogConfig {
                transport: self::request_log::RequestLogTransport::Http,
                ..Default::default()
            },
            cloudflare_kv,
            tikv_kv: None,
            semantic_cache: self::semantic_cache::SemanticCacheConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Mutex, OnceLock},
        time::Duration,
    };

    use super::*;
    use crate::config::deployment_target::{
        DeploymentTarget, MasterKeyResolution,
    };

    #[test]
    fn router_id_regex_is_valid() {
        assert!(Regex::new(ROUTER_ID_REGEX).is_ok());
    }

    #[test]
    fn default_config_is_serializable() {
        // if it doesn't panic, it's good
        let _config = serde_json::to_string(&Config::default())
            .expect("default config is serializable");
    }

    #[test]
    fn merge_s_dash_3_into_s3_folds_next_to_existing_s3() {
        let mut v = serde_json::to_value(Config::default()).expect("default");
        let obj = v.as_object_mut().expect("object");
        obj.insert(
            "s-3".to_string(),
            serde_json::json!({ "endpoint": "http://env-only:9000" }),
        );
        super::merge_s_dash_3_into_s3(&mut v);
        let cfg: Config =
            serde_json::from_value(v).expect("folded s-3 into s3");
        assert_eq!(cfg.s3.endpoint.as_str(), "http://env-only:9000/");
    }

    #[test]
    fn merge_s_dash_3_into_s3_renames_when_s3_missing() {
        let mut v = serde_json::to_value(Config::default()).expect("default");
        let obj = v.as_object_mut().expect("object");
        let s3 = obj.remove("s3").expect("s3 key");
        obj.insert("s-3".to_string(), s3);
        super::merge_s_dash_3_into_s3(&mut v);
        let cfg: Config = serde_json::from_value(v).expect("s-3 becomes s3");
        assert_eq!(cfg.s3.bucket_name, Config::default().s3.bucket_name);
    }

    #[test]
    fn compat_mode_rejects_non_none_alephant_features() {
        use crate::config::alephant::AlephantFeatures;

        let mut config = Config {
            compat_mode: true,
            ..Default::default()
        };
        config.alephant.features = AlephantFeatures::All;
        let err = config.validate().expect_err("expected validation error");
        let msg = err.to_string();
        assert!(
            msg.contains("compat") || msg.contains("none"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn deployment_target_round_trip() {
        let cloud_config = DeploymentTarget::new(
            Duration::from_secs(60),
            Duration::from_secs(300),
            MasterKeyResolution::PrimaryThenWorkspaceFallback,
        );
        let serialized = serde_json::to_string(&cloud_config).unwrap();
        let deserialized =
            serde_json::from_str::<DeploymentTarget>(&serialized).unwrap();
        assert_eq!(cloud_config, deserialized);
    }

    #[cfg(feature = "testing")]
    #[test]
    fn test_default_uses_cloud_deployment_target() {
        use crate::tests::TestDefault as _;

        let cfg = Config::test_default();
        assert_eq!(
            cfg.deployment_target,
            DeploymentTarget::new(
                Duration::from_secs(30),
                Duration::from_secs(300),
                MasterKeyResolution::PrimaryThenWorkspaceFallback,
            )
        );
    }

    #[test]
    fn router_id_regex_positive_cases() {
        let regex = Regex::new(ROUTER_ID_REGEX).unwrap();
        let valid_ids = [
            "a",
            "Z",
            "abc",
            "ABC",
            "A1B2",
            "A-1",
            "a_b",
            "abc_def",
            "0123456789",
            "123456789012", // 12 chars
            "a-b-c-d",
        ];
        for id in valid_ids {
            assert!(
                regex.is_match(id),
                "expected '{id}' to be valid according to ROUTER_ID_REGEX"
            );
        }
    }

    #[test]
    fn router_id_regex_negative_cases() {
        let regex = Regex::new(ROUTER_ID_REGEX).unwrap();
        let invalid_ids = [
            "",
            "with space",
            "special$",
            "1234567890123", // 13 chars
            "mixed*chars",
        ];
        for id in invalid_ids {
            assert!(
                !regex.is_match(id),
                "expected '{id}' to be invalid according to ROUTER_ID_REGEX"
            );
        }
    }

    // Individual field round trip tests
    #[test]
    fn telemetry_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.telemetry).unwrap();
        let deserialized =
            serde_json::from_str::<telemetry::Config>(&serialized).unwrap();
        assert_eq!(config.telemetry, deserialized);
    }

    #[test]
    fn server_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.server).unwrap();
        let deserialized =
            serde_json::from_str::<self::server::ServerConfig>(&serialized)
                .unwrap();
        assert_eq!(config.server, deserialized);
    }

    #[test]
    fn dispatcher_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.dispatcher).unwrap();
        let deserialized = serde_json::from_str::<
            self::dispatcher::DispatcherConfig,
        >(&serialized)
        .unwrap();
        assert_eq!(config.dispatcher, deserialized);
    }

    #[test]
    fn discover_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.discover).unwrap();
        let deserialized =
            serde_json::from_str::<self::discover::DiscoverConfig>(&serialized)
                .unwrap();
        assert_eq!(config.discover, deserialized);
    }

    #[test]
    fn response_headers_round_trip() {
        let config = Config::default();
        let serialized =
            serde_json::to_string(&config.response_headers).unwrap();
        let deserialized = serde_json::from_str::<
            self::response_headers::ResponseHeadersConfig,
        >(&serialized)
        .unwrap();
        assert_eq!(config.response_headers, deserialized);
    }

    #[test]
    fn deployment_target_field_round_trip() {
        let config = Config::default();
        let serialized =
            serde_json::to_string(&config.deployment_target).unwrap();
        let deserialized = serde_json::from_str::<
            self::deployment_target::DeploymentTarget,
        >(&serialized)
        .unwrap();
        assert_eq!(config.deployment_target, deserialized);
    }

    #[test]
    fn default_model_mapping_round_trip() {
        let config = Config::default();
        let serialized =
            serde_json::to_string(&config.default_model_mapping).unwrap();
        let deserialized = serde_json::from_str::<
            self::model_mapping::ModelMappingConfig,
        >(&serialized)
        .unwrap();
        assert_eq!(config.default_model_mapping, deserialized);
    }

    #[test]
    fn providers_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.providers).unwrap();
        let deserialized = serde_json::from_str::<
            self::providers::ProvidersConfig,
        >(&serialized)
        .unwrap();
        assert_eq!(config.providers, deserialized);
    }

    #[test]
    fn global_middleware_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.global).unwrap();
        let deserialized =
            serde_json::from_str::<MiddlewareConfig>(&serialized).unwrap();
        assert_eq!(config.global, deserialized);
    }

    #[test]
    fn unified_api_middleware_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.unified_api).unwrap();
        let deserialized =
            serde_json::from_str::<MiddlewareConfig>(&serialized).unwrap();
        assert_eq!(config.unified_api, deserialized);
    }

    #[test]
    fn routers_round_trip() {
        let config = Config::default();
        let serialized = serde_json::to_string(&config.routers).unwrap();
        let deserialized =
            serde_json::from_str::<self::router::RouterConfigs>(&serialized)
                .unwrap();
        assert_eq!(config.routers, deserialized);
    }

    #[test]
    fn secret_serialization_behavior() {
        // This test demonstrates why configs with Secret fields fail round-trip
        // serialization
        use crate::types::secret::Secret;

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct TestConfig {
            secret_field: Secret<String>,
        }

        let original = TestConfig {
            secret_field: Secret::from("my-secret-value".to_string()),
        };

        // Serialize the config
        let serialized = serde_json::to_string(&original).unwrap();
        tracing::info!("Serialized: {serialized}");

        // The serialized form will be: {"secret_field":"*****"}
        assert!(serialized.contains("*****"));

        // Deserializing succeeds but with "*****" as the new value
        let deserialized =
            serde_json::from_str::<TestConfig>(&serialized).unwrap();

        // The values won't be equal because the secret is now "*****" instead
        // of "my-secret-value"
        assert_ne!(
            original, deserialized,
            "Round-trip fails because secret value is lost"
        );

        // To verify, let's check the exposed value
        assert_eq!(deserialized.secret_field.expose(), "*****");
        assert_ne!(
            original.secret_field.expose(),
            deserialized.secret_field.expose()
        );
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_env_overrides<T>(
        vars: &[(&str, Option<&str>)],
        f: impl FnOnce() -> T,
    ) -> T {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k).ok()))
            .collect();

        for (k, v) in vars {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }

        let out = f();

        for (k, v) in previous {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
        out
    }

    #[test]
    fn config_accepts_alephant_yaml_key() {
        let alephant_yaml = r"
alephant:
  features: auth
";
        let c1: Config = serde_yml::from_str(alephant_yaml).unwrap();
        assert_eq!(
            c1.alephant.features,
            self::alephant::AlephantFeatures::Auth
        );
    }

    #[test]
    fn config_accepts_deprecated_control_plane_block() {
        let yaml = r"
control-plane:
  retry:
    strategy: constant
    delay: 2s
    max-retries: 3
";

        let config: Config = serde_yml::from_str(yaml).unwrap();
        assert!(config.control_plane_deprecated.is_some());
    }

    #[test]
    fn config_rejects_removed_rate_limit_blocks() {
        let yaml = r"
rate-limit-store:
  type: in-memory
global:
  rate-limit:
    per-api-key:
      refill-frequency: 1s
      capacity: 5
unified-api:
  rate-limit:
    per-api-key:
      refill-frequency: 2s
      capacity: 7
";

        let err = serde_yml::from_str::<Config>(yaml).unwrap_err().to_string();
        assert!(err.contains("rate-limit"));
    }

    #[test]
    fn alephant_control_plane_env_takes_precedence_over_legacy() {
        let legacy_env = concat!("HELI", "CONE_CONTROL_PLANE_API_KEY");
        let config = with_env_overrides(
            &[
                ("ALEPHANT_CONTROL_PLANE_API_KEY", Some("sk-alephant-env")),
                (legacy_env, Some("sk-legacy-env")),
            ],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(config.alephant.api_key.expose(), "sk-alephant-env");
    }

    #[test]
    fn client_ip_rate_limit_env_overrides_yaml_requests_per_second() {
        let path = std::env::temp_dir().join(format!(
            "ai-gateway-iprl-env-test-{}.yaml",
            std::process::id()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        std::fs::write(
            &path,
            r"global:
  client-ip-rate-limit:
    enabled: true
    requests-per-second: 5
    backend: memory
",
        )
        .expect("write temp yaml");
        let _cleanup = Cleanup(path.clone());

        let cfg = with_env_overrides(
            &[(
                "AI_GATEWAY__GLOBAL__CLIENT_IP_RATE_LIMIT__REQUESTS_PER_SECOND",
                Some("42"),
            )],
            || Config::try_read(Some(&path)).expect("try_read"),
        );
        let c = cfg
            .global
            .client_ip_rate_limit
            .as_ref()
            .expect("client-ip-rate-limit");
        assert_eq!(c.requests_per_second, 42);
        assert!(c.enabled);
    }

    #[test]
    fn semantic_cache_env_overrides_yaml_threshold() {
        let path = std::env::temp_dir().join(format!(
            "ai-gateway-semantic-cache-env-test-{}.yaml",
            std::process::id()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        std::fs::write(
            &path,
            r"semantic-cache:
  default-threshold-millis: 800
  default-ttl-seconds: 3600
  qdrant:
    url: http://127.0.0.1:6333
",
        )
        .expect("write temp yaml");
        let _cleanup = Cleanup(path.clone());

        let cfg = with_env_overrides(
            &[(
                "AI_GATEWAY__SEMANTIC_CACHE__DEFAULT_THRESHOLD_MILLIS",
                Some("900"),
            )],
            || Config::try_read(Some(&path)).expect("try_read"),
        );
        assert_eq!(cfg.semantic_cache.default_threshold_millis, 900);
        assert_eq!(cfg.semantic_cache.qdrant.collection, None);
    }

    #[test]
    fn postgres_database_url_overrides_legacy_database_env() {
        let cfg = with_env_overrides(
            &[
                (
                    "POSTGRES_DATABASE_URL",
                    Some("postgres://new-user:new-pass@127.0.0.1:15432/new_db"),
                ),
                (
                    "AI_GATEWAY__DATABASE__URL",
                    Some(
                        "postgres://legacy-user:legacy-pass@127.0.0.1:25432/\
                         legacy_db",
                    ),
                ),
            ],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(
            cfg.database.url.expose(),
            "postgres://new-user:new-pass@127.0.0.1:15432/new_db"
        );
    }

    #[test]
    fn blank_postgres_database_url_falls_back_to_legacy_database_env() {
        let cfg = with_env_overrides(
            &[
                ("POSTGRES_DATABASE_URL", Some("   ")),
                (
                    "AI_GATEWAY__DATABASE__URL",
                    Some(
                        "postgres://legacy-user:legacy-pass@127.0.0.1:25432/\
                         legacy_db",
                    ),
                ),
            ],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(
            cfg.database.url.expose(),
            "postgres://legacy-user:legacy-pass@127.0.0.1:25432/legacy_db"
        );
    }

    #[test]
    fn redis_url_overrides_legacy_request_log_redis_env() {
        let cfg = with_env_overrides(
            &[
                ("REDIS_URL", Some("redis://127.0.0.1:6380/9")),
                (
                    "AI_GATEWAY__REQUEST_LOG__LOG_QUEUE_REDIS_URL",
                    Some("redis://127.0.0.1:6381/10"),
                ),
            ],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(
            cfg.request_log
                .log_queue_redis_url
                .as_ref()
                .map(Url::as_str),
            Some("redis://127.0.0.1:6380/9")
        );
    }

    #[test]
    fn invalid_redis_url_returns_parse_error() {
        let err =
            with_env_overrides(&[("REDIS_URL", Some("not-a-url"))], || {
                Config::try_read(None).unwrap_err().to_string()
            });
        assert!(err.contains("URL parsing error"), "unexpected err: {err}");
    }

    #[test]
    fn flat_s3_env_overrides_ai_gateway_s3_env() {
        let cfg = with_env_overrides(
            &[
                ("AI_GATEWAY__S3__ENDPOINT", Some("http://127.0.0.1:9100")),
                ("AI_GATEWAY__S3__BUCKET_NAME", Some("from-nested")),
                ("S3_ENDPOINT", Some("http://127.0.0.1:9200")),
                ("S3_BUCKET_NAME", Some("from-flat")),
            ],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(cfg.s3.endpoint.as_str(), "http://127.0.0.1:9200/");
        assert_eq!(cfg.s3.bucket_name, "from-flat");
    }

    #[test]
    fn blank_s3_endpoint_falls_back_to_merged_config() {
        let cfg = with_env_overrides(
            &[
                ("S3_ENDPOINT", Some("   ")),
                ("AI_GATEWAY__S3__ENDPOINT", Some("http://127.0.0.1:9300")),
            ],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(cfg.s3.endpoint.as_str(), "http://127.0.0.1:9300/");
    }

    #[test]
    fn flat_s3_endpoint_applies_without_ai_gateway_s3() {
        let cfg = with_env_overrides(
            &[("S3_ENDPOINT", Some("http://127.0.0.1:9400"))],
            || Config::try_read(None).expect("config load"),
        );
        assert_eq!(cfg.s3.endpoint.as_str(), "http://127.0.0.1:9400/");
    }

    #[test]
    fn invalid_s3_endpoint_returns_url_parse_error() {
        let err =
            with_env_overrides(&[("S3_ENDPOINT", Some("not-a-url"))], || {
                Config::try_read(None).unwrap_err().to_string()
            });
        assert!(err.contains("URL parsing error"), "unexpected err: {err}");
    }

    #[test]
    fn invalid_s3_url_style_returns_error() {
        let err =
            with_env_overrides(&[("S3_URL_STYLE", Some("nosuch"))], || {
                Config::try_read(None).unwrap_err().to_string()
            });
        assert!(
            err.contains("invalid S3_URL_STYLE"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn semantic_cache_accepts_deprecated_qdrant_collection_field() {
        let path = std::env::temp_dir().join(format!(
            "ai-gateway-semantic-cache-deprecated-collection-test-{}.yaml",
            std::process::id()
        ));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        std::fs::write(
            &path,
            r"semantic-cache:
  default-threshold-millis: 900
  default-ttl-seconds: 3600
  qdrant:
    url: http://127.0.0.1:6333
    collection: from-yaml
",
        )
        .expect("write temp yaml");
        let _cleanup = Cleanup(path.clone());

        let cfg = Config::try_read(Some(&path)).expect("try_read");
        assert_eq!(
            cfg.semantic_cache.qdrant.collection,
            Some("from-yaml".to_string())
        );
    }
}
