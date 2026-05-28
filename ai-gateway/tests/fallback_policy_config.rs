use std::time::Duration;

use ai_gateway::{
    config::{
        Config,
        fallback_policy::{
            FallbackMode, FallbackPolicyConfig, ProviderFailoverHealthPolicy,
            ProviderFailoverRateLimitPolicy,
        },
        monitor::GracePeriod,
        retry::RetryConfig,
    },
    error::init::InitError,
};
use rust_decimal::Decimal;
use serde_json::json;

#[test]
fn fallback_policy_defaults_validate() {
    let p = FallbackPolicyConfig::default();
    p.validate().expect("default policy should validate");
}

#[test]
fn fallback_policy_deserialize_minimal_round_trip() {
    let j = json!({
        "enabled": false,
        "mode": "unified",
        "retry": {
            "strategy": "constant",
            "delay": "100ms",
            "max-retries": 1
        },
        "provider-failover": {
            "health": {
                "enabled": true,
                "error-ratio-threshold": 0.15,
                "interval": "10s",
                "grace-period": { "min-requests": 10 }
            },
            "rate-limit": {
                "enabled": true,
                "default-retry-after-seconds": 45,
                "restore-buffer": "5s"
            }
        },
        "observability": {
            "emit-decision-log": false,
            "metrics-prefix": "test.fallback"
        }
    });
    let p: FallbackPolicyConfig = serde_json::from_value(j.clone()).unwrap();
    assert!(!p.enabled);
    assert_eq!(p.mode, FallbackMode::Unified);
    assert!(matches!(p.retry, RetryConfig::Constant { .. }));
    assert_eq!(
        p.provider_failover.health,
        ProviderFailoverHealthPolicy {
            enabled: true,
            error_ratio_threshold: Decimal::new(15, 2),
            interval: Duration::from_secs(10),
            grace_period: GracePeriod::Requests { min_requests: 10 },
        }
    );
    assert_eq!(
        p.provider_failover.rate_limit,
        ProviderFailoverRateLimitPolicy {
            enabled: true,
            default_retry_after_seconds: 45,
            restore_buffer: Duration::from_secs(5),
        }
    );
    assert!(!p.observability.emit_decision_log);
    assert_eq!(p.observability.metrics_prefix, "test.fallback");
    p.validate().unwrap();

    let again: FallbackPolicyConfig =
        serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
    assert_eq!(p, again);
}

#[test]
fn fallback_policy_merged_into_full_config() {
    let mut v = serde_json::to_value(Config::default()).unwrap();
    v["fallback-policy"] = json!({
        "mode": "unified",
        "retry": { "strategy": "exponential", "max-retries": 4 }
    });
    let cfg: Config = serde_json::from_value(v).unwrap();
    assert_eq!(cfg.fallback_policy.mode, FallbackMode::Unified);
    assert_eq!(
        cfg.fallback_policy.retry,
        RetryConfig::Exponential {
            min_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            max_retries: 4,
            factor: rust_decimal::Decimal::try_from(2.0_f32).unwrap(),
        }
    );
    cfg.fallback_policy.validate().unwrap();
}

#[test]
fn fallback_policy_validate_rejects_bad_health_interval() {
    let mut p = FallbackPolicyConfig::default();
    p.provider_failover.health.interval = Duration::ZERO;
    let err = p.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("interval"), "unexpected error message: {msg}");
}

#[test]
fn fallback_policy_deserialize_grace_period_accepts_snake_case_alias() {
    let p: FallbackPolicyConfig = serde_json::from_value(json!({
        "provider-failover": {
            "health": {
                "grace-period": { "min_requests": 7 }
            }
        }
    }))
    .expect("min_requests alias should deserialize");

    assert_eq!(
        p.provider_failover.health.grace_period,
        GracePeriod::Requests { min_requests: 7 }
    );
}

#[test]
fn fallback_policy_validate_rejects_bad_error_ratio_threshold() {
    let mut p = FallbackPolicyConfig::default();
    p.provider_failover.health.error_ratio_threshold = Decimal::new(12, 1); // 1.2
    let err = p.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("error-ratio-threshold"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn fallback_policy_validate_rejects_zero_grace_period_requests() {
    let mut p = FallbackPolicyConfig::default();
    p.provider_failover.health.grace_period = GracePeriod::Requests { min_requests: 0 };
    let err = p.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("min-requests"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn fallback_policy_validate_rejects_bad_rate_limit_defaults() {
    let mut p = FallbackPolicyConfig::default();
    p.provider_failover.rate_limit.default_retry_after_seconds = 0;
    let err = p.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("default-retry-after-seconds"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn fallback_policy_validate_rejects_retry_min_delay_greater_than_max_delay() {
    let p = FallbackPolicyConfig {
        retry: RetryConfig::Exponential {
            min_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(1),
            max_retries: 1,
            factor: Decimal::new(2, 0),
        },
        ..FallbackPolicyConfig::default()
    };
    let err = p.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("min-delay"), "unexpected error message: {msg}");
}

#[test]
fn fallback_policy_validate_rejects_empty_metrics_prefix() {
    let mut p = FallbackPolicyConfig::default();
    p.observability.metrics_prefix = "   ".to_string();
    let err = p.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("metrics-prefix"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn config_validate_surfaces_invalid_fallback_policy() {
    let mut cfg = Config::default();
    cfg.fallback_policy.provider_failover.health.interval = Duration::ZERO;
    let err = cfg.validate().expect_err("config should fail validation");
    assert!(
        matches!(err, InitError::InvalidFallbackPolicy(_)),
        "unexpected error variant: {err:?}"
    );
}
