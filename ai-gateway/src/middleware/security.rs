//! Security plugin middleware layer.
//!
//! This module provides Tower middleware integration for the security plugin system.
//! Plugins are executed as part of the request/response middleware chain.
//!
//! # Streaming Considerations
//!
//! This middleware requires buffering request/response bodies for security checking.
//! Bodies larger than MAX_SECURITY_BODY_SIZE are truncated to prevent memory exhaustion.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::BodyExt;
use tower::{Layer, Service};

use crate::plugin::{PluginLoader, ResponseData, SecurityContext};
use crate::types::body::Body;

/// Maximum body size to collect for security checking.
/// Large bodies (images, embeddings, long outputs) are truncated.
/// This prevents memory exhaustion while still allowing security checks.
const MAX_SECURITY_BODY_SIZE: usize = 10 * 1024 * 1024; // 10MB

/// Security middleware layer.
///
/// This layer integrates the plugin system into Tower's middleware stack,
/// executing security plugins before and after request processing.
#[derive(Debug, Clone)]
pub struct SecurityLayer {
    loader: Arc<PluginLoader>,
}

impl SecurityLayer {
    /// Create a new security layer from a plugin loader.
    #[must_use]
    pub fn new(loader: PluginLoader) -> Self {
        Self {
            loader: Arc::new(loader),
        }
    }

    /// Create a disabled security layer (passes all requests through).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            loader: Arc::new(PluginLoader::default()),
        }
    }
}

impl<S> Layer<S> for SecurityLayer {
    type Service = SecurityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SecurityService {
            inner,
            loader: self.loader.clone(),
        }
    }
}

/// Security middleware service.
#[derive(Debug, Clone)]
pub struct SecurityService<S> {
    inner: S,
    loader: Arc<PluginLoader>,
}

impl<S> SecurityService<S> {
    /// Build security context from request.
    ///
    /// Note: Provider is extracted from request extensions if available,
    /// otherwise from URL path. The URL path provider extraction is unreliable
    /// for routes like `/v1/chat/completions` which would return "v1" instead
    /// of the actual provider.
    fn build_context(req: &Request<Body>, request_body: Bytes) -> SecurityContext {
        // Try to get provider from request extensions first
        let provider = req
            .extensions()
            .get::<String>()
            .cloned()
            .or_else(|| {
                // Fallback to URL path parsing (unreliable for multi-segment paths)
                req.uri()
                    .path()
                    .split('/')
                    .find(|segment| !segment.is_empty())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        // Virtual key should come from auth context in extensions, not client header.
        // If not available, use unknown - do NOT trust x-virtual-key header directly.
        let virtual_key_id = req
            .extensions()
            .get::<String>()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());

        SecurityContext {
            virtual_key_id,
            provider,
            request_body,
            workspace_id: None,
        }
    }

    /// Collect body with size limit to prevent memory exhaustion.
    async fn collect_limited_body(body: Body) -> Result<Bytes, String> {
        let body = body
            .collect()
            .await
            .map_err(|e| format!("body collection failed: {e}"))?;
        let bytes = body.to_bytes();
        if bytes.len() > MAX_SECURITY_BODY_SIZE {
            Ok(bytes.slice(..MAX_SECURITY_BODY_SIZE))
        } else {
            Ok(bytes)
        }
    }
}

impl<S> Service<Request<Body>> for SecurityService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let loader = self.loader.clone();
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let (parts, body) = req.into_parts();

        Box::pin(async move {
            // Collect request body with size limit
            let body_bytes = match Self::collect_limited_body(body).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    let response = Response::builder()
                        .status(400)
                        .body(Body::from(format!(
                            "security request body read failed: {e}"
                        )))
                        .expect("security error response should build");
                    return Ok(response);
                }
            };

            let req = Request::from_parts(parts, Body::from(body_bytes.clone()));
            let ctx = Self::build_context(&req, body_bytes);

            // Pre-check: validate request against plugins
            if let Err(e) = loader.check_request(&ctx) {
                let response = Response::builder()
                    .status(403)
                    .body(Body::from(format!("security check failed: {e}")))
                    .expect("security error response should build");
                return Ok(response);
            }

            let res = inner.call(req).await?;
            let (parts, body) = res.into_parts();

            // Collect response body with size limit
            let response_bytes = match Self::collect_limited_body(body).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    let response = Response::builder()
                        .status(500)
                        .body(Body::from(format!(
                            "security response body read failed: {e}"
                        )))
                        .expect("security error response should build");
                    return Ok(response);
                }
            };

            let mut data = ResponseData {
                body: response_bytes,
                sensitive: false,
            };
            if let Err(e) = loader.mask_response(&mut data) {
                let response = Response::builder()
                    .status(500)
                    .body(Body::from(format!("security response masking failed: {e}")))
                    .expect("security error response should build");
                return Ok(response);
            }

            let mut parts = parts;
            parts.headers.remove(http::header::CONTENT_LENGTH);
            Ok(Response::from_parts(parts, Body::from(data.body)))
        })
    }
}

/// Trait for extending services with security plugins.
pub trait SecurityExt {
    /// Wrap this service with the security plugin layer.
    fn with_security(self, loader: PluginLoader) -> SecurityService<Self>
    where
        Self: Sized;
}

impl<S: Clone> SecurityExt for S {
    fn with_security(self, loader: PluginLoader) -> SecurityService<Self>
    where
        Self: Sized,
    {
        SecurityService {
            inner: self,
            loader: Arc::new(loader),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use http_body_util::BodyExt;
    use tower::{ServiceExt, service_fn};

    use super::*;
    use crate::plugin::loader::{PluginConfig, SecurityPluginsConfig};

    fn sensitive_data_loader() -> PluginLoader {
        PluginLoader::from_config(&SecurityPluginsConfig {
            plugins: vec![PluginConfig {
                name: "sensitive_data_detector".to_string(),
                enabled: true,
                priority: Some(10),
                config: None,
            }],
        })
        .expect("loader should build")
    }

    #[tokio::test]
    async fn security_layer_reads_request_body_and_blocks_sensitive_payload() {
        let inner = service_fn(|_req: Request<Body>| async move {
            Ok::<_, Infallible>(Response::new(Body::from(r#"{"ok":true}"#)))
        });
        let mut service = SecurityLayer::new(sensitive_data_loader()).layer(inner);

        let req = Request::builder()
            .uri("/anthropic/v1/messages")
            .body(Body::from(r#"{"api_key":"secret"}"#))
            .expect("request should build");

        let res = service
            .ready()
            .await
            .expect("ready")
            .call(req)
            .await
            .unwrap();

        assert_eq!(res.status(), http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn security_layer_masks_response_body() {
        let inner = service_fn(|_req: Request<Body>| async move {
            Ok::<_, Infallible>(Response::new(Body::from(
                r#"{"email":"person@example.com","message":"ok"}"#,
            )))
        });
        let mut service = SecurityLayer::new(sensitive_data_loader()).layer(inner);

        let req = Request::builder()
            .uri("/anthropic/v1/messages")
            .body(Body::from(r#"{"message":"ok"}"#))
            .expect("request should build");

        let res = service
            .ready()
            .await
            .expect("ready")
            .call(req)
            .await
            .unwrap();
        let body_bytes = res
            .into_body()
            .collect()
            .await
            .expect("body should collect")
            .to_bytes();
        let body = String::from_utf8(body_bytes.to_vec()).expect("utf8 body");

        assert!(body.contains(r#""email":"***MASKED***""#));
        assert!(body.contains(r#""message":"ok""#));
    }
}
