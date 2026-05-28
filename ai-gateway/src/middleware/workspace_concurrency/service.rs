use std::sync::Arc;

use futures::future::BoxFuture;
use opentelemetry::KeyValue;

use super::{
    constants::{placeholder_workspace, redis_key},
    redis_ops::{decr_floor_refresh_ttl, incr_refresh_ttl, log_redis_err},
};
use crate::{
    app_state::AppState,
    error::api::ApiError,
    middleware::counted_body::CountedBody,
    types::{extensions::AuthContext, request::Request, response::Response},
};

#[derive(Clone)]
pub struct WorkspaceConcurrencyService<S> {
    inner: S,
    redis: Option<Arc<crate::app_redis::AppRedis>>,
    metrics: crate::metrics::Metrics,
}

impl<S> WorkspaceConcurrencyService<S> {
    pub fn new(inner: S, app_state: &AppState) -> Self {
        Self {
            inner,
            redis: app_state.redis().cloned(),
            metrics: app_state.0.metrics.clone(),
        }
    }
}

impl<S> tower::Service<Request> for WorkspaceConcurrencyService<S>
where
    S: tower::Service<Request, Response = Response, Error = ApiError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<'static, Result<Response, ApiError>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let redis = self.redis.clone();
        let metrics = self.metrics.clone();
        // `poll_ready` runs on `self.inner`; `tower::buffer::Buffer` requires
        // the same handle for `call` (see mapper service /
        // LatencyRouter). Swap like tower's clone guidance.
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        Box::pin(async move {
            match redis {
                None => inner.call(req).await,
                Some(client) => {
                    let workspace = req
                        .extensions()
                        .get::<AuthContext>()
                        .map_or_else(placeholder_workspace, |a| *a.org_id.as_ref());
                    if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
                        tracing::info!(
                            "[workspace_concurrency] before redis incr, \
                             workspace={workspace}"
                        );
                    }
                    let key = redis_key(workspace);
                    let incr_res = incr_refresh_ttl(&client, &key).await;
                    let incr_ok = incr_res.is_ok();
                    if incr_res.is_ok() {
                        metrics
                            .workspace_concurrency_redis_incr
                            .add(1, &[KeyValue::new("result", "ok")]);
                    } else if let Err(ref e) = incr_res {
                        log_redis_err("incr", workspace, e);
                        metrics
                            .workspace_concurrency_redis_incr
                            .add(1, &[KeyValue::new("result", "err")]);
                    }

                    if std::env::var_os("AI_GATEWAY_DEBUG_UNIFIED").is_some() {
                        tracing::info!(
                            "[workspace_concurrency] after redis incr, \
                             calling inner (unified/mapper/…)"
                        );
                    }
                    let http_response = inner.call(req).await?;
                    if !incr_ok {
                        return Ok(http_response);
                    }
                    let (parts, body) = http_response.into_parts();
                    let redis_for_cb = client.clone();
                    let key_for_cb = key.clone();
                    let metrics_for_cb = metrics.clone();
                    let ws_for_cb = workspace;
                    let on_release: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                        let client = redis_for_cb.clone();
                        let key = key_for_cb.clone();
                        let metrics = metrics_for_cb.clone();
                        let ws = ws_for_cb;
                        tokio::spawn(async move {
                            match decr_floor_refresh_ttl(&client, &key).await {
                                Ok(()) => {
                                    metrics
                                        .workspace_concurrency_redis_decr
                                        .add(1, &[KeyValue::new("result", "ok")]);
                                }
                                Err(ref e) => {
                                    log_redis_err("decr", ws, e);
                                    metrics
                                        .workspace_concurrency_redis_decr
                                        .add(1, &[KeyValue::new("result", "err")]);
                                }
                            }
                        });
                    });
                    let wrapped = axum_core::body::Body::new(CountedBody::new(body, on_release));
                    Ok(http::Response::from_parts(parts, wrapped))
                }
            }
        })
    }
}
