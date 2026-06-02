//! Pure-function tests for
//! `fallback_bridge::resolved_rate_limit_restore_after`.
//!
//! These tests verify the priority logic that replaced the two hardcoded
//! compile-time constants in `rate_limit/provider.rs`:
//!
//! * `DEFAULT_WAIT_SECONDS = 30`
//! * `RATE_LIMIT_BUFFER_SECONDS = 30s` (prod) / `1s` (test)
use std::time::Duration;

use ai_gateway::{
    config::{
        Config, fallback_bridge,
        fallback_policy::{
            FallbackMode, FallbackPolicyConfig, ProviderFailoverHealthPolicy,
            ProviderFailoverPolicyBlock, ProviderFailoverRateLimitPolicy,
        },
    },
    fallback::evaluator::{FailoverSource, should_remove},
    tests::TestDefault,
};

// ─── helpers
// ──────────────────────────────────────────────────────────────────

fn config_with_rate_limit(
    policy_enabled: bool,
    rl_enabled: bool,
    default_retry_after_secs: u64,
    restore_buffer: Duration,
) -> Config {
    let mut cfg = Config::test_default();
    cfg.fallback_policy = FallbackPolicyConfig {
        enabled: policy_enabled,
        mode: FallbackMode::Compat,
        provider_failover: ProviderFailoverPolicyBlock {
            health: ProviderFailoverHealthPolicy::default(),
            rate_limit: ProviderFailoverRateLimitPolicy {
                enabled: rl_enabled,
                default_retry_after_seconds: default_retry_after_secs,
                restore_buffer,
            },
        },
        ..FallbackPolicyConfig::test_default()
    };
    cfg
}

// ─── tests ────────────────────────────────────────────────────────────────────

/// When a `Retry-After` header is present the base comes from the header, not
/// the default.
#[test]
fn retry_after_header_overrides_default() {
    let cfg = config_with_rate_limit(true, true, 30, Duration::from_secs(5));
    let duration =
        fallback_bridge::resolved_rate_limit_restore_after(&cfg, Some(10));
    assert_eq!(duration, Duration::from_secs(10 + 5));
}

/// When no `Retry-After` header is present the policy default is used.
#[test]
fn no_header_uses_policy_default() {
    let cfg = config_with_rate_limit(true, true, 45, Duration::from_secs(10));
    let duration =
        fallback_bridge::resolved_rate_limit_restore_after(&cfg, None);
    assert_eq!(duration, Duration::from_secs(45 + 10));
}

/// Production compat: policy defaults (30 + 30) mirror the old hardcoded
/// constants exactly.
#[test]
fn compat_mode_matches_legacy_prod_constants() {
    let mut cfg = Config::test_default();
    cfg.fallback_policy = FallbackPolicyConfig::default();
    let duration =
        fallback_bridge::resolved_rate_limit_restore_after(&cfg, None);
    assert_eq!(duration, Duration::from_secs(30 + 30));
}

/// Test-mode compat: `test_default` (30 + 1s buffer) mirrors the old test-mode
/// constant `RATE_LIMIT_BUFFER_SECONDS = 1s`.
#[test]
fn compat_mode_matches_legacy_test_constants() {
    let cfg = Config::test_default();
    let duration =
        fallback_bridge::resolved_rate_limit_restore_after(&cfg, Some(2));
    // Retry-After: 2  +  restore_buffer: 1s (test_default)
    assert_eq!(duration, Duration::from_secs(3));
}

/// Even when the policy is disabled, the policy field values are still used
/// (the bridge always delegates to the evaluator).
#[test]
fn disabled_policy_still_uses_policy_field_values() {
    let cfg = config_with_rate_limit(false, false, 15, Duration::from_secs(3));
    let duration =
        fallback_bridge::resolved_rate_limit_restore_after(&cfg, None);
    assert_eq!(duration, Duration::from_secs(15 + 3));
}

/// Zero `Retry-After` header (e.g., provider replied with `Retry-After: 0`)
/// still applies the restore buffer.
#[test]
fn zero_retry_after_adds_buffer() {
    let cfg = config_with_rate_limit(true, true, 30, Duration::from_secs(10));
    let duration =
        fallback_bridge::resolved_rate_limit_restore_after(&cfg, Some(0));
    assert_eq!(duration, Duration::from_secs(10));
}

/// Disabled policy (or disabled rate-limit sub-block) should gate off
/// provider removal decisions for 429 handling.
#[test]
fn disabled_policy_disables_rate_limit_remove_decision() {
    let cfg = config_with_rate_limit(false, false, 15, Duration::from_secs(3));
    assert!(
        !should_remove(&cfg.fallback_policy, FailoverSource::RateLimit),
        "rate-limit remove decision must be disabled when policy is disabled"
    );
}
