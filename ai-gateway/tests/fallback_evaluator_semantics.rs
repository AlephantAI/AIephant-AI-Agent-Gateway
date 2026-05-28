//! Table-driven "golden" tests for evaluator compat semantics.
//!
//! These pin the compat mode to the exact retry conditions currently
//! hard-coded in `dispatcher/service.rs`, so a future refactor can't
//! silently regress the behaviour.

use std::time::Duration;

use ai_gateway::{
    config::fallback_policy::{
        FallbackMode, FallbackPolicyConfig, ProviderFailoverHealthPolicy,
        ProviderFailoverRateLimitPolicy,
    },
    fallback::evaluator::{
        ErrorClass, FailoverSource, restore_after, should_remove, should_retry,
    },
};

// ─── should_retry ────────────────────────────────────────────────────────────

/// Pairs of (`ErrorClass`, `expected_retry_result`) for a default (enabled,
/// compat)
/// policy — golden values derived from dispatcher/service.rs conditions.
const RETRY_GOLDEN: &[(ErrorClass, bool)] = &[
    (ErrorClass::Upstream5xx, true),
    (ErrorClass::ConnectionError, true),
    (ErrorClass::StreamRetryable, true),
    (ErrorClass::NonRetryable, false),
];

#[test]
fn compat_should_retry_golden() {
    let policy = FallbackPolicyConfig {
        mode: FallbackMode::Compat,
        ..FallbackPolicyConfig::default()
    };
    for &(class, expected) in RETRY_GOLDEN {
        assert_eq!(
            should_retry(&policy, class),
            expected,
            "compat should_retry({class:?}) should be {expected}"
        );
    }
}

#[test]
fn unified_should_retry_same_as_compat() {
    let compat = FallbackPolicyConfig {
        mode: FallbackMode::Compat,
        ..FallbackPolicyConfig::default()
    };
    let unified = FallbackPolicyConfig {
        mode: FallbackMode::Unified,
        ..FallbackPolicyConfig::default()
    };
    for &(class, _) in RETRY_GOLDEN {
        assert_eq!(
            should_retry(&compat, class),
            should_retry(&unified, class),
            "unified and compat should agree on {class:?}",
        );
    }
}

#[test]
fn disabled_policy_never_retries() {
    let policy = FallbackPolicyConfig {
        enabled: false,
        ..FallbackPolicyConfig::default()
    };
    for &(class, _) in RETRY_GOLDEN {
        assert!(
            !should_retry(&policy, class),
            "disabled policy should not retry {class:?}",
        );
    }
}

// ─── should_remove ───────────────────────────────────────────────────────────

#[test]
fn should_remove_health_and_rate_limit_enabled_by_default() {
    let policy = FallbackPolicyConfig::default();
    assert!(should_remove(&policy, FailoverSource::Health));
    assert!(should_remove(&policy, FailoverSource::RateLimit));
}

#[test]
fn should_remove_respects_per_source_enabled_flag() {
    let mut policy = FallbackPolicyConfig::default();
    policy.provider_failover.health = ProviderFailoverHealthPolicy {
        enabled: false,
        ..Default::default()
    };
    assert!(!should_remove(&policy, FailoverSource::Health));
    assert!(should_remove(&policy, FailoverSource::RateLimit));
}

#[test]
fn should_remove_respects_rate_limit_enabled_flag() {
    let mut policy = FallbackPolicyConfig::default();
    policy.provider_failover.rate_limit = ProviderFailoverRateLimitPolicy {
        enabled: false,
        ..Default::default()
    };
    assert!(should_remove(&policy, FailoverSource::Health));
    assert!(!should_remove(&policy, FailoverSource::RateLimit));
}

#[test]
fn should_remove_disabled_policy_returns_false() {
    let policy = FallbackPolicyConfig {
        enabled: false,
        ..FallbackPolicyConfig::default()
    };
    assert!(!should_remove(&policy, FailoverSource::Health));
    assert!(!should_remove(&policy, FailoverSource::RateLimit));
}

// ─── restore_after ───────────────────────────────────────────────────────────

/// Default config: 30s retry-after + 30s buffer.
#[test]
fn restore_after_no_header_uses_default_plus_buffer() {
    let policy = FallbackPolicyConfig::default();
    assert_eq!(restore_after(&policy, None), Duration::from_secs(60));
}

/// When Retry-After header is present, use it as base.
#[test]
fn restore_after_uses_header_plus_buffer() {
    let policy = FallbackPolicyConfig::default();
    // header says 45s, buffer is 30s → 75s
    assert_eq!(restore_after(&policy, Some(45)), Duration::from_secs(75));
}

#[test]
fn restore_after_custom_buffer() {
    let mut policy = FallbackPolicyConfig::default();
    policy.provider_failover.rate_limit = ProviderFailoverRateLimitPolicy {
        default_retry_after_seconds: 10,
        restore_buffer: Duration::from_secs(1),
        ..Default::default()
    };
    assert_eq!(restore_after(&policy, None), Duration::from_secs(11));
    assert_eq!(restore_after(&policy, Some(20)), Duration::from_secs(21));
}

/// Rate-limit monitor golden: default `retry_after` + buffer == 30 + 30 = 60s
/// (matches `DEFAULT_WAIT_SECONDS` + `RATE_LIMIT_BUFFER_SECONDS` in prod).
#[test]
fn restore_after_compat_matches_monitor_defaults() {
    let policy = FallbackPolicyConfig::default();
    let prod_wait_secs: u64 = 30;
    let prod_buffer_secs: u64 = 30;
    assert_eq!(
        restore_after(&policy, None),
        Duration::from_secs(prod_wait_secs + prod_buffer_secs)
    );
}
