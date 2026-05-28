use std::{
    future::{Ready, ready},
    task::{Context, Poll},
};

use axum_core::response::Response;
use futures::future::Either;
use http::{Method, Request, StatusCode};
use tower::{Layer, Service};

use crate::app_state::AppState;

#[derive(Debug, Clone)]
pub struct HealthCheckLayer {
    app_state: Option<AppState>,
}

impl HealthCheckLayer {
    #[must_use]
    pub const fn new() -> Self {
        Self { app_state: None }
    }

    #[must_use]
    pub fn with_app_state(app_state: AppState) -> Self {
        Self {
            app_state: Some(app_state),
        }
    }
}

impl Default for HealthCheckLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for HealthCheckLayer {
    type Service = HealthCheck<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HealthCheck {
            inner,
            app_state: self.app_state.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HealthCheck<S> {
    inner: S,
    app_state: Option<AppState>,
}

impl<S, ReqBody> Service<Request<ReqBody>> for HealthCheck<S>
where
    S: Service<Request<ReqBody>, Response = Response> + Send + 'static,
    S::Error: Send,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Either<Ready<Result<Self::Response, Self::Error>>, S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        if req.method() != Method::GET && req.method() != Method::HEAD {
            return Either::Right(self.inner.call(req));
        }

        match req.uri().path() {
            "/health" => Either::Left(ready(Ok(healthy_response()))),
            "/healthz/ready" => {
                let is_ready = self.app_state.as_ref().is_none_or(
                    super::super::app_state::AppState::is_cache_warmed,
                );
                if is_ready {
                    Either::Left(ready(Ok(healthy_response())))
                } else {
                    Either::Left(ready(Ok(not_ready_response())))
                }
            }
            _ => Either::Right(self.inner.call(req)),
        }
    }
}

fn healthy_response() -> Response {
    let body = axum_core::body::Body::empty();
    http::Response::builder()
        .status(StatusCode::OK)
        .body(body)
        .expect("always valid if tests pass")
}

fn not_ready_response() -> Response {
    let body = axum_core::body::Body::from("not ready");
    http::Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .body(body)
        .expect("always valid if tests pass")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_healthy_response() {
        let response = healthy_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn test_not_ready_response() {
        let response = not_ready_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
