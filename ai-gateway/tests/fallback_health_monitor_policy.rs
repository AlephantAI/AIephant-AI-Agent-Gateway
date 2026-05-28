//! Pure-function tests for `fallback_bridge::resolved_health_monitor_config`.
//!
//! Verifies that the health-monitor parameters are drawn from
//! `fallback-policy.provider-failover.health` when enabled, and fall back to
//! `discover.monitor` (compat) otherwise.

use std::time::Duration;

use ai_gateway::config::{
    Config,
    discover::DiscoverConfig,
    fallback_bridge::{HealthMonitorParams, resolved_health_monitor_config},
    fallback_policy::{FallbackPolicyConfig, ProviderFailoverHealthPolicy},
    monitor::{GracePeriod, HealthMonitorConfig, MonitorConfig},
};
use rust_decimal::{Decimal, prelude::FromPrimitive};

// ─── helpers ────────────────────────────────────────────────────────────────

fn config_with_policy_health(
    policy_enabled: bool,
    health_enabled: bool,
    threshold: Decimal,
    interval: Duration,
    min_requests: u32,
) -> Config {
    let mut config = Config {
        fallback_policy: FallbackPolicyConfig {
            enabled: policy_enabled,
            ..FallbackPolicyConfig::default()
        },
        ..Config::default()
    };
    config.fallback_policy.provider_failover.health =
        ProviderFailoverHealthPolicy {
            enabled: health_enabled,
            error_ratio_threshold: threshold,
            interval,
            grace_period: GracePeriod::Requests { min_requests },
        };
    config
}

fn config_with_discover_monitor(
    threshold: f64,
    interval: Duration,
    min_requests: u32,
) -> Config {
    use rust_decimal::prelude::FromPrimitive;
    let mut config = Config::default();
    // Disable policy so discover.monitor is used.
    config.fallback_policy.enabled = false;
    config.discover = DiscoverConfig {
        monitor: MonitorConfig {
            health: HealthMonitorConfig::ErrorRatio {
                ratio: Decimal::from_f64(threshold).unwrap(),
                window: Duration::from_secs(60),
                buckets: 10,
                interval,
                grace_period: GracePeriod::Requests { min_requests },
            },
        },
        ..DiscoverConfig::default()
    };
    config
}

// ─── priority tests ──────────────────────────────────────────────────────────

/// When policy is enabled and health sub-block is enabled, use
/// `fallback-policy.provider-failover.health.*`.
#[test]
fn policy_health_wins_when_both_enabled() {
    let threshold = Decimal::new(15, 2); // 0.15
    let interval = Duration::from_secs(7);
    let config = config_with_policy_health(true, true, threshold, interval, 30);

    let params = resolved_health_monitor_config(&config);
    assert!(
        (params.error_threshold - 0.15_f64).abs() < 1e-9,
        "error_threshold should come from fallback-policy: {:.4}",
        params.error_threshold
    );
    assert_eq!(params.interval, Duration::from_secs(7));
    assert_eq!(
        params.grace_period,
        GracePeriod::Requests { min_requests: 30 }
    );
}

/// When policy is disabled, fall back to discover.monitor.
#[test]
fn discover_monitor_used_when_policy_disabled() {
    let config = config_with_discover_monitor(0.20, Duration::from_secs(3), 15);
    let params = resolved_health_monitor_config(&config);
    assert!(
        (params.error_threshold - 0.20_f64).abs() < 1e-9,
        "expected 0.20, got {}",
        params.error_threshold
    );
    assert_eq!(params.interval, Duration::from_secs(3));
    assert_eq!(
        params.grace_period,
        GracePeriod::Requests { min_requests: 15 }
    );
}

/// When policy is enabled but health sub-block is disabled, fall back to
/// discover.monitor.
#[test]
fn discover_monitor_used_when_health_subblock_disabled() {
    let mut config = config_with_policy_health(
        true,
        false,
        Decimal::new(5, 2),
        Duration::from_secs(1),
        5,
    );
    config.discover.monitor = MonitorConfig {
        health: HealthMonitorConfig::ErrorRatio {
            ratio: Decimal::from_f64(0.25).unwrap(),
            window: Duration::from_secs(60),
            buckets: 10,
            interval: Duration::from_secs(9),
            grace_period: GracePeriod::Requests { min_requests: 50 },
        },
    };
    let params = resolved_health_monitor_config(&config);
    assert!(
        (params.error_threshold - 0.25_f64).abs() < 1e-9,
        "should fall back to discover.monitor, got {}",
        params.error_threshold
    );
    assert_eq!(params.interval, Duration::from_secs(9));
    assert_eq!(
        params.grace_period,
        GracePeriod::Requests { min_requests: 50 }
    );
}

// ─── compat golden ───────────────────────────────────────────────────────────

/// Default fallback-policy health params must match the legacy discover.monitor
/// defaults, so compat mode is truly transparent.
#[test]
fn compat_golden_defaults_match_legacy_monitor_defaults() {
    // Config::default() has fallback_policy.enabled = true, so policy wins.
    // Verify policy defaults equal monitor defaults.
    let policy_config = Config::default();
    let policy_params = resolved_health_monitor_config(&policy_config);

    let mut legacy_config = Config::default();
    legacy_config.fallback_policy.enabled = false;
    let legacy_params = resolved_health_monitor_config(&legacy_config);

    assert!(
        (policy_params.error_threshold - legacy_params.error_threshold).abs()
            < 1e-9,
        "compat: policy default threshold {:.4} != legacy {:.4}",
        policy_params.error_threshold,
        legacy_params.error_threshold
    );
    assert_eq!(
        policy_params.interval, legacy_params.interval,
        "compat: policy default interval != legacy interval"
    );
    assert_eq!(
        policy_params.grace_period, legacy_params.grace_period,
        "compat: policy default grace_period != legacy grace_period"
    );
}

/// `HealthMonitorParams` equality works correctly.
#[test]
fn health_monitor_params_equality() {
    let a = HealthMonitorParams {
        error_threshold: 0.1,
        interval: Duration::from_secs(5),
        grace_period: GracePeriod::Requests { min_requests: 20 },
    };
    let b = a.clone();
    assert_eq!(a, b);
}
