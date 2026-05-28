use ai_gateway::{
    config::fallback_policy::FallbackPolicyConfig,
    fallback::evaluator::{ErrorClass, FailoverSource, restore_after, should_remove, should_retry},
};

#[test]
fn stub_should_retry_placeholder() {
    let policy = FallbackPolicyConfig::default();
    // Default policy is enabled; at least one error class is retryable.
    assert!(should_retry(&policy, ErrorClass::Upstream5xx));
}

#[test]
fn stub_should_remove_placeholder() {
    let policy = FallbackPolicyConfig::default();
    assert!(should_remove(&policy, FailoverSource::Health));
}

#[test]
fn stub_restore_after_placeholder() {
    use std::time::Duration;
    let policy = FallbackPolicyConfig::default();
    let d = restore_after(&policy, None);
    // default: 30s base + 30s buffer = 60s
    assert_eq!(d, Duration::from_secs(60));
}
