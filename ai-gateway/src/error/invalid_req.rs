use axum_core::response::IntoResponse;
use displaydoc::Display;
use http::{HeaderMap, StatusCode};
use thiserror::Error;
use tracing::debug;

use crate::{
    error::api::{ErrorDetails, ErrorResponse},
    middleware::mapper::openai::INVALID_REQUEST_ERROR_TYPE,
    types::{json::Json, provider::InferenceProvider},
};

#[derive(Debug, Display)]
#[displaydoc("Retry after {retry_after}s.")]
pub struct TooManyRequestsError {
    /// Request limit
    pub ratelimit_limit: u64,
    /// Number of requests left for the time window
    pub ratelimit_remaining: u64,
    /// Number of seconds in which the API will become available again after
    /// its rate limit has been exceeded
    pub retry_after: u64,
}

/// User errors
#[derive(Debug, Error, Display, strum::AsRefStr)]
#[ignore_extra_doc_attributes]
pub enum InvalidRequestError {
    /// Resource not found: {0}
    NotFound(String),
    /// HTTP method `{method}` is not allowed for `{path}`
    MethodNotAllowed { method: String, path: String },
    /// Unsupported provider: {0}
    UnsupportedProvider(InferenceProvider),
    /// Unsupported endpoint: {0}
    UnsupportedEndpoint(String),
    /// Router id not found: {0}
    RouterIdNotFound(String),
    /// Missing router id in request path
    MissingRouterId,
    /// Missing model id in request body
    MissingModelId,
    /// Invalid model id in request body
    InvalidModelId,
    /// Unsupported model: {0}
    UnsupportedGatewayModel(String),
    /// Invalid request: {0}
    InvalidRequest(http::Error),
    /// Invalid request url: {0}
    InvalidUrl(String),
    /// Invalid request body: {0}
    InvalidRequestBody(#[from] serde_json::Error),
    /// Upstream 4xx error: {0}
    Provider4xxError(StatusCode),
    /// Too many requests: {0}
    TooManyRequests(TooManyRequestsError),
    /// Invalid request header: {0}
    InvalidRequestHeader(http::header::ToStrError),
    /// Invalid large context handler: {0}
    InvalidLargeContextHandler(String),
    /// Invalid prompt inputs: {0}
    InvalidPromptInputs(String),
    /// Model access denied: {0}
    ModelAccessDenied(String),
    /// No model available
    NoModelAvailable,
    /// {message}
    PiicacheOutBodyMissing { message: String },
    /// {message}
    PromptCacheInvalid { message: String },
    /// Ambiguous model '{model_id}' matches providers: {candidates:?}
    AmbiguousBareModel {
        model_id: String,
        candidates: Vec<String>,
    },
    /// Custom provider requires a base_url in master_key configuration
    CustomProviderMissingBaseUrl,
    /// Content filter denied (HTTP 200 + OpenAI-style JSON for clients).
    ContentPolicyDenied { message: String },
}

impl IntoResponse for InvalidRequestError {
    #[allow(clippy::too_many_lines)]
    fn into_response(self) -> axum_core::response::Response {
        debug!(error = %self, "Invalid request");
        let message = self.to_string();
        match self {
            Self::ContentPolicyDenied { message } => (
                StatusCode::OK,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: Some("content_filter".to_string()),
                    },
                }),
            )
                .into_response(),
            Self::MethodNotAllowed { .. } => (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: Some("method_not_allowed".to_string()),
                    },
                }),
            )
                .into_response(),
            Self::NotFound(_) | Self::RouterIdNotFound(_) => (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: None,
                    },
                }),
            )
                .into_response(),
            Self::Provider4xxError(status) => (
                status,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: None,
                    },
                }),
            )
                .into_response(),
            Self::TooManyRequests(error) => {
                let mut headers = HeaderMap::new();
                headers.insert(
                    "retry-after",
                    error.retry_after.to_string().parse().unwrap(),
                );
                headers.insert(
                    "x-ratelimit-after",
                    error.retry_after.to_string().parse().unwrap(),
                );
                headers.insert(
                    "x-ratelimit-limit",
                    error.ratelimit_limit.to_string().parse().unwrap(),
                );
                headers.insert(
                    "x-ratelimit-remaining",
                    error.ratelimit_remaining.to_string().parse().unwrap(),
                );
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    headers,
                    Json(ErrorResponse {
                        error: ErrorDetails {
                            message,
                            r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                            param: None,
                            code: None,
                        },
                    }),
                )
                    .into_response()
            }
            Self::ModelAccessDenied(_) => (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: None,
                    },
                }),
            )
                .into_response(),
            Self::NoModelAvailable => (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: None,
                    },
                }),
            )
                .into_response(),
            Self::PiicacheOutBodyMissing { message } => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: Some("piicache_out_body_missing".to_string()),
                    },
                }),
            )
                .into_response(),
            Self::PromptCacheInvalid { message } => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: Some("prompt_cache_invalid".to_string()),
                    },
                }),
            )
                .into_response(),
            Self::AmbiguousBareModel {
                model_id,
                candidates,
            } => {
                let candidates_str = candidates.join(", ");
                let message = format!(
                    "Ambiguous model '{model_id}': matches multiple \
                     providers. Please specify one of: {candidates_str}"
                );
                (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: ErrorDetails {
                            message,
                            r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                            param: None,
                            code: Some("ambiguous_model".to_string()),
                        },
                    }),
                )
                    .into_response()
            }
            Self::CustomProviderMissingBaseUrl => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message: "Custom provider requires a base_url in \
                                  master_key configuration"
                            .to_string(),
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: Some("custom_provider_missing_base_url".to_string()),
                    },
                }),
            )
                .into_response(),
            _ => (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message,
                        r#type: Some(INVALID_REQUEST_ERROR_TYPE.to_string()),
                        param: None,
                        code: None,
                    },
                }),
            )
                .into_response(),
        }
    }
}

/// User errors for metrics. This is a special type
/// that avoids including dynamic information to limit cardinality
/// such that we can use this type in metrics.
#[derive(Debug, Error, Display, strum::AsRefStr)]
pub enum InvalidRequestErrorMetric {
    /// Resource not found
    NotFound,
    /// HTTP method not allowed for path
    MethodNotAllowed,
    /// Unsupported provider
    UnsupportedProvider,
    /// Invalid request
    InvalidRequest,
    /// Invalid request url
    InvalidUrl,
    /// Invalid request body
    InvalidRequestBody,
    /// Upstream 4xx error
    Provider4xxError,
    /// Too many requests
    TooManyRequests,
    /// Model access denied
    ModelAccessDenied,
    /// No default model (policy/price empty)
    NoModelAvailable,
    /// Unsupported gateway model (not in provider catalog)
    UnsupportedGatewayModel,
    /// Content policy denied (HTTP 200 to caller)
    ContentPolicyDenied,
    /// PII cache path: empty policy `out_body` (HTTP 400)
    PiicacheOutBodyMissing,
    /// Prompt cache JSON or merge failure (HTTP 400)
    PromptCacheInvalid,
    /// Ambiguous bare model id (multiple providers match)
    AmbiguousBareModel,
    /// Custom provider has no base_url in master_key
    CustomProviderMissingBaseUrl,
}

impl From<&InvalidRequestError> for InvalidRequestErrorMetric {
    fn from(error: &InvalidRequestError) -> Self {
        match error {
            InvalidRequestError::UnsupportedProvider(_) => Self::UnsupportedProvider,
            InvalidRequestError::MethodNotAllowed { .. } => Self::MethodNotAllowed,
            InvalidRequestError::NotFound(_)
            | InvalidRequestError::RouterIdNotFound(_)
            | InvalidRequestError::MissingRouterId
            | InvalidRequestError::InvalidRequestHeader(_) => Self::NotFound,
            InvalidRequestError::InvalidRequest(_)
            | InvalidRequestError::UnsupportedEndpoint(_)
            | InvalidRequestError::InvalidLargeContextHandler(_)
            | InvalidRequestError::InvalidPromptInputs(_)
            | InvalidRequestError::MissingModelId
            | InvalidRequestError::InvalidModelId => Self::InvalidRequest,
            InvalidRequestError::InvalidUrl(_) => Self::InvalidUrl,
            InvalidRequestError::InvalidRequestBody(_) => Self::InvalidRequestBody,
            InvalidRequestError::Provider4xxError(_) => Self::Provider4xxError,
            InvalidRequestError::TooManyRequests(_) => Self::TooManyRequests,
            InvalidRequestError::ModelAccessDenied(_) => Self::ModelAccessDenied,
            InvalidRequestError::NoModelAvailable => Self::NoModelAvailable,
            InvalidRequestError::UnsupportedGatewayModel(_) => Self::UnsupportedGatewayModel,
            InvalidRequestError::ContentPolicyDenied { .. } => Self::ContentPolicyDenied,
            InvalidRequestError::PiicacheOutBodyMissing { .. } => Self::PiicacheOutBodyMissing,
            InvalidRequestError::PromptCacheInvalid { .. } => Self::PromptCacheInvalid,
            InvalidRequestError::AmbiguousBareModel { .. } => Self::AmbiguousBareModel,
            InvalidRequestError::CustomProviderMissingBaseUrl => Self::CustomProviderMissingBaseUrl,
        }
    }
}
