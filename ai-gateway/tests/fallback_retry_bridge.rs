//! Table-driven tests for `config::fallback_bridge::resolved_global_retry`
//! and `deprecated_per_path_retries`.
//!
//! Priority rule: fallback-policy.retry (enabled) > global.retries > None.

use std::{collections::HashMap, time::Duration};

use ai_gateway::{
    config::{
        Config,
        fallback_bridge::{
            DeprecatedRetryLocation, deprecated_per_path_retries, resolved_global_retry,
            warn_deprecated_per_path_retries,
        },
        retry::RetryConfig,
        router::{RouterConfig, RouterConfigs},
    },
    types::router::RouterId,
};

// ─── helpers ────────────────────────────────────────────────────────────────

fn const_retry(secs: u64) -> RetryConfig {
    RetryConfig::Constant {
        delay: Duration::from_secs(secs),
        max_retries: 1,
    }
}

fn exp_retry_default() -> RetryConfig {
    RetryConfig::default()
}

// ─── resolved_global_retry priority ─────────────────────────────────────────

/// When policy is enabled (default), fallback-policy.retry wins even if
/// global.retries is also set.
#[test]
fn fallback_policy_enabled_wins_over_global_retries() {
    let mut config = Config::default();
    // Policy is enabled by default; set a custom retry there.
    config.fallback_policy.retry = const_retry(5);
    config.global.retries = Some(const_retry(99));

    let result = resolved_global_retry(&config).unwrap();
    assert_eq!(*result, const_retry(5), "fallback-policy.retry should win");
}

/// When policy is disabled, fall through to global.retries.
#[test]
fn policy_disabled_uses_global_retries() {
    let mut config = Config::default();
    config.fallback_policy.enabled = false;
    config.global.retries = Some(const_retry(10));

    let result = resolved_global_retry(&config).unwrap();
    assert_eq!(
        *result,
        const_retry(10),
        "should use global.retries as fallback"
    );
}

/// When policy is disabled and no global.retries, return None.
#[test]
fn policy_disabled_no_global_returns_none() {
    let mut config = Config::default();
    config.fallback_policy.enabled = false;
    config.global.retries = None;

    assert!(
        resolved_global_retry(&config).is_none(),
        "should return None when both are absent"
    );
}

/// Enabled policy with default retry returns the policy default (not None).
#[test]
fn policy_enabled_default_retry_returns_something() {
    let config = Config::default(); // enabled = true, retry = Default
    let result = resolved_global_retry(&config);
    assert!(
        result.is_some(),
        "enabled policy should always yield a retry"
    );
    assert_eq!(*result.unwrap(), exp_retry_default());
}

/// When policy is enabled and global.retries is None, still returns policy.
#[test]
fn policy_enabled_no_global_returns_policy_retry() {
    let mut config = Config::default();
    config.fallback_policy.retry = const_retry(7);
    config.global.retries = None;

    let result = resolved_global_retry(&config).unwrap();
    assert_eq!(*result, const_retry(7));
}

// ─── deprecated_per_path_retries ─────────────────────────────────────────────

/// Clean config — no deprecated locations.
#[test]
fn no_deprecated_retries_when_config_is_clean() {
    let config = Config::default();
    assert!(
        deprecated_per_path_retries(&config).is_empty(),
        "default config has no deprecated per-path retries"
    );
}

/// unified-api.retries is deprecated.
#[test]
fn unified_api_retries_reported_as_deprecated() {
    let mut config = Config::default();
    config.unified_api.retries = Some(const_retry(3));

    let locs = deprecated_per_path_retries(&config);
    assert!(
        locs.contains(&DeprecatedRetryLocation::UnifiedApi),
        "unified-api.retries should be reported as deprecated"
    );
}

/// routers.<id>.retries is deprecated.
#[test]
fn router_retries_reported_as_deprecated() {
    let mut config = Config::default();
    let router_id = RouterId::Named("test-router".into());
    let router_cfg = RouterConfig {
        retries: Some(const_retry(2)),
        ..RouterConfig::default()
    };

    config.routers = RouterConfigs::new(HashMap::from([(router_id.clone(), router_cfg)]));

    let locs = deprecated_per_path_retries(&config);
    assert!(
        locs.contains(&DeprecatedRetryLocation::Router(router_id.clone())),
        "router retries should be reported as deprecated"
    );
}

/// Both unified-api and router — both reported.
#[test]
fn both_deprecated_locations_reported() {
    let mut config = Config::default();
    config.unified_api.retries = Some(const_retry(1));

    let router_id = RouterId::Named("r1".into());
    let router_cfg = RouterConfig {
        retries: Some(const_retry(1)),
        ..RouterConfig::default()
    };
    config.routers = RouterConfigs::new(HashMap::from([(router_id.clone(), router_cfg)]));

    let locs = deprecated_per_path_retries(&config);
    assert_eq!(locs.len(), 2);
    assert!(locs.contains(&DeprecatedRetryLocation::UnifiedApi));
    assert!(locs.contains(&DeprecatedRetryLocation::Router(router_id)));
}

/// Deprecated configs are ignored: `resolved_global_retry` returns global value
/// regardless of what per-path configs are set.
#[test]
fn deprecated_per_path_retries_are_ignored_by_bridge() {
    let mut config = Config::default();
    config.fallback_policy.retry = const_retry(42);
    config.unified_api.retries = Some(const_retry(1)); // deprecated
    let router_id = RouterId::Named("r2".into());
    let router_cfg = RouterConfig {
        retries: Some(const_retry(2)), // deprecated
        ..RouterConfig::default()
    };
    config.routers = RouterConfigs::new(HashMap::from([(router_id, router_cfg)]));

    let result = resolved_global_retry(&config).unwrap();
    assert_eq!(
        *result,
        const_retry(42),
        "bridge must return fallback-policy.retry and ignore per-path configs"
    );
}

/// Warning emission is best-effort observability and must never break startup.
#[test]
fn warn_deprecated_per_path_retries_is_non_fatal() {
    let mut config = Config::default();
    config.unified_api.retries = Some(const_retry(1));
    let router_id = RouterId::Named("warn-router".into());
    let router_cfg = RouterConfig {
        retries: Some(const_retry(2)),
        ..RouterConfig::default()
    };
    config.routers = RouterConfigs::new(HashMap::from([(router_id, router_cfg)]));

    warn_deprecated_per_path_retries(&config);
}
