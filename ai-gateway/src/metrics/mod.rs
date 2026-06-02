pub mod attribute_extractor;
pub mod request_count;
pub mod rolling_counter;
pub mod system;
pub mod tfft;

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};

pub use self::rolling_counter::RollingCounter;

/// The top level struct that contains all metrics
/// which are exported to OpenTelemetry.
#[derive(Debug, Clone)]
pub struct Metrics {
    pub error_count: Counter<u64>,
    pub provider_health: Gauge<u64>,
    pub auth_attempts: Counter<u64>,
    pub auth_rejections: Counter<u64>,
    pub request_count: Counter<u64>,
    pub response_count: Counter<u64>,
    pub tfft_duration: Histogram<f64>,
    pub ingest_log_sends: Counter<u64>,
    pub ingest_log_errors: Counter<u64>,
    pub workspace_concurrency_redis_incr: Counter<u64>,
    pub workspace_concurrency_redis_decr: Counter<u64>,
    /// Global client-IP rate limit: allowed (pass) decisions.
    pub client_ip_rate_limit_allowed: Counter<u64>,
    /// Global client-IP rate limit: rejected (429) decisions.
    pub client_ip_rate_limit_rejected: Counter<u64>,
    /// Global client-IP rate limit: Redis path failed, fell back to memory.
    pub client_ip_rate_limit_redis_degraded: Counter<u64>,
    /// Whole-gateway in-flight: allowed (acquired slot).
    pub gateway_in_flight_allowed: Counter<u64>,
    /// Whole-gateway in-flight: rejected (429).
    pub gateway_in_flight_rejected: Counter<u64>,
    /// Whole-gateway in-flight: Redis acquire failed, used memory fallback.
    pub gateway_in_flight_redis_degraded: Counter<u64>,
    /// 1 = rate-limit layer has fallen back to in-memory; 0 = using Redis.
    /// Labels: `layer` ∈ {"client_ip", "in_flight"}
    pub rate_limit_redis_degraded_gauge: Gauge<u64>,
    /// Labels: `profile`, `explicit` ∈ {`true`,`false`}
    pub mapper_client_profile_resolved: Counter<u64>,
    /// Labels: `passthrough` ∈ {`true`,`false`}
    pub mapper_native_semantic_passthrough: Counter<u64>,
    /// Labels: `profile`, `applied` ∈ {`true`,`false`}
    pub mapper_ide_ingress_adjust_total: Counter<u64>,
    pub routers: RouterMetrics,
    pub vk: VkMetrics,
}

impl Metrics {
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        let error_count = meter
            .u64_counter("error_count")
            .with_description("Number of error occurences")
            .build();
        let provider_health = meter
            .u64_gauge("provider_health")
            .with_description("Upstream provider health")
            .build();
        let auth_attempts = meter
            .u64_counter("auth_attempts")
            .with_description("Number of authentication attempts")
            .build();
        let auth_rejections = meter
            .u64_counter("auth_rejections")
            .with_description("Number of unauthenticated requests")
            .build();
        let request_count = meter
            .u64_counter("request_count")
            .with_description("Total request count")
            .build();
        let response_count = meter
            .u64_counter("response_count")
            .with_description("Number of successful responses")
            .build();
        let tfft_duration = meter
            .f64_histogram("tfft_duration")
            .with_unit("ms")
            .with_description("Time to first token duration")
            .build();
        let ingest_log_sends = meter
            .u64_counter("ingest_log_sends")
            .with_description("Request log deliveries by transport mode")
            .build();
        let ingest_log_errors = meter
            .u64_counter("ingest_log_errors")
            .with_description("Request log delivery failures")
            .build();
        let workspace_concurrency_redis_incr = meter
            .u64_counter("workspace_concurrency_redis_incr_total")
            .with_description("MQ Redis workspace concurrency INCR attempts")
            .build();
        let workspace_concurrency_redis_decr = meter
            .u64_counter("workspace_concurrency_redis_decr_total")
            .with_description("MQ Redis workspace concurrency DECR attempts")
            .build();
        let client_ip_rate_limit_allowed = meter
            .u64_counter("client_ip_rate_limit_allowed_total")
            .with_description("Global client IP rate limit: allowed")
            .build();
        let client_ip_rate_limit_rejected = meter
            .u64_counter("client_ip_rate_limit_rejected_total")
            .with_description("Global client IP rate limit: rejected (429)")
            .build();
        let client_ip_rate_limit_redis_degraded = meter
            .u64_counter("client_ip_rate_limit_redis_degraded_total")
            .with_description(
                "Global client IP rate limit: Redis error, used memory \
                 fallback",
            )
            .build();
        let gateway_in_flight_allowed = meter
            .u64_counter("gateway_in_flight_allowed_total")
            .with_description("Whole-gateway in-flight: allowed")
            .build();
        let gateway_in_flight_rejected = meter
            .u64_counter("gateway_in_flight_rejected_total")
            .with_description("Whole-gateway in-flight: rejected (429)")
            .build();
        let gateway_in_flight_redis_degraded = meter
            .u64_counter("gateway_in_flight_redis_degraded_total")
            .with_description(
                "Whole-gateway in-flight: Redis error, used memory fallback",
            )
            .build();
        let rate_limit_redis_degraded_gauge = meter
            .u64_gauge("ai_gateway_rate_limit_redis_degraded")
            .with_description(
                "1 when rate-limit layer is using in-memory fallback instead \
                 of Redis",
            )
            .build();
        let mapper_client_profile_resolved = meter
            .u64_counter("mapper_client_profile_resolved_total")
            .with_description(
                "Mapper: client profile resolution (header + heuristic)",
            )
            .build();
        let mapper_native_semantic_passthrough = meter
            .u64_counter("mapper_native_semantic_passthrough_total")
            .with_description(
                "Mapper: native semantic passthrough decision per request",
            )
            .build();
        let mapper_ide_ingress_adjust_total = meter
            .u64_counter("mapper_ide_ingress_adjust_total")
            .with_description(
                "Mapper: IDE ingress preprocessing invocations by profile and \
                 applied flag",
            )
            .build();
        let routers = RouterMetrics::new(meter);
        let vk = VkMetrics::new(meter);
        Self {
            error_count,
            provider_health,
            auth_attempts,
            auth_rejections,
            request_count,
            response_count,
            tfft_duration,
            ingest_log_sends,
            ingest_log_errors,
            workspace_concurrency_redis_incr,
            workspace_concurrency_redis_decr,
            client_ip_rate_limit_allowed,
            client_ip_rate_limit_rejected,
            client_ip_rate_limit_redis_degraded,
            gateway_in_flight_allowed,
            gateway_in_flight_rejected,
            gateway_in_flight_redis_degraded,
            rate_limit_redis_degraded_gauge,
            mapper_client_profile_resolved,
            mapper_native_semantic_passthrough,
            mapper_ide_ingress_adjust_total,
            routers,
            vk,
        }
    }
}

/// Counters for virtual-key model access policy and rate-limit events.
/// Only incremented in Cloud mode (`VkPolicy` is only present there).
#[derive(Debug, Clone)]
pub struct VkMetrics {
    /// Requests blocked by `allowed_models` / `blocked_models` policy (HTTP
    /// 403).
    pub model_denied: Counter<u64>,
    /// Content-filter gRPC: allowed.
    pub content_filter_allowed: Counter<u64>,
    /// Content-filter gRPC: denied by policy.
    pub content_filter_denied: Counter<u64>,
    /// Content-filter gRPC: unavailable / error path (excluding explicit
    /// DENY).
    pub content_filter_unavailable: Counter<u64>,
    /// Labels: `outcome` = attached | attached_truncated | skipped_false |
    /// skipped_nil | skipped_other | redis_error | no_redis
    pub policy_piicache_request_body: Counter<u64>,
    /// Prompt cache: `outcome` = skipped_no_header | skipped_no_workspace |
    /// miss | hit_injected | hit_noop_empty_template | parse_or_shape_400 |
    /// redis_error | no_redis | prompt_id_too_long_400
    pub policy_prompt_cache_messages: Counter<u64>,
    /// VK auth: memory miss, PG row found and written to `virtual_keys_cache`.
    pub pg_fallback_heal: Counter<u64>,
    /// VK auth: memory miss, PG query failed (not “no row”).
    pub pg_fallback_db_errors: Counter<u64>,
}

impl VkMetrics {
    #[must_use]
    #[allow(clippy::similar_names)]
    pub fn new(meter: &Meter) -> Self {
        let model_denied = meter
            .u64_counter("vk_model_denied")
            .with_description(
                "Requests blocked by virtual key model access policy",
            )
            .build();
        let content_filter_allowed = meter
            .u64_counter("content_filter_allowed")
            .with_description("Content filter policy allowed the request")
            .build();
        let content_filter_denied = meter
            .u64_counter("content_filter_denied")
            .with_description("Content filter policy denied the request")
            .build();
        let content_filter_unavailable = meter
            .u64_counter("content_filter_unavailable")
            .with_description(
                "Content filter unavailable or error when evaluating",
            )
            .build();
        let policy_piicache_request_body = meter
            .u64_counter("policy_piicache_request_body")
            .with_description(
                "Policy Evaluate body attachment driven by Redis piicache flag",
            )
            .build();
        let policy_prompt_cache_messages = meter
            .u64_counter("policy_prompt_cache_messages")
            .with_description(
                "Prompt cache Redis merge of template.messages after policy \
                 allow",
            )
            .build();
        let pg_fallback_heal = meter
            .u64_counter("vk_pg_fallback_heal_total")
            .with_description(
                "Virtual key auth: PG fallback found row and upserted cache",
            )
            .build();
        let pg_fallback_db_errors = meter
            .u64_counter("vk_pg_fallback_db_errors_total")
            .with_description(
                "Virtual key auth: PG fallback query error (auth still 401)",
            )
            .build();
        Self {
            model_denied,
            content_filter_allowed,
            content_filter_denied,
            content_filter_unavailable,
            policy_piicache_request_body,
            policy_prompt_cache_messages,
            pg_fallback_heal,
            pg_fallback_db_errors,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouterMetrics {
    /// labels:
    /// - `router_id`
    /// - `org_id`
    pub routers: UpDownCounter<i64>,
    /// labels:
    /// - `router_id`
    /// - `endpoint_type`
    pub router_strategies: UpDownCounter<i64>,
    /// labels:
    /// - `router_id`
    pub model_mappings: UpDownCounter<i64>,
    /// labels:
    /// - `router_id`
    pub retries_enabled: UpDownCounter<i64>,
    pub provider_api_keys: UpDownCounter<i64>,
    pub alephant_api_keys: UpDownCounter<i64>,
}

impl RouterMetrics {
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        let routers = meter
            .i64_up_down_counter("routers")
            .with_description("Number of routers")
            .build();
        let router_strategies = meter
            .i64_up_down_counter("router_strategies")
            .with_description("Number of router strategies")
            .build();
        let model_mappings = meter
            .i64_up_down_counter("model_mappings")
            .with_description("Number of model mappings")
            .build();
        let retries_enabled = meter
            .i64_up_down_counter("retries_enabled")
            .with_description("Number of routers with retries enabled")
            .build();
        let provider_api_keys = meter
            .i64_up_down_counter("provider_api_keys")
            .with_description("Number of provider API keys")
            .build();
        let alephant_api_keys = meter
            .i64_up_down_counter("alephant_api_keys")
            .with_description("Number of alephant API keys")
            .build();
        Self {
            routers,
            router_strategies,
            model_mappings,
            retries_enabled,
            provider_api_keys,
            alephant_api_keys,
        }
    }
}
