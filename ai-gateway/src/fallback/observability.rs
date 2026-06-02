//! Structured decision logging for the unified fallback policy.
//!
//! All logging is gated on `fallback-policy.observability.emit-decision-log`.
//! Calls that return before emitting are zero-cost (no allocation, no lock).

use crate::{config::fallback_policy::FallbackPolicyConfig, fallback::evaluator::FailoverSource};

// ──────────────────────────────────────────────────────────────────────────────
// Decision kind
// ──────────────────────────────────────────────────────────────────────────────

/// The type of fallback decision being logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    /// Dispatcher is retrying the same provider after a transient error.
    Retry,
    /// Dispatcher switched to another provider after retries were exhausted.
    CrossProviderFallback,
    /// Monitor is removing a provider from the balancer pool.
    Remove,
    /// Monitor is re-inserting a provider after its grace period expires.
    Restore,
    /// F-10: request rejected because the provider is not in the workspace
    /// allowlist.
    ProviderDenied,
}

impl DecisionKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::CrossProviderFallback => "cross_provider_fallback",
            Self::Remove => "remove",
            Self::Restore => "restore",
            Self::ProviderDenied => "provider_denied",
        }
    }
}

impl FailoverSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::RateLimit => "rate_limit",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Core emit function
// ──────────────────────────────────────────────────────────────────────────────

/// Emit a structured fallback decision log when
/// `policy.observability.emit_decision_log` is `true`.
///
/// This is the **single canonical emit point** for all fallback decisions.
/// Callers supply the resolved policy and the relevant decision details.
/// When `emit_decision_log` is `false` the function is a pure no-op.
pub fn log_decision(
    policy: &FallbackPolicyConfig,
    kind: DecisionKind,
    source: Option<FailoverSource>,
    provider: &dyn std::fmt::Display,
) {
    if !policy.observability.emit_decision_log {
        return;
    }
    let decision = kind.as_str();
    let failover_source = source.map(FailoverSource::as_str);
    tracing::info!(
        target: "ai_gateway::fallback::decision",
        decision,
        failover_source,
        provider = %provider,
        "fallback decision",
    );
}
