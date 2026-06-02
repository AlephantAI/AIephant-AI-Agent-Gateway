//! Global Tower layer: whole-gateway in-flight request cap.

use std::{
    convert::Infallible,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use axum_core::response::IntoResponse;
use futures::future::BoxFuture;
use tower::{Layer, Service};

use super::{memory::MemoryInFlightLimiter, redis_release, redis_script};
use crate::{
    app_state::AppState,
    config::gateway_in_flight_limit::{GatewayInFlightBackend, GatewayInFlightLimitConfig},
    error::{
        init::InitError,
        invalid_req::{InvalidRequestError, TooManyRequestsError},
    },
    middleware::counted_body::CountedBody,
    types::{request::Request, response::Response},
};

static ACQUIRE_SCRIPT: OnceLock<redis::Script> = OnceLock::new();

fn acquire_script() -> &'static redis::Script {
    ACQUIRE_SCRIPT.get_or_init(redis_script::acquire_in_flight_script)
}

fn log_redis_degraded_throttled(summary: &str) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let now = Instant::now();
    let mut g = LAST.lock().expect("degraded log mutex poisoned");
    let should_log = g.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(5));
    if should_log {
        *g = Some(now);
        tracing::error!(
            target: "ai_gateway::gateway_in_flight_limit",
            "{summary}"
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum HeldSlot {
    Memory,
    Redis,
}

#[derive(Clone)]
struct State {
    cfg: GatewayInFlightLimitConfig,
    redis_counter_key: String,
    memory: Arc<MemoryInFlightLimiter>,
    redis: Option<Arc<crate::app_redis::AppRedis>>,
    /// `true` when Redis is reachable and active for in-flight limiting.
    /// Flipped to `false` on Redis error; background task restores it.
    use_redis: Arc<AtomicBool>,
    metrics: crate::metrics::Metrics,
}

#[derive(Clone)]
pub struct GatewayInFlightLimitLayer {
    state: Arc<State>,
}

impl GatewayInFlightLimitLayer {
    pub async fn new(app_state: &AppState) -> Result<Self, InitError> {
        let cfg = app_state
            .config()
            .global
            .gateway_in_flight_limit
            .clone()
            .unwrap_or_default();
        let redis_counter_key = format!("{}inflight", cfg.redis_key_prefix);
        let memory = Arc::new(MemoryInFlightLimiter::new(cfg.max_concurrent.max(1)));
        let use_redis = Arc::new(AtomicBool::new(false));
        if cfg.enabled && matches!(cfg.backend, GatewayInFlightBackend::Redis) {
            match app_state.redis() {
                None => {
                    tracing::error!(
                        target: "ai_gateway::gateway_in_flight_limit",
                        "gateway-in-flight-limit: backend=redis but AppRedis is \
                         not configured; using memory backend"
                    );
                }
                Some(redis) => match redis.ping().await {
                    Ok(()) => {
                        use_redis.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "ai_gateway::gateway_in_flight_limit",
                            error = %e,
                            "gateway-in-flight-limit: Redis ping failed at \
                             startup; using memory backend"
                        );
                    }
                },
            }
        }
        let state = Arc::new(State {
            cfg,
            redis_counter_key,
            memory,
            redis: app_state.redis().cloned(),
            use_redis,
            metrics: app_state.0.metrics.clone(),
        });

        if let Some(redis) = app_state.redis().cloned() {
            let use_redis = state.use_redis.clone();
            let metrics = state.metrics.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    if use_redis.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Ok(()) = redis.ping().await {
                        use_redis.store(true, Ordering::Relaxed);
                        metrics
                            .rate_limit_redis_degraded_gauge
                            .record(0, &[opentelemetry::KeyValue::new("layer", "in_flight")]);
                        tracing::info!(
                            target: "ai_gateway::gateway_in_flight_limit",
                            "Redis recovered, switching back from in-memory \
                             fallback"
                        );
                    }
                }
            });
        }

        Ok(Self { state })
    }

    async fn try_acquire(state: &State) -> Result<HeldSlot, ()> {
        let max = state.cfg.max_concurrent.max(1);
        let max_i64 = i64::from(max);
        let key = state.redis_counter_key.as_str();

        if state.use_redis.load(Ordering::Relaxed)
            && let Some(ref redis) = state.redis
        {
            match redis
                .gateway_in_flight_try_acquire(
                    acquire_script(),
                    key,
                    redis_release::TTL_SECS,
                    max_i64,
                )
                .await
            {
                Ok(true) => return Ok(HeldSlot::Redis),
                Ok(false) => return Err(()),
                Err(e) => {
                    state.metrics.gateway_in_flight_redis_degraded.add(1, &[]);
                    log_redis_degraded_throttled(&format!(
                        "gateway-in-flight-limit: Redis EVAL failed: {e}"
                    ));
                    state.use_redis.store(false, Ordering::Relaxed);
                    state
                        .metrics
                        .rate_limit_redis_degraded_gauge
                        .record(1, &[opentelemetry::KeyValue::new("layer", "in_flight")]);
                }
            }
        }
        state.memory.try_acquire().map(|()| HeldSlot::Memory)
    }

    fn too_many(max: u32) -> Response {
        InvalidRequestError::TooManyRequests(TooManyRequestsError {
            ratelimit_limit: u64::from(max),
            ratelimit_remaining: 0,
            retry_after: 1,
        })
        .into_response()
    }
}

impl<S> Layer<S> for GatewayInFlightLimitLayer {
    type Service = GatewayInFlightLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        GatewayInFlightLimitService {
            inner,
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct GatewayInFlightLimitService<S> {
    inner: S,
    state: Arc<State>,
}

impl<S> Service<Request> for GatewayInFlightLimitService<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let state = self.state.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        Box::pin(async move {
            if !state.cfg.enabled {
                return inner.call(req).await;
            }
            let max = state.cfg.max_concurrent.max(1);
            let key = state.redis_counter_key.clone();

            let slot = if let Ok(s) = GatewayInFlightLimitLayer::try_acquire(&state).await {
                s
            } else {
                state.metrics.gateway_in_flight_rejected.add(1, &[]);
                return Ok(GatewayInFlightLimitLayer::too_many(max));
            };

            state.metrics.gateway_in_flight_allowed.add(1, &[]);
            let resp = inner.call(req).await?;
            let (parts, body) = resp.into_parts();

            let on_release: Arc<dyn Fn() + Send + Sync> = match slot {
                HeldSlot::Memory => {
                    let memory = state.memory.clone();
                    Arc::new(move || memory.release())
                }
                HeldSlot::Redis => {
                    let redis_opt = state.redis.clone();
                    let key = key.clone();
                    Arc::new(move || {
                        let Some(redis) = redis_opt.clone() else {
                            tracing::error!(
                                target: "ai_gateway::gateway_in_flight_limit",
                                "gateway-in-flight-limit: release skipped, AppRedis \
                                 missing after Redis acquire (invariant broken)"
                            );
                            return;
                        };
                        let key_for_task = key.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                redis_release::decr_floor_refresh_ttl(&redis, &key_for_task).await
                            {
                                tracing::warn!(
                                    target:
                                        "ai_gateway::gateway_in_flight_limit",
                                    error = %e,
                                    "gateway-in-flight-limit: Redis DECR \
                                     failed",
                                );
                            }
                        });
                    })
                }
            };
            let wrapped = axum_core::body::Body::new(CountedBody::new(body, on_release));
            Ok(http::Response::from_parts(parts, wrapped))
        })
    }
}
