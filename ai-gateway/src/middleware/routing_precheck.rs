//! Route and HTTP-method checks **before** authentication.
//!
//! Ensures unknown paths return 404 and disallowed methods (e.g. GET on
//! POST-only inference paths) return 405 without consuming auth or request
//! bodies.

use std::task::{Context, Poll};

use http::{Method, uri::PathAndQuery};
use tower::{Layer, Service};

use crate::{
    error::{
        api::ApiError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    router::{router_details::RouteType, unified_api::UnifiedApi},
    types::{request::Request, response::Response},
};

#[derive(Clone)]
pub struct RoutingPrecheckLayer;

impl Default for RoutingPrecheckLayer {
    fn default() -> Self {
        Self
    }
}

impl RoutingPrecheckLayer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for RoutingPrecheckLayer {
    type Service = RoutingPrecheckService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RoutingPrecheckService { inner }
    }
}

#[derive(Clone)]
pub struct RoutingPrecheckService<S> {
    inner: S,
}

/// Paths that mirror upstream LLM HTTP APIs and expect a JSON **POST** body.
#[must_use]
pub(crate) fn path_requires_post(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    if p == "chat/completions" || p.ends_with("/chat/completions") {
        return true;
    }
    if p == "v1/chat/completions" || p.ends_with("/v1/chat/completions") {
        return true;
    }
    // Unified API uses stripped subpaths: `messages`, not `v1/messages`.
    if p == "messages"
        || p == "v1/messages"
        || p.ends_with("/v1/messages")
        || p.ends_with("/messages")
    {
        return true;
    }
    if p.contains("/converse") {
        return true;
    }
    if p.contains("v1beta/openai/chat/completions") {
        return true;
    }
    if p == "embeddings"
        || p.ends_with("/v1/embeddings")
        || p.ends_with("v1/embeddings")
        || p.ends_with("/embeddings")
    {
        return true;
    }
    if p == "images/generations" || p.contains("images/generations") {
        return true;
    }
    if p == "responses"
        || p == "v1/responses"
        || p.ends_with("/v1/responses")
        || p.ends_with("/responses")
    {
        return true;
    }
    // Legacy completions: bare `completions` or `.../completions` (not
    // `.../chat/completions`, handled above).
    if p == "completions"
        || (p.ends_with("/completions") && !p.ends_with("/chat/completions"))
    {
        return true;
    }
    false
}

fn check_method(method: &Method, path: &str) -> Result<(), ApiError> {
    if *method == Method::OPTIONS {
        // CORS preflight: must not require POST or auth.
        return Ok(());
    }
    if path.trim_start_matches('/') == "models" && *method != Method::GET {
        return Err(ApiError::InvalidRequest(
            InvalidRequestError::MethodNotAllowed {
                method: method.as_str().to_string(),
                path: path.to_string(),
            },
        ));
    }
    if path_requires_post(path) && *method != Method::POST {
        return Err(ApiError::InvalidRequest(
            InvalidRequestError::MethodNotAllowed {
                method: method.as_str().to_string(),
                path: path.to_string(),
            },
        ));
    }
    Ok(())
}

fn precheck(req: &Request) -> Result<(), ApiError> {
    let Some(route_type) = req.extensions().get::<RouteType>() else {
        return Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(
            req.uri().path().to_string(),
        )));
    };

    let path_and_query =
        req.extensions().get::<PathAndQuery>().ok_or_else(|| {
            ApiError::Internal(InternalError::ExtensionNotFound("PathAndQuery"))
        })?;
    let path = path_and_query.path();

    let RouteType::UnifiedApi { .. } = route_type;
    UnifiedApi::try_from(path).map_err(|_| {
        ApiError::InvalidRequest(InvalidRequestError::NotFound(
            req.uri().path().to_string(),
        ))
    })?;
    check_method(req.method(), path)?;

    Ok(())
}

impl<S> Service<Request> for RoutingPrecheckService<S>
where
    S: Service<Request, Response = Response, Error = ApiError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = futures::future::Either<
        std::future::Ready<Result<Self::Response, Self::Error>>,
        S::Future,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        if let Err(e) = precheck(&req) {
            return futures::future::Either::Left(std::future::ready(Err(e)));
        }
        futures::future::Either::Right(self.inner.call(req))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn request_with_route(
        path: &str,
        route_type: RouteType,
        method: Method,
    ) -> Request {
        let mut req = http::Request::builder()
            .method(method)
            .uri(format!("http://router.alephant.test/{path}"))
            .body(axum_core::body::Body::empty())
            .unwrap();
        req.extensions_mut().insert(route_type);
        req.extensions_mut()
            .insert(PathAndQuery::from_str(path).unwrap());
        req
    }

    #[test]
    fn precheck_accepts_unified_chat_post() {
        let req = request_with_route(
            "chat/completions",
            RouteType::UnifiedApi {
                path: "chat/completions".into(),
            },
            Method::POST,
        );
        assert!(precheck(&req).is_ok());
    }

    #[test]
    fn precheck_rejects_unknown_unified_endpoint() {
        let req = request_with_route(
            "not-supported",
            RouteType::UnifiedApi {
                path: "not-supported".into(),
            },
            Method::POST,
        );
        assert!(matches!(
            precheck(&req),
            Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(_)))
        ));
    }

    #[test]
    fn post_required_heuristics() {
        assert!(path_requires_post("chat/completions"));
        assert!(path_requires_post("/v1/chat/completions"));
        assert!(path_requires_post("prefix/v1/messages"));
        assert!(path_requires_post("messages"));
        assert!(path_requires_post("completions"));
        assert!(path_requires_post("embeddings"));
        assert!(path_requires_post("images/generations"));
        assert!(path_requires_post("responses"));
        assert!(path_requires_post("v1/responses"));
        assert!(path_requires_post("model/foo/converse"));
        assert!(path_requires_post("v1beta/openai/chat/completions"));
        assert!(!path_requires_post("v1/models"));
        assert!(!path_requires_post("openapi.json"));
    }

    #[test]
    fn models_allows_get_only() {
        assert!(check_method(&Method::GET, "models").is_ok());
        assert!(check_method(&Method::POST, "models").is_err());
    }
}
