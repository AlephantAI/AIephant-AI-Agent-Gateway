//! Compat bridge: resolves the effective global `RetryConfig` from a
//! `Config` that may contain both the new unified `fallback-policy.retry`
//! and the legacy per-path retry fields.
//!
//! ## Priority (highest → lowest)
//!
//! 1. `fallback-policy.retry` — when `fallback-policy.enabled = true`
//! 2. `global.retries` — legacy fallback
//! 3. `None` — no retry
//!
//! ## Deprecated per-path retries
//!
//! `unified-api.retries` and per-router `retries` fields are superseded by
//! the single global `fallback-policy.retry`. When they are set,
//! `deprecated_per_path_retries` reports them so callers can warn operators
//! exactly once at start-up.

use std::{borrow::Cow, time::Duration};

use rust_decimal::prelude::ToPrimitive;

use crate::{
    config::{
        Config, monitor::GracePeriod, retry::RetryConfig, router::RouterConfigs,
    },
    types::router::RouterId,
};

// ─── Health-monitor params
// ────────────────────────────────────────────────────

/// Resolved health-monitor parameters, drawn from either
/// `fallback-policy.provider-failover.health` (preferred) or the legacy
/// `discover.monitor` config (compat fallback).
#[derive(Debug, Clone, PartialEq)]
pub struct HealthMonitorParams {
    /// Fraction of 5xx responses above which a provider is considered
    /// unhealthy.
    pub error_threshold: f64,
    /// How often the health check loop runs.
    pub interval: Duration,
    /// Minimum request sample before making a health determination.
    pub grace_period: GracePeriod,
}

/// Return the effective health-monitor parameters.
///
/// Priority:
/// 1. `fallback-policy.provider-failover.health` — when both the policy and the
///    health sub-block are enabled.
/// 2. `discover.monitor` — legacy compat fallback.
#[must_use]
pub fn resolved_health_monitor_config(config: &Config) -> HealthMonitorParams {
    let fp = &config.fallback_policy;
    if fp.enabled && fp.provider_failover.health.enabled {
        let h = &fp.provider_failover.health;
        return HealthMonitorParams {
            error_threshold: h.error_ratio_threshold.to_f64().unwrap_or(0.1),
            interval: h.interval,
            grace_period: h.grace_period.clone(),
        };
    }
    HealthMonitorParams {
        error_threshold: config.discover.monitor.error_threshold(),
        interval: config.discover.monitor.health_interval(),
        grace_period: config.discover.monitor.grace_period().clone(),
    }
}

// ─── Rate-limit params
// ─────────────────────────────────────────────────────

/// Return the duration to wait before re-inserting a rate-limited provider.
///
/// Delegates to [`crate::fallback::evaluator::restore_after`] so that both
/// compat and unified modes share the same calculation.  The policy defaults
/// (`default_retry_after_seconds = 30`, `restore_buffer = 30s`) mirror the
/// constants previously hard-coded in `rate_limit/provider.rs`, preserving
/// compat behaviour when the policy is enabled at its defaults.
#[must_use]
pub fn resolved_rate_limit_restore_after(
    config: &Config,
    retry_after_secs: Option<u64>,
) -> Duration {
    crate::fallback::evaluator::restore_after(
        &config.fallback_policy,
        retry_after_secs,
    )
}

/// Identifies a per-path retry config that has been superseded by
/// `fallback-policy.retry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeprecatedRetryLocation {
    /// `unified-api.retries` is set.
    UnifiedApi,
    /// `routers.<id>.retries` is set.
    Router(RouterId),
}

/// Return the effective global retry config, respecting the priority order
/// described in the module-level documentation.
#[must_use]
pub fn resolved_global_retry(config: &Config) -> Option<Cow<'_, RetryConfig>> {
    if config.fallback_policy.enabled {
        return Some(Cow::Borrowed(&config.fallback_policy.retry));
    }
    config.global.retries.as_ref().map(Cow::Borrowed)
}

/// Return all per-path retry configurations that are superseded by the
/// unified policy. Returns an empty `Vec` when none are set.
#[must_use]
pub fn deprecated_per_path_retries(
    config: &Config,
) -> Vec<DeprecatedRetryLocation> {
    let mut out = Vec::new();
    if config.unified_api.retries.is_some() {
        out.push(DeprecatedRetryLocation::UnifiedApi);
    }
    for (router_id, router_config) in iter_routers(&config.routers) {
        if router_config.retries.is_some() {
            out.push(DeprecatedRetryLocation::Router(router_id.clone()));
        }
    }
    out
}

/// Emit `tracing::warn` for every deprecated per-path retry found.
/// Call this once during application start-up after config is loaded.
pub fn warn_deprecated_per_path_retries(config: &Config) {
    for loc in deprecated_per_path_retries(config) {
        match loc {
            DeprecatedRetryLocation::UnifiedApi => {
                tracing::warn!(
                    "unified-api.retries is deprecated and will be ignored; \
                     configure retries under fallback-policy.retry instead"
                );
            }
            DeprecatedRetryLocation::Router(ref router_id) => {
                tracing::warn!(
                    router_id = %router_id,
                    "routers.{}.retries is deprecated and will be ignored; \
                     configure retries under fallback-policy.retry instead",
                    router_id
                );
            }
        }
    }
}

fn iter_routers(
    routers: &RouterConfigs,
) -> impl Iterator<Item = (&RouterId, &crate::config::router::RouterConfig)> {
    routers.iter()
}
