use displaydoc::Display;
use telemetry::TelemetryError;
use thiserror::Error;

use crate::{
    config::validation::ModelMappingValidationError,
    types::{provider::InferenceProvider, router::RouterId},
};

/// Errors that can occur during initialization.
#[derive(Debug, Error, Display)]
pub enum InitError {
    /// Default router not found
    DefaultRouterNotFound,
    /// Failed to read TLS certificate: {0}
    Tls(std::io::Error),
    /// Failed to bind to address: {0}
    Bind(std::io::Error),
    /// Telemetry: {0}
    Telemetry(#[from] TelemetryError),
    /// Invalid bucket config: {0}
    InvalidBucketConfig(#[from] rusty_s3::BucketError),
    /// OAuth config: {0}
    OAuthConfig(url::ParseError),
    /// Failed to create reqwest client: {0}
    CreateReqwestClient(reqwest::Error),
    /// Failed to create balancer: {0}
    CreateBalancer(tower::BoxError),
    /// Provider error: {0}
    ProviderError(#[from] crate::error::provider::ProviderError),
    /// Invalid weight for provider: {0}
    InvalidWeight(InferenceProvider),
    /// Invalid balancer: {0}
    InvalidBalancer(String),
    /// Converter registry endpoints not configured for provider: {0}
    EndpointsNotConfigured(InferenceProvider),
    /// Failed to create redis client: {0}
    CreateRedisClient(#[from] redis::RedisError),
    /// Failed to build otel metrics layer: {0}
    InitOtelMetricsLayer(#[from] tower_otel_http_metrics::Error),
    /// Failed to initialize system metrics
    InitSystemMetrics,
    /// Invalid rate limit config: {0}
    InvalidRateLimitConfig(&'static str),
    /// Invalid global client IP rate limit config: {0}
    InvalidClientIpRateLimitConfig(String),
    /// Invalid global gateway in-flight limit config: {0}
    InvalidGatewayInFlightLimitConfig(String),
    /// Invalid mappings config: {0}
    InvalidMappingsConfig(#[from] ModelMappingValidationError),
    /// Rate limit channels not initialized for router: {0}
    RateLimitChannelsNotInitialized(RouterId),
    /// Invalid router id: {0}
    InvalidRouterId(String),
    /// Cache not configured
    CacheNotConfigured,
    /// S3 client not configured
    S3NotConfigured,
    /// Database connection error: {0}
    DatabaseConnection(sqlx::Error),
    /// Model ID not recognized: {0}
    ModelIdNotRecognized(String),
    /// Provider not yet supported: {0}
    ProviderNotSupported(InferenceProvider),
    /// Store not configured: {0}
    StoreNotConfigured(&'static str),
    /// Router api keys not initialized
    RouterApiKeysNotInitialized,
    /// Invalid organization id: {0}
    InvalidOrganizationId(String),
    /// Router tx not set
    RouterTxNotSet,
    /// Failed to load initial alephant api keys from db: {0}
    InitAlephantKeys(String),
    /// Failed to load initial routers from db: {0}
    InitRouters(String),
    /// `MASTER_KEY_ENCRYPTION_KEY` is missing or invalid: {0}
    InvalidMasterKeyEncryptionKey(String),
    /// Invalid fallback policy config: {0}
    InvalidFallbackPolicy(&'static str),
    /// Policy content-filter gRPC: {0}
    PolicyGrpcConnect(String),
    /// `compat_mode` requires alephant.features to be `none`: {0}
    CompatModeAlephantFeatures(String),
    /// Security plugin configuration error: {0}
    SecurityPluginConfig(String),
}
