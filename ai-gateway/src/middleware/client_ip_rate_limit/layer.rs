//! Global Tower layer: per-client-IP sliding 1s rate limit.

use std::{
    convert::Infallible,
    net::SocketAddr,
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

use super::{memory::MemoryLimiter, redis_script, resolve_ip};
use crate::{
    app_state::AppState,
    config::client_ip_rate_limit::{
        ClientIpRateLimitBackend, ClientIpRateLimitConfig,
    },
    error::invalid_req::{InvalidRequestError, TooManyRequestsError},
    types::{request::Request, response::Response},
};

static SLIDING_SCRIPT: OnceLock<redis::Script> = OnceLock::new();

fn sliding_script() -> &'static redis::Script {
    SLIDING_SCRIPT.get_or_init(redis_script::sliding_window_script)
}

/// Emit at most one `error!` every 5 seconds to avoid log spam during Redis
/// flapping.
fn log_redis_degraded_throttled(summary: &str) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let now = Instant::now();
    let mut g = LAST.lock().expect("degraded log mutex poisoned");
    let should_log =
        g.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(5));
    if should_log {
        *g = Some(now);
        tracing::error!(
            target: "ai_gateway::client_ip_rate_limit",
            "{summary}"
        );
    }
}

#[derive(Clone)]
struct State {
    cfg: ClientIpRateLimitConfig,
    trusted: Vec<ipnetwork::IpNetwork>,
    memory: Arc<MemoryLimiter>,
    redis: Option<Arc<crate::app_redis::AppRedis>>,
    /// `true` when Redis is reachable and active for rate limiting.
    /// Flipped to `false` on Redis error; background task restores it.
    use_redis: Arc<AtomicBool>,
    metrics: crate::metrics::Metrics,
}

/// Warn only once when `SocketAddr` extension is missing.
fn warn_missing_socket_once() {
    static DONE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        tracing::warn!(
            target: "ai_gateway::client_ip_rate_limit",
            "client-ip-rate-limit: missing SocketAddr extension; skipping limit"
        );
    }
}

#[derive(Clone)]
pub struct ClientIpRateLimitLayer {
    state: Arc<State>,
}

impl ClientIpRateLimitLayer {
    /// Build layer: parse trusted CIDRs; if `backend=redis`, use completed
    /// startup ping result.
    pub async fn new(
        app_state: &AppState,
    ) -> Result<Self, crate::error::init::InitError> {
        let cfg = app_state
            .config()
            .global
            .client_ip_rate_limit
            .clone()
            .unwrap_or_default();
        let trusted = if cfg.enabled {
            resolve_ip::parse_trusted_proxy_networks(&cfg.trusted_proxy_cidrs)?
        } else {
            Vec::new()
        };
        let memory =
            Arc::new(MemoryLimiter::new(cfg.requests_per_second.max(1)));
        let use_redis = Arc::new(AtomicBool::new(false));
        if cfg.enabled && matches!(cfg.backend, ClientIpRateLimitBackend::Redis)
        {
            match app_state.redis() {
                None => {
                    tracing::error!(
                        target: "ai_gateway::client_ip_rate_limit",
                        "client-ip-rate-limit: backend=redis but AppRedis is not \
                         configured; using memory backend"
                    );
                }
                Some(redis) => match redis.ping().await {
                    Ok(()) => {
                        use_redis.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "ai_gateway::client_ip_rate_limit",
                            error = %e,
                            "client-ip-rate-limit: Redis ping failed at startup; \
                             using memory backend"
                        );
                    }
                },
            }
        }

        let state = Arc::new(State {
            cfg,
            trusted,
            memory,
            redis: app_state.redis().cloned(),
            use_redis: use_redis.clone(),
            metrics: app_state.0.metrics.clone(),
        });

        if let Some(redis) = app_state.redis().cloned() {
            let use_redis = state.use_redis.clone();
            let metrics = state.metrics.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs(30));
                interval.set_missed_tick_behavior(
                    tokio::time::MissedTickBehavior::Skip,
                );
                loop {
                    interval.tick().await;
                    if use_redis.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Ok(()) = redis.ping().await {
                        use_redis.store(true, Ordering::Relaxed);
                        metrics.rate_limit_redis_degraded_gauge.record(
                            0,
                            &[opentelemetry::KeyValue::new(
                                "layer",
                                "client_ip",
                            )],
                        );
                        tracing::info!(
                            target: "ai_gateway::client_ip_rate_limit",
                            "Redis recovered, switching back from in-memory \
                             fallback"
                        );
                    }
                }
            });
        }

        Ok(Self { state })
    }
}

impl<S> Layer<S> for ClientIpRateLimitLayer {
    type Service = ClientIpRateLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ClientIpRateLimitService {
            inner,
            state: self.state.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ClientIpRateLimitService<S> {
    inner: S,
    state: Arc<State>,
}

impl<S> Service<Request> for ClientIpRateLimitService<S>
where
    S: Service<Request, Response = Response, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
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
            let Some(peer_sa) = req.extensions().get::<SocketAddr>().copied()
            else {
                warn_missing_socket_once();
                return inner.call(req).await;
            };
            let ip = resolve_ip::effective_client_ip(
                peer_sa.ip(),
                req.headers(),
                &state.trusted,
            );
            let limit_i64 = i64::from(state.cfg.requests_per_second);
            let allowed = if state.use_redis.load(Ordering::Relaxed) {
                if let Some(ref redis) = state.redis {
                    let key = format!("{}{}", state.cfg.redis_key_prefix, ip);
                    let member = format!(
                        "{}:{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_nanos()),
                        rand::random::<u64>()
                    );
                    match redis
                        .client_ip_rate_limit_allow(
                            sliding_script(),
                            &key,
                            1000,
                            limit_i64,
                            &member,
                        )
                        .await
                    {
                        Ok(true) => true,
                        Ok(false) => false,
                        Err(e) => {
                            state
                                .metrics
                                .client_ip_rate_limit_redis_degraded
                                .add(1, &[]);
                            state.use_redis.store(false, Ordering::Relaxed);
                            state
                                .metrics
                                .rate_limit_redis_degraded_gauge
                                .record(
                                    1,
                                    &[opentelemetry::KeyValue::new(
                                        "layer",
                                        "client_ip",
                                    )],
                                );
                            log_redis_degraded_throttled(&format!(
                                "client-ip-rate-limit: Redis EVAL failed: {e}"
                            ));
                            state.memory.check(ip).is_ok()
                        }
                    }
                } else {
                    state.memory.check(ip).is_ok()
                }
            } else {
                state.memory.check(ip).is_ok()
            };
            if !allowed {
                state.metrics.client_ip_rate_limit_rejected.add(1, &[]);
                let resp = InvalidRequestError::TooManyRequests(
                    TooManyRequestsError {
                        ratelimit_limit: u64::from(
                            state.cfg.requests_per_second,
                        ),
                        ratelimit_remaining: 0,
                        retry_after: 1,
                    },
                )
                .into_response();
                return Ok(resp);
            }
            state.metrics.client_ip_rate_limit_allowed.add(1, &[]);
            inner.call(req).await
        })
    }
}
