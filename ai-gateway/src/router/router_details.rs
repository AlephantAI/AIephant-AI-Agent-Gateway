use std::{
    future::{Ready, ready},
    str::FromStr,
    task::{Context, Poll},
};

use compact_str::CompactString;
use futures::future::Either;
use http::uri::PathAndQuery;
use regex::Regex;

use crate::{
    error::{
        api::ApiError, internal::InternalError,
        invalid_req::InvalidRequestError,
    },
    types::{extensions::RequestKind, request::Request, response::Response},
};

/// Regex for extracting the first path segment and the rest of the path.
const UNIFIED_URL_REGEX: &str =
    r"^/(?P<first_segment>[^/?]+)(?P<rest>/[^?]*)?(?P<query>\?.*)?$";
const X402_AGENT_ROUTE_PREFIX: &str = "/x402/agents/";
const X402_API_ROUTE_PREFIX: &str = "/x402/api/";

pub struct RouterDetailsLayer {}

impl RouterDetailsLayer {
    pub fn new() -> Self {
        Self {}
    }
}

impl<S> tower::Layer<S> for RouterDetailsLayer {
    type Service = RouterDetailsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RouterDetailsService {
            inner,
            unified_url_regex: Regex::new(UNIFIED_URL_REGEX).unwrap(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouterDetailsService<S> {
    inner: S,
    unified_url_regex: Regex,
}

#[derive(Debug, Clone)]
pub enum RouteType {
    AgentEvents,
    UnifiedApi {
        path: CompactString,
    },
    X402Agent {
        slug: CompactString,
        remaining_path: CompactString,
        route_kind: X402RouteKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X402RouteKind {
    Agent,
    HttpApi,
}

impl X402RouteKind {
    #[must_use]
    pub fn expected_endpoint_type(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::HttpApi => "http_api",
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agents",
            Self::HttpApi => "api",
        }
    }
}

impl<S> RouterDetailsService<S> {
    fn parse_route(&self, request: &Request) -> Result<RouteType, ApiError> {
        let path = request.uri().path();
        if let Some((route_kind, slug, remaining_path)) = parse_x402_path(path)
        {
            return Ok(RouteType::X402Agent {
                slug: slug.into(),
                remaining_path: remaining_path.into(),
                route_kind,
            });
        }
        if path.starts_with("/x402/agents") || path.starts_with("/x402/api") {
            return Err(ApiError::InvalidRequest(
                InvalidRequestError::NotFound(path.to_string()),
            ));
        }

        let Some(captures) = self.unified_url_regex.captures(path) else {
            return Err(ApiError::InvalidRequest(
                InvalidRequestError::NotFound(path.to_string()),
            ));
        };
        let first_segment = captures
            .name("first_segment")
            .ok_or_else(|| {
                ApiError::InvalidRequest(InvalidRequestError::NotFound(
                    path.to_string(),
                ))
            })?
            .as_str();
        if first_segment != "v1" {
            return Err(ApiError::InvalidRequest(
                InvalidRequestError::NotFound(path.to_string()),
            ));
        }
        let rest_path = captures
            .name("rest")
            .map(|m| m.as_str())
            .unwrap_or_default();
        if rest_path == "/agent/events" {
            return Ok(RouteType::AgentEvents);
        }
        Ok(RouteType::UnifiedApi {
            path: rest_path.trim_start_matches('/').into(),
        })
    }
}

fn parse_x402_path(path: &str) -> Option<(X402RouteKind, &str, &str)> {
    let (route_kind, rest) =
        if let Some(rest) = path.strip_prefix(X402_AGENT_ROUTE_PREFIX) {
            (X402RouteKind::Agent, rest)
        } else if let Some(rest) = path.strip_prefix(X402_API_ROUTE_PREFIX) {
            (X402RouteKind::HttpApi, rest)
        } else {
            return None;
        };
    let (slug, remaining) =
        rest.split_once('/')
            .map_or((rest, ""), |(slug, remaining)| {
                (
                    slug,
                    if remaining.is_empty() {
                        ""
                    } else {
                        &rest[slug.len()..]
                    },
                )
            });
    let slug = slug.trim();
    (!slug.is_empty()).then_some((route_kind, slug, remaining))
}

fn extract_path_and_query(
    path: &str,
    query: Option<&str>,
) -> Result<PathAndQuery, ApiError> {
    let path_and_query = if let Some(query_params) = query {
        PathAndQuery::from_str(&format!("{path}?{query_params}"))
    } else {
        PathAndQuery::from_str(path)
    };

    path_and_query.map_err(|e| {
        tracing::warn!(error = %e, "Failed to convert extracted path to PathAndQuery");
        ApiError::Internal(InternalError::Internal)
    })
}

impl<S> tower::Service<Request> for RouterDetailsService<S>
where
    S: tower::Service<Request, Response = Response, Error = ApiError>,
    S::Future: Send + 'static,
{
    type Response = Response;
    type Error = ApiError;
    type Future = Either<Ready<Result<Self::Response, Self::Error>>, S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut req: Request) -> Self::Future {
        let route_type = match self.parse_route(&req) {
            Ok(rt) => rt,
            Err(e) => return Either::Left(ready(Err(e))),
        };

        match &route_type {
            RouteType::AgentEvents => {
                req.extensions_mut().insert(RequestKind::AgentEvents);
            }
            RouteType::UnifiedApi { path } => {
                tracing::info!(path = %path, "unified api request path");
                let extracted_path_and_query =
                    match extract_path_and_query(path, req.uri().query()) {
                        Ok(p) => p,
                        Err(e) => {
                            return Either::Left(ready(Err(e)));
                        }
                    };
                req.extensions_mut().insert(extracted_path_and_query);
                req.extensions_mut().insert(RequestKind::UnifiedApi);
            }
            RouteType::X402Agent { .. } => {
                req.extensions_mut().insert(RequestKind::X402Agent);
            }
        }
        req.extensions_mut().insert(route_type);

        let future = self.inner.call(req);
        Either::Right(future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_join() {
        let url = "https://api.groq.com/openai/";
        let url = url::Url::parse(url).unwrap();
        let url = url.join("v1/chat/completions").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.groq.com/openai/v1/chat/completions"
        );
    }

    #[test]
    fn test_unified_regex() {
        let regex =
            Regex::new(UNIFIED_URL_REGEX).expect("Regex should be valid");

        assert!(regex.is_match("/v1"));
        assert!(regex.is_match("/v1/chat/completions"));
        assert!(regex.is_match("/v1/chat/completions?user=test"));

        assert!(!regex.is_match("/"));
        assert!(!regex.is_match("//double-slash"));
    }

    fn service() -> RouterDetailsService<
        tower::util::ServiceFn<
            fn(Request) -> std::future::Ready<Result<Response, ApiError>>,
        >,
    > {
        fn handler(
            _req: Request,
        ) -> std::future::Ready<Result<Response, ApiError>> {
            std::future::ready(Ok::<Response, ApiError>(
                http::Response::builder()
                    .body(axum_core::body::Body::empty())
                    .unwrap(),
            ))
        }

        RouterDetailsService {
            inner: tower::service_fn(handler),
            unified_url_regex: Regex::new(UNIFIED_URL_REGEX).unwrap(),
        }
    }

    #[test]
    fn parses_v1_as_unified_api() {
        let service = service();

        let request = http::Request::builder()
            .uri("http://router.alephant.test/v1/chat/completions")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request).expect("route should parse"),
            RouteType::UnifiedApi { path } if path == "chat/completions"
        ));
    }

    #[test]
    fn parses_agent_events_route() {
        let service = service();

        let request = http::Request::builder()
            .uri("http://router.alephant.test/v1/agent/events")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request).expect("route should parse"),
            RouteType::AgentEvents
        ));
    }

    #[test]
    fn parses_x402_agent_route() {
        let service = service();

        let request = http::Request::builder()
            .uri("http://router.alephant.test/x402/agents/test-agent/foo/bar")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request).expect("route should parse"),
            RouteType::X402Agent { slug, remaining_path, route_kind }
                if slug == "test-agent"
                    && remaining_path == "/foo/bar"
                    && route_kind == X402RouteKind::Agent
        ));
    }

    #[test]
    fn parses_x402_api_route() {
        let service = service();

        let request = http::Request::builder()
            .uri("http://router.alephant.test/x402/api/test-api/foo/bar")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request).expect("route should parse"),
            RouteType::X402Agent { slug, remaining_path, route_kind }
                if slug == "test-api"
                    && remaining_path == "/foo/bar"
                    && route_kind == X402RouteKind::HttpApi
        ));
    }

    #[test]
    fn x402_agent_path_extracts_slug_only() {
        assert_eq!(
            parse_x402_path("/x402/agents/test-agent"),
            Some((X402RouteKind::Agent, "test-agent", ""))
        );
    }

    #[test]
    fn x402_api_path_extracts_slug_only() {
        assert_eq!(
            parse_x402_path("/x402/api/test-api"),
            Some((X402RouteKind::HttpApi, "test-api", ""))
        );
    }

    #[test]
    fn x402_agent_path_rejects_missing_slug() {
        assert_eq!(parse_x402_path("/x402/agents/"), None);
    }

    #[test]
    fn x402_api_path_rejects_missing_slug() {
        assert_eq!(parse_x402_path("/x402/api/"), None);
    }

    #[test]
    fn x402_agent_route_rejects_missing_slug() {
        let service = service();
        let request = http::Request::builder()
            .uri("/x402/agents/")
            .body(axum_core::body::Body::empty())
            .unwrap();

        let err = service
            .parse_route(&request)
            .expect_err("missing x402 agent slug should be rejected");

        assert!(matches!(
            err,
            ApiError::InvalidRequest(InvalidRequestError::NotFound(path))
                if path == "/x402/agents/"
        ));
    }

    #[test]
    fn x402_api_route_rejects_missing_slug() {
        let service = service();
        let request = http::Request::builder()
            .uri("/x402/api/")
            .body(axum_core::body::Body::empty())
            .unwrap();

        let err = service
            .parse_route(&request)
            .expect_err("missing x402 api slug should be rejected");

        assert!(matches!(
            err,
            ApiError::InvalidRequest(InvalidRequestError::NotFound(path))
                if path == "/x402/api/"
        ));
    }

    #[test]
    fn non_v1_provider_prefix_is_not_found() {
        let service = service();

        let request = http::Request::builder()
            .uri("http://router.alephant.test/openai/v1/chat/completions")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request),
            Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(_)))
        ));
    }

    #[test]
    fn router_prefix_is_not_found() {
        let service = service();

        let request = http::Request::builder()
            .uri("http://router.alephant.test/router/default/v1/chat/completions")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request),
            Err(ApiError::InvalidRequest(InvalidRequestError::NotFound(_)))
        ));
    }

    #[test]
    fn removed_routing_override_header_is_ignored() {
        let service = service();
        let header_name = ["alephant", "forced", "routing"].join("-");

        let request = http::Request::builder()
            .uri("http://router.alephant.test/v1/chat/completions")
            .header(header_name.as_str(), "anthropic")
            .body(axum_core::body::Body::empty())
            .unwrap();

        assert!(matches!(
            service.parse_route(&request).expect("route should parse"),
            RouteType::UnifiedApi { path } if path == "chat/completions"
        ));
    }
}
