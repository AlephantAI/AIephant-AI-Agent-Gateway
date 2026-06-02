use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::header::CONTENT_TYPE;
use http_body_util::BodyExt as _;
use tower::{Layer, Service};

use crate::{
    error::{api::ApiError, internal::InternalError},
    types::{body::Body, request::Request, response::Response},
    utils::debug_log::{self, DebugLogConfig},
};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone, Default)]
pub struct DebugLogLayer;

impl DebugLogLayer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for DebugLogLayer {
    type Service = DebugLogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        DebugLogService { inner }
    }
}

#[derive(Debug, Clone)]
pub struct DebugLogService<S> {
    inner: S,
}

impl<S> Service<Request> for DebugLogService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<ApiError> + Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        Box::pin(async move {
            let req = prepare_request_for_debug_logging(req).await?;
            let debug_log_config = req
                .extensions()
                .get::<DebugLogConfig>()
                .copied()
                .unwrap_or_else(DebugLogConfig::from_env);

            let response = inner.call(req).await.map_err(Into::into)?;
            log_response_for_debug(response, debug_log_config).await
        })
    }
}

async fn prepare_request_for_debug_logging(
    req: Request,
) -> Result<Request, ApiError> {
    let (mut parts, body) = req.into_parts();
    let debug_log_config = DebugLogConfig::from_headers(&mut parts.headers);
    parts.extensions.insert(debug_log_config);

    debug_log::maybe_log_headers("v1", &parts.headers, debug_log_config);

    if !debug_log_config.body {
        return Ok(Request::from_parts(parts, body));
    }

    let body = body
        .collect()
        .await
        .map_err(|e| ApiError::from(InternalError::CollectBodyError(e)))?
        .to_bytes();
    debug_log::maybe_log_body("v1", &body, debug_log_config);

    Ok(Request::from_parts(parts, Body::from(body)))
}

async fn log_response_for_debug(
    response: Response,
    debug_log_config: DebugLogConfig,
) -> Result<Response, ApiError> {
    if !debug_log_config.headers && !debug_log_config.body {
        return Ok(response);
    }

    let (parts, body) = response.into_parts();

    if debug_log_config.headers {
        let joined = debug_log::debug_header_lines(&parts.headers);
        tracing::info!(
            %joined,
            "v1: response headers (debug headers enabled)"
        );
    }

    if !debug_log_config.body {
        return Ok(Response::from_parts(parts, body));
    }

    if !should_collect_response_body_for_debug(&parts.headers) {
        tracing::info!(
            "v1: response body skipped for streaming response (debug body \
             enabled)"
        );
        return Ok(Response::from_parts(parts, body));
    }

    let body = body
        .collect()
        .await
        .map_err(|e| ApiError::from(InternalError::CollectBodyError(e)))?
        .to_bytes();
    let preview = debug_log::debug_body_preview(&body);
    tracing::info!(
        body_len = preview.body_len,
        truncated = preview.truncated,
        body = %preview.body,
        "v1: response body (debug body enabled)"
    );

    Ok(Response::from_parts(parts, Body::from(body)))
}

pub(crate) fn should_collect_response_body_for_debug(
    headers: &http::HeaderMap,
) -> bool {
    let Some(content_type) = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };

    !content_type
        .split(';')
        .next()
        .is_some_and(|ty| ty.trim().eq_ignore_ascii_case("text/event-stream"))
}

#[cfg(test)]
mod tests {
    use http::{HeaderValue, Request, Response, header::CONTENT_TYPE};
    use http_body_util::BodyExt as _;
    use tower::{Service, ServiceBuilder, service_fn};

    use crate::{
        error::api::ApiError, types::body::Body,
        utils::debug_log::DebugLogConfig,
    };

    #[tokio::test]
    async fn debug_layer_removes_control_headers_and_replays_request_body() {
        let mut service = ServiceBuilder::new()
            .layer(super::DebugLogLayer::new())
            .service(service_fn(
                |req: crate::types::request::Request| async move {
                    assert_eq!(
                        req.extensions().get::<DebugLogConfig>().copied(),
                        Some(DebugLogConfig {
                            headers: true,
                            body: true,
                        })
                    );
                    assert!(
                        !req.headers().contains_key("alephant-debug-headers")
                    );
                    assert!(!req.headers().contains_key("alephant-debug-body"));

                    let body = req
                        .into_body()
                        .collect()
                        .await
                        .expect("request body should collect")
                        .to_bytes();
                    assert_eq!(body.as_ref(), br#"{"prompt":"hello"}"#);

                    Ok::<_, ApiError>(Response::new(Body::from("ok")))
                },
            ));

        let req = Request::builder()
            .uri("/v1/agent/events")
            .header("alephant-debug-headers", "true")
            .header("alephant-debug-body", "true")
            .body(Body::from(r#"{"prompt":"hello"}"#))
            .expect("request should build");

        let response = service.call(req).await.expect("service should respond");
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes();
        assert_eq!(body.as_ref(), b"ok");
    }

    #[tokio::test]
    async fn debug_layer_replays_non_streaming_response_body() {
        let mut service = ServiceBuilder::new()
            .layer(super::DebugLogLayer::new())
            .service(service_fn(|_req| async move {
                Ok::<_, ApiError>(
                    Response::builder()
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(r#"{"answer":"done"}"#))
                        .expect("response should build"),
                )
            }));

        let req = Request::builder()
            .uri("/v1/chat/completions")
            .header("alephant-debug-body", "true")
            .body(Body::from("{}"))
            .expect("request should build");

        let response = service.call(req).await.expect("service should respond");
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body should collect")
            .to_bytes();

        assert_eq!(body.as_ref(), br#"{"answer":"done"}"#);
    }

    #[test]
    fn event_stream_response_body_is_not_collectable_for_debug_logging() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );

        assert!(!super::should_collect_response_body_for_debug(&headers));
    }
}
