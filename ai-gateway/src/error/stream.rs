use axum_core::response::{IntoResponse, Response};
use displaydoc::Display;
use http::StatusCode;
use thiserror::Error;

use super::api::ErrorResponse;
use crate::{
    error::api::ErrorDetails,
    middleware::mapper::openai::{
        INVALID_REQUEST_ERROR_TYPE, SERVER_ERROR_TYPE,
    },
    types::json::Json,
};

#[derive(Debug, strum::AsRefStr, Error, Display)]
pub enum StreamError {
    /// Stream error: {0}
    StreamError(#[from] Box<reqwest_eventsource::Error>),
    /// Upstream error (status {status_code}): {body}
    UpstreamError {
        status_code: StatusCode,
        body: String,
    },
    /// Body error: {0}
    BodyError(axum_core::Error),
}

impl StreamError {
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            StreamError::StreamError(error) => match &**error {
                reqwest_eventsource::Error::Utf8(_)
                | reqwest_eventsource::Error::Parser(_)
                | reqwest_eventsource::Error::Transport(_) => true,
                reqwest_eventsource::Error::InvalidStatusCode(
                    status_code,
                    _response,
                ) => status_code.is_server_error(),

                reqwest_eventsource::Error::InvalidLastEventId(_)
                | reqwest_eventsource::Error::InvalidContentType(_, _)
                | reqwest_eventsource::Error::StreamEnded => false,
            },
            StreamError::UpstreamError { status_code, .. } => {
                status_code.is_server_error()
            }
            StreamError::BodyError(_error) => false,
        }
    }
}

impl IntoResponse for StreamError {
    fn into_response(self) -> Response {
        match self {
            Self::UpstreamError { status_code, body } => {
                if status_code.is_server_error() {
                    tracing::error!(
                        status_code = %status_code,
                        "upstream server error in stream"
                    );
                } else if status_code.is_client_error() {
                    tracing::debug!(
                        status_code = %status_code,
                        "upstream client error in stream"
                    );
                }

                if let Ok(upstream_error) =
                    serde_json::from_str::<ErrorResponse>(&body)
                {
                    (status_code, Json(upstream_error)).into_response()
                } else {
                    let error_type = if status_code.is_server_error() {
                        SERVER_ERROR_TYPE
                    } else {
                        INVALID_REQUEST_ERROR_TYPE
                    };
                    (
                        status_code,
                        Json(ErrorResponse {
                            error: ErrorDetails {
                                message: body,
                                r#type: Some(error_type.to_string()),
                                param: None,
                                code: None,
                            },
                        }),
                    )
                        .into_response()
                }
            }
            Self::StreamError(error) => {
                tracing::error!(error = %error, "internal error in stream");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse {
                        error: ErrorDetails {
                            message: error.to_string(),
                            r#type: Some(SERVER_ERROR_TYPE.to_string()),
                            param: None,
                            code: None,
                        },
                    }),
                )
                    .into_response()
            }
            Self::BodyError(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: ErrorDetails {
                        message: error.to_string(),
                        r#type: Some(SERVER_ERROR_TYPE.to_string()),
                        param: None,
                        code: None,
                    },
                }),
            )
                .into_response(),
        }
    }
}

/// Auth errors for metrics. This is a special type
/// that avoids including dynamic information to limit cardinality
/// such that we can use this type in metrics.
#[derive(Debug, Error, Display, strum::AsRefStr)]
pub enum StreamErrorMetric {
    /// Event stream error
    StreamError,
    /// Upstream error
    UpstreamError,
    /// Body error
    BodyError,
}

impl From<&StreamError> for StreamErrorMetric {
    fn from(error: &StreamError) -> Self {
        match error {
            StreamError::StreamError(_) => Self::StreamError,
            StreamError::UpstreamError { .. } => Self::UpstreamError,
            StreamError::BodyError(_) => Self::BodyError,
        }
    }
}
