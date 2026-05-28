//! Redis key `policy:piicache:{workspace_id}` controls whether the gateway
//! attaches the inbound HTTP body to `EvaluateRequest.body`.

use std::sync::Arc;

use bytes::Bytes;
use opentelemetry::KeyValue;
use tracing::info;

use crate::{
    app_redis::AppRedis, config::policy::POLICY_MAX_REQUEST_BODY_BYTES,
    metrics::VkMetrics, policy_proto::EvaluateRequest,
};

pub const PIICACHE_REDIS_KEY_PREFIX: &str = "policy:piicache:";

#[must_use]
pub fn piicache_redis_key(workspace_id: &str) -> String {
    let mut s = String::with_capacity(
        PIICACHE_REDIS_KEY_PREFIX.len() + workspace_id.len(),
    );
    s.push_str(PIICACHE_REDIS_KEY_PREFIX);
    s.push_str(workspace_id);
    s
}

/// `true` only when Redis returned a string that equals `"true"` after trim.
#[must_use]
pub fn piicache_redis_value_is_true(raw: &str) -> bool {
    raw.trim() == "true"
}

#[must_use]
pub fn truncate_request_body_bytes(body: &Bytes, max: usize) -> (Bytes, bool) {
    if body.len() <= max {
        (body.clone(), false)
    } else {
        (body.slice(..max), true)
    }
}

pub async fn attach_request_body_from_piicache_redis(
    redis: Option<&Arc<AppRedis>>,
    workspace_id: &str,
    body: &Bytes,
    req: &mut EvaluateRequest,
    vk_metrics: &VkMetrics,
) {
    use tracing::warn;

    let key = piicache_redis_key(workspace_id);
    let Some(r) = redis else {
        vk_metrics
            .policy_piicache_request_body
            .add(1, &[KeyValue::new("outcome", "no_redis")]);
        return;
    };

    match r.get_opt_string(&key).await {
        Ok(opt) => {
            info!(
                "piicache redis GET key={} value={}",
                key,
                opt.as_deref().unwrap_or("<nil>")
            );
            match opt {
                Some(ref v) if piicache_redis_value_is_true(v) => {
                    let (slice, truncated) = truncate_request_body_bytes(
                        body,
                        POLICY_MAX_REQUEST_BODY_BYTES,
                    );
                    req.body = slice.to_vec();
                    let outcome = if truncated {
                        "attached_truncated"
                    } else {
                        "attached"
                    };
                    vk_metrics
                        .policy_piicache_request_body
                        .add(1, &[KeyValue::new("outcome", outcome)]);
                }
                Some(v) if v.trim() == "false" => {
                    vk_metrics
                        .policy_piicache_request_body
                        .add(1, &[KeyValue::new("outcome", "skipped_false")]);
                }
                None => {
                    vk_metrics
                        .policy_piicache_request_body
                        .add(1, &[KeyValue::new("outcome", "skipped_nil")]);
                }
                Some(_) => {
                    vk_metrics
                        .policy_piicache_request_body
                        .add(1, &[KeyValue::new("outcome", "skipped_other")]);
                }
            }
        }
        Err(e) => {
            warn!(key = %key, error = %e, "piicache redis GET failed");
            vk_metrics
                .policy_piicache_request_body
                .add(1, &[KeyValue::new("outcome", "redis_error")]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_key_format() {
        assert_eq!(
            piicache_redis_key("550e8400-e29b-41d4-a716-446655440000"),
            "policy:piicache:550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn true_only_after_trim() {
        assert!(piicache_redis_value_is_true("  true  "));
        assert!(!piicache_redis_value_is_true("TRUE"));
        assert!(!piicache_redis_value_is_true("false"));
        assert!(!piicache_redis_value_is_true("yes"));
    }

    #[test]
    fn truncate_bytes_not_char_boundaries() {
        let body = Bytes::from_static(b"abcdef");
        let (out, cut) = truncate_request_body_bytes(&body, 4);
        assert!(cut);
        assert_eq!(&out[..], b"abcd");
    }
}
