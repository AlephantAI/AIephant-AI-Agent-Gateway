use std::{
    convert::Infallible,
    future::{Ready, ready},
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use tower::{Service as _, ServiceBuilder, buffer::BufferLayer, util::BoxCloneService};
use tower_http::auth::AsyncRequireAuthorizationLayer;

use crate::{
    agent::tools::service::AgentToolsService,
    app_state::AppState,
    error::{api::ApiError, init::InitError, invalid_req::InvalidRequestError},
    middleware::{
        debug_log::DebugLogLayer, model_support::ModelSupportLayer,
        routing_precheck::RoutingPrecheckLayer,
    },
    router::{
        router_details::{RouteType, RouterDetailsLayer},
        unified_api,
    },
    utils::handle_error::{ErrorHandler, ErrorHandlerLayer},
};

pub(crate) const MIDDLEWARE_BUFFER_SIZE: usize = 256;

pub type UnifiedApiService = ErrorHandler<unified_api::Service>;
pub type AgentEventsService = crate::agent::service::AgentEventsService;

#[derive(Debug)]
pub struct MetaRouter {
    agent_events: AgentEventsService,
    agent_tools: AgentToolsService,
    unified_api: UnifiedApiService,
    x402_agents: crate::x402::service::X402AgentService,
}

pub type MetaRouterService =
    BoxCloneService<crate::types::request::Request, crate::types::response::Response, Infallible>;

impl MetaRouter {
    pub async fn build(app_state: AppState) -> Result<MetaRouterService, InitError> {
        let meta_router = Self::cloud(app_state.clone()).await?;

        let service_stack = ServiceBuilder::new()
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(RouterDetailsLayer::new())
            .layer(DebugLogLayer::new())
            .layer(RoutingPrecheckLayer::new())
            .layer(AsyncRequireAuthorizationLayer::new(
                crate::middleware::auth::AuthService::new(app_state.clone()),
            ))
            .layer(ModelSupportLayer {
                app_state: app_state.clone(),
            })
            .layer(
                crate::middleware::fallback_request_log::FallbackRequestLogLayer::new(&app_state),
            )
            .layer(
                crate::middleware::workspace_concurrency::WorkspaceConcurrencyLayer::new(
                    &app_state,
                ),
            )
            .map_err(|e: std::convert::Infallible| match e {})
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .map_err(crate::error::internal::InternalError::BufferError)
            .layer(BufferLayer::new(MIDDLEWARE_BUFFER_SIZE))
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .service(meta_router);
        Ok(BoxCloneService::new(service_stack))
    }

    async fn cloud(app_state: AppState) -> Result<Self, InitError> {
        if !app_state.config().compat_mode {
            crate::discover::router::provider_db::bootstrap_provider_catalog(&app_state).await?;
        }

        let unified_api = ServiceBuilder::new()
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .service(unified_api::Service::new(&app_state).await?);
        let agent_events = AgentEventsService::new(app_state.clone());
        let agent_tools = AgentToolsService::new(app_state.clone());
        let x402_agents = crate::x402::service::X402AgentService::new(app_state);

        Ok(Self {
            agent_events,
            agent_tools,
            unified_api,
            x402_agents,
        })
    }

    fn handle_agent_events_request(
        &mut self,
        req: crate::types::request::Request,
    ) -> ResponseFuture {
        tracing::trace!("received agent events request");
        ResponseFuture::AgentEvents {
            future: self.agent_events.call(req),
        }
    }

    fn handle_unified_api_request(
        &mut self,
        req: crate::types::request::Request,
        rest: &str,
    ) -> ResponseFuture {
        tracing::trace!(api_path = rest, "received unified API request");
        // assumes request is from OpenAI compatible client
        // and uses the model name to determine the provider.
        ResponseFuture::UnifiedApi {
            future: self.unified_api.call(req),
        }
    }

    fn handle_agent_tools_request(
        &mut self,
        req: crate::types::request::Request,
    ) -> ResponseFuture {
        tracing::trace!("received agent tools request");
        ResponseFuture::AgentTools {
            future: self.agent_tools.call(req),
        }
    }

    fn handle_x402_agent_request(&mut self, req: crate::types::request::Request) -> ResponseFuture {
        tracing::trace!("received x402 agent request");
        ResponseFuture::X402Agent {
            future: self.x402_agents.call(req),
        }
    }
}

impl tower::Service<crate::types::request::Request> for MetaRouter {
    type Response = crate::types::response::Response;
    type Error = ApiError;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let agent_events_pending = self.agent_events.poll_ready(ctx).is_pending();
        let agent_tools_pending = self.agent_tools.poll_ready(ctx).is_pending();
        if agent_tools_pending {
            tracing::warn!(
                "agent tools service is not ready; continuing meta router \
                 readiness"
            );
        }
        let unified_api_pending = self.unified_api.poll_ready(ctx).is_pending();
        let x402_agents_pending = self.x402_agents.poll_ready(ctx).is_pending();

        combined_readiness_ignoring_agent_tools(
            agent_events_pending,
            agent_tools_pending,
            unified_api_pending,
            x402_agents_pending,
        )
    }

    fn call(&mut self, req: crate::types::request::Request) -> Self::Future {
        let route_type = req.extensions().get::<RouteType>().cloned();
        match route_type {
            Some(RouteType::AgentEvents) => self.handle_agent_events_request(req),
            Some(RouteType::AgentTools { .. }) => self.handle_agent_tools_request(req),
            Some(RouteType::UnifiedApi { path }) => self.handle_unified_api_request(req, &path),
            Some(RouteType::X402Agent {
                slug,
                remaining_path,
            }) => {
                tracing::debug!(
                    slug = %slug,
                    remaining_path = %remaining_path,
                    "routing x402 request"
                );
                self.handle_x402_agent_request(req)
            }
            None => {
                tracing::debug!("no route type found");
                ResponseFuture::Ready {
                    future: ready(Err(ApiError::InvalidRequest(
                        InvalidRequestError::NotFound(req.uri().path().to_string()),
                    ))),
                }
            }
        }
    }
}

fn combined_readiness_ignoring_agent_tools(
    agent_events_pending: bool,
    _agent_tools_pending: bool,
    unified_api_pending: bool,
    x402_agents_pending: bool,
) -> Poll<Result<(), ApiError>> {
    if agent_events_pending || unified_api_pending || x402_agents_pending {
        Poll::Pending
    } else {
        Poll::Ready(Ok(()))
    }
}

pin_project! {
    #[project = ResponseFutureProj]
    pub enum ResponseFuture {
        Ready {
            #[pin]
            future: Ready<Result<crate::types::response::Response, ApiError>>,
        },
        UnifiedApi {
            #[pin]
            future: <UnifiedApiService as tower::Service<crate::types::request::Request>>::Future,
        },
        AgentEvents {
            #[pin]
            future: <AgentEventsService as tower::Service<crate::types::request::Request>>::Future,
        },
        AgentTools {
            #[pin]
            future: <AgentToolsService as tower::Service<crate::types::request::Request>>::Future,
        },
        X402Agent {
            #[pin]
            future: <crate::x402::service::X402AgentService as tower::Service<crate::types::request::Request>>::Future,
        },
    }
}

impl std::future::Future for ResponseFuture {
    type Output = Result<crate::types::response::Response, ApiError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            ResponseFutureProj::Ready { future } => future.poll(cx),
            ResponseFutureProj::UnifiedApi { future } => future.poll(cx).map_err(|e| match e {}),
            ResponseFutureProj::AgentEvents { future } => future.poll(cx).map_err(|e| match e {}),
            ResponseFutureProj::AgentTools { future } => future.poll(cx).map_err(|e| match e {}),
            ResponseFutureProj::X402Agent { future } => future.poll(cx).map_err(|e| match e {}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_tools_pending_does_not_make_meta_router_pending() {
        assert!(combined_readiness_ignoring_agent_tools(false, true, false, false).is_ready());
    }

    #[test]
    fn non_agent_tools_pending_still_make_meta_router_pending() {
        assert!(combined_readiness_ignoring_agent_tools(true, false, false, false).is_pending());
        assert!(combined_readiness_ignoring_agent_tools(false, false, true, false).is_pending());
        assert!(combined_readiness_ignoring_agent_tools(false, false, false, true).is_pending());
    }
}
