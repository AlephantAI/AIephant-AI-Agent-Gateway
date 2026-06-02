use std::{
    collections::HashSet,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum_core::response::IntoResponse;
use http::uri::PathAndQuery;
use pin_project_lite::pin_project;
use rustc_hash::FxHashMap as HashMap;
use tower::{ServiceBuilder, buffer, util::BoxCloneService};

use crate::{
    app_state::AppState,
    config::router::RouterConfig,
    endpoints::{ApiEndpoint, EndpointType},
    error::{
        api::ApiError, init::InitError, internal::InternalError, invalid_req::InvalidRequestError,
    },
    middleware::{prompts::PromptLayer, request_context},
    router::{meta::MIDDLEWARE_BUFFER_SIZE, strategy::RoutingStrategyService},
    types::router::RouterId,
    utils::handle_error::ErrorHandlerLayer,
};

type InnerRouterService =
    BoxCloneService<crate::types::request::Request, crate::types::response::Response, Infallible>;

/// The top-level service we use to compose both
/// middleware along with the routing strategy service.
#[derive(Debug)]
pub struct Router {
    inner: HashMap<EndpointType, InnerRouterService>,
    pub(crate) router_config: Arc<RouterConfig>,
    app_state: AppState,
}

impl Router {
    pub async fn new(
        id: RouterId,
        router_config: Arc<RouterConfig>,
        app_state: AppState,
    ) -> Result<Self, InitError> {
        router_config.validate()?;

        let mut inner = HashMap::default();
        let prompt_layer = PromptLayer::new(&app_state)?;
        let request_context_layer = request_context::Layer::for_router(
            router_config.clone(),
            app_state.config().agent.clone(),
        );
        for (endpoint_type, balance_config) in router_config.load_balance.as_ref() {
            let routing_strategy = RoutingStrategyService::new(
                app_state.clone(),
                id.clone(),
                router_config.clone(),
                balance_config,
            )
            .await?;
            let service_stack = ServiceBuilder::new()
                .layer(ErrorHandlerLayer::new(app_state.clone()))
                .layer(prompt_layer.clone())
                .map_err(|e: std::convert::Infallible| match e {})
                .layer(ErrorHandlerLayer::new(app_state.clone()))
                .map_err(|e| ApiError::from(InternalError::BufferError(e)))
                .layer(buffer::BufferLayer::new(MIDDLEWARE_BUFFER_SIZE))
                .layer(request_context_layer.clone())
                .service(routing_strategy);

            inner.insert(*endpoint_type, BoxCloneService::new(service_stack));
        }

        Ok(Self {
            inner,
            router_config,
            app_state,
        })
    }
}

impl tower::Service<crate::types::request::Request> for Router {
    type Response = crate::types::response::Response;
    type Error = Infallible;
    type Future = ResponseFuture;

    #[inline]
    fn poll_ready(&mut self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut any_pending = false;
        for balancer in self.inner.values_mut() {
            if balancer.poll_ready(ctx).is_pending() {
                any_pending = true;
            }
        }
        if any_pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    #[inline]
    #[tracing::instrument(level = "debug", name = "router", skip_all)]
    fn call(&mut self, mut req: crate::types::request::Request) -> Self::Future {
        let Some(extracted_path_and_query) = req.extensions().get::<PathAndQuery>() else {
            let api_error = ApiError::Internal(InternalError::ExtensionNotFound("PathAndQuery"));
            let response = api_error.into_response();
            return ResponseFuture::Ready {
                response: Some(response),
            };
        };

        let api_endpoint = ApiEndpoint::new(extracted_path_and_query.path());
        if let Some(api_endpoint) = api_endpoint {
            let endpoint_type = api_endpoint.endpoint_type();
            if let Some(err) = precheck_workspace_candidate_intersection(
                &self.app_state,
                &self.router_config,
                endpoint_type,
                req.extensions()
                    .get::<crate::types::extensions::AuthContext>(),
            ) {
                let response = ApiError::Internal(err).into_response();
                return ResponseFuture::Ready {
                    response: Some(response),
                };
            }
            if let Some(balancer) = self.inner.get_mut(&endpoint_type) {
                req.extensions_mut().insert(api_endpoint);
                ResponseFuture::Inner {
                    future: balancer.call(req),
                }
            } else {
                let api_error = ApiError::InvalidRequest(InvalidRequestError::NotFound(
                    extracted_path_and_query.path().to_string(),
                ));
                let response = api_error.into_response();
                ResponseFuture::Ready {
                    response: Some(response),
                }
            }
        } else {
            let api_error = ApiError::InvalidRequest(InvalidRequestError::NotFound(
                extracted_path_and_query.path().to_string(),
            ));
            let response = api_error.into_response();
            ResponseFuture::Ready {
                response: Some(response),
            }
        }
    }
}

pin_project! {
    #[project = ResponseFutureProj]
    pub enum ResponseFuture
    {
        Ready {
            response: Option<crate::types::response::Response>,
        },
        Inner {
            #[pin]
            future: <InnerRouterService as tower::Service<crate::types::request::Request>>::Future,
        },
    }
}

impl Future for ResponseFuture {
    type Output = Result<crate::types::response::Response, Infallible>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            ResponseFutureProj::Ready { response } => {
                Poll::Ready(Ok(response.take().expect("future polled after completion")))
            }
            ResponseFutureProj::Inner { future } => {
                match futures::ready!(future.poll(cx)) {
                    Ok(res) => Poll::Ready(Ok(res)),
                    // never happens due to `Infallible` bound
                    Err(e) => match e {},
                }
            }
        }
    }
}

fn precheck_workspace_candidate_intersection(
    app_state: &AppState,
    router_config: &RouterConfig,
    endpoint_type: EndpointType,
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
) -> Option<InternalError> {
    let workspace_id = allowlist_workspace_id_for_request(auth_ctx)?;

    let balance_cfg = router_config.load_balance.0.get(&endpoint_type)?;

    let candidates: HashSet<_> = balance_cfg.providers().into_iter().collect();
    if candidates.is_empty() {
        return None;
    }

    let has_allowed = candidates
        .iter()
        .any(|provider| app_state.is_provider_allowed_for_workspace(workspace_id, provider));
    if has_allowed {
        return None;
    }

    let denied_provider = candidates
        .into_iter()
        .next()
        .expect("candidate set is known to be non-empty");
    tracing::warn!(
        workspace_id = %workspace_id,
        endpoint_type = ?endpoint_type,
        provider = %denied_provider,
        "no provider candidate intersects workspace allowlist (F-10 precheck)"
    );
    Some(InternalError::ProviderNotAllowedForWorkspace(
        denied_provider,
    ))
}

fn allowlist_workspace_id_for_request(
    auth_ctx: Option<&crate::types::extensions::AuthContext>,
) -> Option<uuid::Uuid> {
    auth_ctx.map(|ctx| *ctx.org_id.as_ref())
}
