use std::{
    convert::Infallible,
    future::{Ready, ready},
    task::{Context, Poll},
};

use pin_project_lite::pin_project;
use tower::{Service as _, ServiceBuilder, buffer::BufferLayer, util::BoxCloneService};
use tower_http::auth::AsyncRequireAuthorizationLayer;

use crate::{
    app_state::AppState,
    error::{api::ApiError, init::InitError, invalid_req::InvalidRequestError},
    middleware::{model_support::ModelSupportLayer, routing_precheck::RoutingPrecheckLayer},
    router::{
        router_details::{RouteType, RouterDetailsLayer},
        unified_api,
    },
    utils::handle_error::{ErrorHandler, ErrorHandlerLayer},
};

pub(crate) const MIDDLEWARE_BUFFER_SIZE: usize = 256;

pub type UnifiedApiService = ErrorHandler<unified_api::Service>;

#[derive(Debug)]
pub struct MetaRouter {
    unified_api: UnifiedApiService,
}

pub type MetaRouterService =
    BoxCloneService<crate::types::request::Request, crate::types::response::Response, Infallible>;

impl MetaRouter {
    pub async fn build(app_state: AppState) -> Result<MetaRouterService, InitError> {
        let meta_router = Self::cloud(app_state.clone()).await?;

        let service_stack = ServiceBuilder::new()
            .layer(ErrorHandlerLayer::new(app_state.clone()))
            .layer(RouterDetailsLayer::new())
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

        Ok(Self { unified_api })
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
}

impl tower::Service<crate::types::request::Request> for MetaRouter {
    type Response = crate::types::response::Response;
    type Error = ApiError;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.unified_api.poll_ready(ctx).map_err(|e| match e {})
    }

    fn call(&mut self, req: crate::types::request::Request) -> Self::Future {
        let route_type = req.extensions().get::<RouteType>().cloned();
        match route_type {
            Some(RouteType::UnifiedApi { path }) => self.handle_unified_api_request(req, &path),
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
    }
}

impl std::future::Future for ResponseFuture {
    type Output = Result<crate::types::response::Response, ApiError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project() {
            ResponseFutureProj::Ready { future } => future.poll(cx),
            ResponseFutureProj::UnifiedApi { future } => future.poll(cx).map_err(|e| match e {}),
        }
    }
}
