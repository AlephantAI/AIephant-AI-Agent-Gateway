//! Top-level `fallback-policy` configuration (unified failover / retry policy).

use std::time::Duration;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::{monitor::GracePeriod, retry::RetryConfig};
use crate::error::init::InitError;

/// High-level fallback behaviour mode (`compat` matches legacy layering).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum FallbackMode {
    #[default]
    Compat,
    Unified,
}

/// Health criteria for automatic provider failover.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProviderFailoverHealthPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_error_ratio_threshold")]
    pub error_ratio_threshold: Decimal,
    #[serde(default = "default_health_interval", with = "humantime_serde")]
    pub interval: Duration,
    #[serde(default = "default_grace_period")]
    pub grace_period: GracePeriod,
}

/// Rate-limit recovery behaviour for failover (align with
/// `discover/monitor/rate_limit/provider`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProviderFailoverRateLimitPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(
        default = "default_retry_after_secs",
        rename = "default-retry-after-seconds"
    )]
    pub default_retry_after_seconds: u64,
    #[serde(default = "default_restore_buffer", with = "humantime_serde")]
    pub restore_buffer: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ProviderFailoverPolicyBlock {
    #[serde(default)]
    pub health: ProviderFailoverHealthPolicy,
    #[serde(default)]
    pub rate_limit: ProviderFailoverRateLimitPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FallbackObservabilityPolicy {
    #[serde(
        default = "default_emit_decision_log",
        rename = "emit-decision-log"
    )]
    pub emit_decision_log: bool,
    #[serde(default = "default_metrics_prefix", rename = "metrics-prefix")]
    pub metrics_prefix: String,
}

/// Root `fallback-policy` block from gateway YAML.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
pub struct FallbackPolicyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: FallbackMode,
    #[serde(default)]
    pub retry: RetryConfig,
    #[serde(default, rename = "provider-failover")]
    pub provider_failover: ProviderFailoverPolicyBlock,
    #[serde(default)]
    pub observability: FallbackObservabilityPolicy,
}

impl Default for ProviderFailoverHealthPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            error_ratio_threshold: default_error_ratio_threshold(),
            interval: default_health_interval(),
            grace_period: default_grace_period(),
        }
    }
}

impl Default for ProviderFailoverRateLimitPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            default_retry_after_seconds: default_retry_after_secs(),
            restore_buffer: default_restore_buffer(),
        }
    }
}

impl Default for FallbackObservabilityPolicy {
    fn default() -> Self {
        Self {
            emit_decision_log: default_emit_decision_log(),
            metrics_prefix: default_metrics_prefix(),
        }
    }
}

impl Default for FallbackPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: FallbackMode::default(),
            retry: RetryConfig::default(),
            provider_failover: ProviderFailoverPolicyBlock::default(),
            observability: FallbackObservabilityPolicy::default(),
        }
    }
}

impl FallbackPolicyConfig {
    pub fn validate(&self) -> Result<(), InitError> {
        if self.provider_failover.health.enabled {
            if self.provider_failover.health.interval.is_zero() {
                return Err(InitError::InvalidFallbackPolicy(
                    "provider-failover.health.interval must be positive when \
                     health failover is enabled",
                ));
            }
            let t = self
                .provider_failover
                .health
                .error_ratio_threshold
                .clamp(Decimal::ZERO, Decimal::ONE);
            if t != self.provider_failover.health.error_ratio_threshold {
                return Err(InitError::InvalidFallbackPolicy(
                    "provider-failover.health.error-ratio-threshold must be \
                     between 0 and 1 inclusive",
                ));
            }
            match &self.provider_failover.health.grace_period {
                GracePeriod::Requests { min_requests } => {
                    if *min_requests == 0 {
                        return Err(InitError::InvalidFallbackPolicy(
                            "provider-failover.health.grace-period.\
                             min-requests must be > 0",
                        ));
                    }
                }
            }
        }

        if self.provider_failover.rate_limit.enabled {
            if self
                .provider_failover
                .rate_limit
                .default_retry_after_seconds
                == 0
            {
                return Err(InitError::InvalidFallbackPolicy(
                    "provider-failover.rate-limit.default-retry-after-seconds \
                     must be > 0 when rate-limit failover is enabled",
                ));
            }
            if self.provider_failover.rate_limit.restore_buffer.is_zero() {
                return Err(InitError::InvalidFallbackPolicy(
                    "provider-failover.rate-limit.restore-buffer must be \
                     positive when rate-limit failover is enabled",
                ));
            }
        }

        match &self.retry {
            RetryConfig::Exponential {
                min_delay,
                max_delay,
                ..
            } => {
                if min_delay > max_delay {
                    return Err(InitError::InvalidFallbackPolicy(
                        "fallback-policy.retry.min-delay must be <= max-delay",
                    ));
                }
            }
            RetryConfig::Constant { .. } => {}
        }

        if self.observability.metrics_prefix.trim().is_empty() {
            return Err(InitError::InvalidFallbackPolicy(
                "observability.metrics-prefix must be non-empty",
            ));
        }

        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_error_ratio_threshold() -> Decimal {
    Decimal::new(1, 1)
}

fn default_health_interval() -> Duration {
    Duration::from_secs(5)
}

fn default_grace_period() -> GracePeriod {
    GracePeriod::Requests { min_requests: 20 }
}

fn default_retry_after_secs() -> u64 {
    30
}

fn default_restore_buffer() -> Duration {
    Duration::from_secs(30)
}

fn default_emit_decision_log() -> bool {
    true
}

fn default_metrics_prefix() -> String {
    "ai_gateway.fallback".to_string()
}

#[cfg(feature = "testing")]
impl crate::tests::TestDefault for FallbackPolicyConfig {
    /// Test default aligns health-monitor timing with
    /// `MonitorConfig::test_default()` (1ms interval, 10 min-requests) so
    /// that integration tests that rely on fast health-check loops are not
    /// broken by the policy taking precedence.
    fn test_default() -> Self {
        use crate::config::retry::RetryConfig;
        Self {
            enabled: true,
            mode: FallbackMode::default(),
            // Zero retries by default so tests that don't explicitly want
            // retry behaviour are not affected by the global retry config.
            // Tests that need retries should set fallback_policy.retry directly.
            retry: RetryConfig::Constant {
                delay: std::time::Duration::from_millis(5),
                max_retries: 0,
            },
            provider_failover: crate::config::fallback_policy::ProviderFailoverPolicyBlock {
                health: ProviderFailoverHealthPolicy {
                    enabled: true,
                    error_ratio_threshold: Decimal::new(2, 2), // 0.02 matches MonitorConfig::test_default
                    interval: Duration::from_millis(1),
                    grace_period: GracePeriod::Requests { min_requests: 10 },
                },
                rate_limit: ProviderFailoverRateLimitPolicy {
                    enabled: true,
                    default_retry_after_seconds: 30,
                    restore_buffer: Duration::from_secs(1),
                },
            },
            observability: crate::config::fallback_policy::FallbackObservabilityPolicy::default(),
        }
    }
}
