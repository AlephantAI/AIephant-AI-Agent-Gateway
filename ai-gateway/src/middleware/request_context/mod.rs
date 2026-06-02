use std::{
    sync::Arc,
    task::{Context, Poll},
};

use crate::{
    agent::headers::parse_agent_context_from_headers,
    config::{agent::AgentConfig, router::RouterConfig},
    types::{
        extensions::{AuthContext, RequestContext},
        request::Request,
        response::Response,
    },
};

#[derive(Debug, Clone)]
pub struct Service<S> {
    inner: S,
    /// If `None`, this service is for a direct proxy.
    /// If `Some`, this service is for a load balanced router.
    router_config: Option<Arc<RouterConfig>>,
    agent_config: AgentConfig,
}

impl<S> Service<S> {
    pub fn new(
        inner: S,
        router_config: Option<Arc<RouterConfig>>,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            inner,
            router_config,
            agent_config,
        }
    }
}

impl<S> tower::Service<Request> for Service<S>
where
    S: tower::Service<Request, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    #[tracing::instrument(level = "debug", name = "request_context", skip_all)]
    fn call(&mut self, mut req: Request) -> Self::Future {
        let router_config = self.router_config.clone();
        let auth_context = req.extensions_mut().remove::<AuthContext>();
        let agent_context = if self.agent_config.enabled && self.agent_config.allow_header_context {
            parse_agent_context_from_headers(
                req.headers(),
                self.agent_config.max_header_value_bytes,
            )
        } else {
            None
        };
        let req_ctx = RequestContext {
            router_config,
            auth_context,
            llm_kv_cache_read_allowed: true,
            llm_kv_cache_write_allowed: true,
            agent_context,
        };
        req.extensions_mut().insert(Arc::new(req_ctx));
        self.inner.call(req)
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    router_config: Option<Arc<RouterConfig>>,
    agent_config: AgentConfig,
}

impl Layer {
    #[must_use]
    pub fn for_router(router_config: Arc<RouterConfig>, agent_config: AgentConfig) -> Self {
        Self {
            router_config: Some(router_config),
            agent_config,
        }
    }

    #[must_use]
    pub fn for_direct_proxy(agent_config: AgentConfig) -> Self {
        Self {
            router_config: None,
            agent_config,
        }
    }
}

impl<S> tower::Layer<S> for Layer {
    type Service = Service<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Service::new(inner, self.router_config.clone(), self.agent_config.clone())
    }
}

#[cfg(test)]
mod tests {
    use axum_core::body::Body;
    use tower::{Service as _, ServiceExt};

    use super::*;

    #[tokio::test]
    async fn request_context_attaches_agent_context_from_headers() {
        let service = tower::service_fn(|req: Request| async move {
            let ctx = req
                .extensions()
                .get::<Arc<RequestContext>>()
                .expect("request context should be attached");
            assert_eq!(
                ctx.agent_context
                    .as_ref()
                    .and_then(|c| c.agent_id_external.as_deref()),
                Some("coding-agent")
            );
            assert_eq!(
                ctx.agent_context.as_ref().and_then(|c| c.run_id.as_deref()),
                Some("run-1")
            );
            Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
        });
        let agent_config = AgentConfig {
            enabled: true,
            ..AgentConfig::default()
        };
        let mut service = Service::new(service, None, agent_config);
        let req = http::Request::builder()
            .uri("/v1/chat/completions")
            .header("Alephant-Agent-Id", "coding-agent")
            .header("Alephant-Run-Id", "run-1")
            .body(Body::empty())
            .unwrap();

        let _ = service.ready().await.unwrap().call(req).await.unwrap();
    }

    #[tokio::test]
    async fn request_context_ignores_agent_headers_when_disabled() {
        let service = tower::service_fn(|req: Request| async move {
            let ctx = req
                .extensions()
                .get::<Arc<RequestContext>>()
                .expect("request context should be attached");
            assert!(ctx.agent_context.is_none());
            Ok::<_, std::convert::Infallible>(Response::new(Body::empty()))
        });
        let mut service = Service::new(service, None, AgentConfig::default());
        let req = http::Request::builder()
            .uri("/v1/chat/completions")
            .header("Alephant-Agent-Id", "coding-agent")
            .header("Alephant-Run-Id", "run-1")
            .body(Body::empty())
            .unwrap();

        let _ = service.ready().await.unwrap().call(req).await.unwrap();
    }
}
