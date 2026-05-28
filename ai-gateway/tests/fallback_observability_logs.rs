//! Tests for `fallback::observability::log_decision`.
//!
//! Covers:
//! - `DecisionKind::as_str` / `FailoverSource::as_str` field-name contracts.
//! - Gate: `emit_decision_log = false` suppresses the log event entirely.
//! - Gate: `emit_decision_log = true` emits exactly one log event.
//! - Structured fields (`decision`, `failover_source`, `provider`) are present
//!   with the correct values.

use std::sync::{Arc, Mutex};

use ai_gateway::{
    config::fallback_policy::{FallbackObservabilityPolicy, FallbackPolicyConfig},
    fallback::{
        evaluator::FailoverSource,
        observability::{DecisionKind, log_decision},
    },
};
use tracing::field::{Field, Visit};
use tracing_subscriber::{Layer, layer::SubscriberExt};

// ──────────────────────────────────────────────────────────────────────────────
// Minimal tracing layer that captures structured fields from log events.
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
struct RecordedEvents(Arc<Mutex<Vec<CapturedEvent>>>);

#[derive(Debug, Default, Clone)]
struct CapturedEvent {
    decision: Option<String>,
    failover_source: Option<String>,
    provider: Option<String>,
}

struct FieldVisitor<'a>(&'a mut CapturedEvent);

impl Visit for FieldVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "decision" => self.0.decision = Some(value.to_owned()),
            "failover_source" => {
                self.0.failover_source = Some(value.to_owned());
            }
            "provider" => self.0.provider = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `provider` is formatted with `%` (Display), so it may arrive here
        // when the subscriber formats it as debug.
        self.record_str(field, &format!("{value:?}"));
    }
}

impl<S: tracing::Subscriber> Layer<S> for RecordedEvents {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        // Only capture events from our decision log target.
        if event.metadata().target() != "ai_gateway::fallback::decision" {
            return;
        }
        let mut captured = CapturedEvent::default();
        event.record(&mut FieldVisitor(&mut captured));
        self.0.lock().unwrap().push(captured);
    }
}

fn make_subscriber(recorder: RecordedEvents) -> impl tracing::Subscriber {
    tracing_subscriber::registry().with(recorder)
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn policy_with_emit(emit: bool) -> FallbackPolicyConfig {
    FallbackPolicyConfig {
        observability: FallbackObservabilityPolicy {
            emit_decision_log: emit,
            metrics_prefix: "ai_gateway.fallback".to_string(),
        },
        ..Default::default()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// DecisionKind::as_str — field-name contract
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn decision_kind_as_str_retry() {
    assert_eq!(DecisionKind::Retry.as_str(), "retry");
}

#[test]
fn decision_kind_as_str_remove() {
    assert_eq!(DecisionKind::Remove.as_str(), "remove");
}

#[test]
fn decision_kind_as_str_restore() {
    assert_eq!(DecisionKind::Restore.as_str(), "restore");
}

#[test]
fn decision_kind_as_str_provider_denied() {
    assert_eq!(DecisionKind::ProviderDenied.as_str(), "provider_denied");
}

// ──────────────────────────────────────────────────────────────────────────────
// FailoverSource::as_str — field-name contract
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn failover_source_as_str_health() {
    assert_eq!(FailoverSource::Health.as_str(), "health");
}

#[test]
fn failover_source_as_str_rate_limit() {
    assert_eq!(FailoverSource::RateLimit.as_str(), "rate_limit");
}

// ──────────────────────────────────────────────────────────────────────────────
// Gate: emit_decision_log = false  →  no log event emitted
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn log_decision_suppressed_when_disabled() {
    let recorder = RecordedEvents::default();
    let subscriber = make_subscriber(recorder.clone());

    tracing::subscriber::with_default(subscriber, || {
        let policy = policy_with_emit(false);
        log_decision(
            &policy,
            DecisionKind::Remove,
            Some(FailoverSource::Health),
            &"openai",
        );
    });

    let events = recorder.0.lock().unwrap();
    assert!(
        events.is_empty(),
        "expected no events when emit_decision_log = false, got {events:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Gate: emit_decision_log = true  →  exactly one event with correct fields
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn log_decision_emitted_when_enabled() {
    let recorder = RecordedEvents::default();
    let subscriber = make_subscriber(recorder.clone());

    tracing::subscriber::with_default(subscriber, || {
        let policy = policy_with_emit(true);
        log_decision(
            &policy,
            DecisionKind::Remove,
            Some(FailoverSource::Health),
            &"openai",
        );
    });

    let events = recorder.0.lock().unwrap();
    assert_eq!(
        events.len(),
        1,
        "expected exactly 1 event, got {}",
        events.len()
    );

    let ev = &events[0];
    assert_eq!(
        ev.decision.as_deref(),
        Some("remove"),
        "decision field mismatch: {ev:?}"
    );
    assert_eq!(
        ev.failover_source.as_deref(),
        Some("health"),
        "failover_source field mismatch: {ev:?}"
    );
    assert!(
        ev.provider.as_deref().is_some_and(|p| p.contains("openai")),
        "provider field should contain 'openai': {ev:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Retry decision: no failover_source field (Option<None>)
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn log_decision_retry_has_no_failover_source() {
    let recorder = RecordedEvents::default();
    let subscriber = make_subscriber(recorder.clone());

    tracing::subscriber::with_default(subscriber, || {
        let policy = policy_with_emit(true);
        log_decision(&policy, DecisionKind::Retry, None, &"anthropic");
    });

    let events = recorder.0.lock().unwrap();
    assert_eq!(events.len(), 1, "expected 1 event");
    let ev = &events[0];
    assert_eq!(ev.decision.as_deref(), Some("retry"));
    // failover_source is None → the field is recorded as the option's Display
    // which evaluates to "None" or is absent entirely.
    assert!(
        ev.failover_source.as_ref().is_none_or(|s| s == "None"),
        "failover_source should be absent or 'None' for retry, got: {ev:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Rate-limit restore: correct fields
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn log_decision_restore_rate_limit_fields() {
    let recorder = RecordedEvents::default();
    let subscriber = make_subscriber(recorder.clone());

    tracing::subscriber::with_default(subscriber, || {
        let policy = policy_with_emit(true);
        log_decision(
            &policy,
            DecisionKind::Restore,
            Some(FailoverSource::RateLimit),
            &"anthropic",
        );
    });

    let events = recorder.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!(ev.decision.as_deref(), Some("restore"));
    assert_eq!(ev.failover_source.as_deref(), Some("rate_limit"));
}

// ──────────────────────────────────────────────────────────────────────────────
// ProviderDenied: fields correct
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn log_decision_provider_denied_fields() {
    let recorder = RecordedEvents::default();
    let subscriber = make_subscriber(recorder.clone());

    tracing::subscriber::with_default(subscriber, || {
        let policy = policy_with_emit(true);
        log_decision(&policy, DecisionKind::ProviderDenied, None, &"openai");
    });

    let events = recorder.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!(ev.decision.as_deref(), Some("provider_denied"));
}
