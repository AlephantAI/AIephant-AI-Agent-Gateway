//! Fallback evaluator: pure-function policy judgements used by Dispatcher,
//! `HealthMonitor`, and `RateLimitMonitor`.
//!
//! All functions are **pure** — no IO, no `AppState` dependency. Callers
//! supply the resolved `FallbackPolicyConfig` and an `ErrorClass`.

use std::time::Duration;

use crate::config::fallback_policy::{FallbackMode, FallbackPolicyConfig};

// ──────────────────────────────────────────────────────────────────────────────
// Error classification
// ──────────────────────────────────────────────────────────────────────────────

/// High-level classification of an error for retry eligibility.
///
/// `compat` mode maps exactly to the conditions currently hard-coded in
/// `dispatcher/service.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// HTTP 5xx from upstream (sync response status code).
    Upstream5xx,
    /// TCP/TLS connection failure (`reqwest::Error::is_connect()`).
    ConnectionError,
    /// Stream-layer retryable event (UTF-8, parser, transport,
    /// 5xx `InvalidStatusCode`); maps to `StreamError::is_retryable()`.
    StreamRetryable,
    /// Any other error that is not retried.
    NonRetryable,
}

// ──────────────────────────────────────────────────────────────────────────────
// Failover source (health vs rate-limit)
// ──────────────────────────────────────────────────────────────────────────────

/// The reason a provider is being considered for removal / rate-limit pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverSource {
    /// Error-ratio threshold exceeded (`HealthMonitor`).
    Health,
    /// Provider returned 429 (`RateLimitMonitor`).
    RateLimit,
}

// ──────────────────────────────────────────────────────────────────────────────
// Core judgements
// ──────────────────────────────────────────────────────────────────────────────

/// Return `true` when the error class is eligible for a same-provider retry.
///
/// Both `compat` and `unified` modes share this predicate; the only
/// difference is the source of the `RetryConfig` parameters (Task 3/4
/// wires the global `FallbackPolicyConfig` in `compat` mode too).
#[must_use]
pub fn should_retry(policy: &FallbackPolicyConfig, error_class: ErrorClass) -> bool {
    if !policy.enabled {
        return false;
    }
    match policy.mode {
        FallbackMode::Compat | FallbackMode::Unified => matches!(
            error_class,
            ErrorClass::Upstream5xx | ErrorClass::ConnectionError | ErrorClass::StreamRetryable
        ),
    }
}

/// Return `true` when a provider should be removed from the Balancer pool
/// given a failover event.
#[must_use]
pub fn should_remove(policy: &FallbackPolicyConfig, source: FailoverSource) -> bool {
    if !policy.enabled {
        return false;
    }
    match source {
        FailoverSource::Health => policy.provider_failover.health.enabled,
        FailoverSource::RateLimit => policy.provider_failover.rate_limit.enabled,
    }
}

/// Duration to wait before re-inserting a 429-removed provider.
///
/// `retry_after_header_secs` is the parsed value from the provider's
/// `Retry-After` header (or `None` if absent / unparseable).
#[must_use]
pub fn restore_after(
    policy: &FallbackPolicyConfig,
    retry_after_header_secs: Option<u64>,
) -> Duration {
    let base = Duration::from_secs(
        retry_after_header_secs.unwrap_or(
            policy
                .provider_failover
                .rate_limit
                .default_retry_after_seconds,
        ),
    );
    base + policy.provider_failover.rate_limit.restore_buffer
}
