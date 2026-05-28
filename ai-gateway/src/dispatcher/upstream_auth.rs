use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};

use crate::{
    app_state::AppState,
    dispatcher::client::{Client, ProviderClient},
    error::api::ApiError,
    session_headers::remove_session_headers,
    types::{extensions::AuthContext, provider::InferenceProvider},
    utils::debug_log,
};

pub(crate) struct UpstreamAuthApplier<'a> {
    app_state: &'a AppState,
}

pub(crate) struct UpstreamAuthRequest<'a> {
    pub(crate) client: &'a Client,
    pub(crate) request_builder: reqwest::RequestBuilder,
    pub(crate) req_body_bytes: &'a Bytes,
    pub(crate) auth_context: Option<&'a AuthContext>,
    pub(crate) provider: InferenceProvider,
}

impl<'a> UpstreamAuthApplier<'a> {
    #[must_use]
    pub(crate) fn new(app_state: &'a AppState) -> Self {
        Self { app_state }
    }

    pub(crate) fn sanitize_headers(headers: &mut HeaderMap) {
        headers.remove(http::header::HOST);
        headers.remove(http::header::AUTHORIZATION);
        headers.remove(http::header::CONTENT_LENGTH);
        remove_internal_gateway_auth_headers(headers);
        debug_log::remove_debug_control_headers(headers);
        headers.remove("Alephant-Embeddings-Key");
        headers.remove("Alephant-Embeddings-Model");
        headers.remove("Alephant-Cache-Semantic-Threshold");
        headers.remove("Alephant-Cache-Ttl");
        remove_session_headers(headers);
        headers.remove(http::header::ACCEPT_ENCODING);
        headers.insert(
            http::header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
    }

    pub(crate) async fn apply(
        &self,
        request: UpstreamAuthRequest<'_>,
    ) -> Result<reqwest::RequestBuilder, ApiError> {
        request
            .client
            .authenticate(
                self.app_state,
                request.request_builder,
                request.req_body_bytes,
                request.auth_context,
                request.provider,
            )
            .await
    }
}

fn remove_internal_gateway_auth_headers(headers: &mut HeaderMap) {
    headers.remove(HeaderName::from_static("alephant-api-key"));
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};

    use super::*;

    #[test]
    fn remove_internal_gateway_auth_headers_removes_alephant_key() {
        let mut headers = HeaderMap::new();
        headers.insert("alephant-api-key", HeaderValue::from_static("new"));

        remove_internal_gateway_auth_headers(&mut headers);

        assert!(!headers.contains_key("alephant-api-key"));
    }

    #[test]
    fn remove_internal_gateway_auth_headers_keeps_other_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer provider-key"),
        );

        remove_internal_gateway_auth_headers(&mut headers);

        assert!(headers.contains_key(http::header::AUTHORIZATION));
    }

    #[test]
    fn sanitize_headers_removes_gateway_auth_and_debug_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::HOST, HeaderValue::from_static("gateway"));
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer user-key"),
        );
        headers.insert(
            http::header::CONTENT_LENGTH,
            HeaderValue::from_static("9"),
        );
        headers.insert("alephant-api-key", HeaderValue::from_static("new"));
        headers
            .insert("alephant-debug-headers", HeaderValue::from_static("true"));
        headers.insert("alephant-debug-body", HeaderValue::from_static("true"));

        UpstreamAuthApplier::sanitize_headers(&mut headers);

        assert!(!headers.contains_key(http::header::HOST));
        assert!(!headers.contains_key(http::header::AUTHORIZATION));
        assert!(!headers.contains_key(http::header::CONTENT_LENGTH));
        assert!(!headers.contains_key("alephant-api-key"));
        assert!(!headers.contains_key("alephant-debug-headers"));
        assert!(!headers.contains_key("alephant-debug-body"));
    }

    #[test]
    fn sanitize_headers_sets_accept_encoding_identity() {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, br"),
        );

        UpstreamAuthApplier::sanitize_headers(&mut headers);

        assert_eq!(
            headers.get(http::header::ACCEPT_ENCODING),
            Some(&HeaderValue::from_static("identity"))
        );
    }

    #[test]
    fn sanitize_headers_removes_internal_cache_and_session_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Alephant-Embeddings-Key",
            HeaderValue::from_static("embed-key"),
        );
        headers.insert(
            "Alephant-Embeddings-Model",
            HeaderValue::from_static("embed-model"),
        );
        headers.insert(
            "Alephant-Cache-Semantic-Threshold",
            HeaderValue::from_static("0.8"),
        );
        headers.insert("Alephant-Cache-Ttl", HeaderValue::from_static("3600"));
        headers
            .insert("alephant-session-id", HeaderValue::from_static("session"));
        headers
            .insert("alephant-session-path", HeaderValue::from_static("/repo"));
        headers.insert(
            "alephant-session-name",
            HeaderValue::from_static("coding"),
        );

        UpstreamAuthApplier::sanitize_headers(&mut headers);

        assert!(!headers.contains_key("Alephant-Embeddings-Key"));
        assert!(!headers.contains_key("Alephant-Embeddings-Model"));
        assert!(!headers.contains_key("Alephant-Cache-Semantic-Threshold"));
        assert!(!headers.contains_key("Alephant-Cache-Ttl"));
        assert!(!headers.contains_key("alephant-session-id"));
        assert!(!headers.contains_key("alephant-session-path"));
        assert!(!headers.contains_key("alephant-session-name"));
    }
}
