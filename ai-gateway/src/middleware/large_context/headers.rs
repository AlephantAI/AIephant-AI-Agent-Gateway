use http::HeaderMap;

use crate::error::invalid_req::InvalidRequestError;

pub const ALEPHANT_HANDLER_HEADER: &str =
    "Alephant-Token-Limit-Exception-Handler";
pub const ALEPHANT_MODEL_OVERRIDE_HEADER: &str = "Alephant-Model-Override";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenLimitExceptionHandler {
    Truncate,
    MiddleOut,
    Fallback,
}

impl TokenLimitExceptionHandler {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Truncate => "truncate",
            Self::MiddleOut => "middle-out",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LargeContextHeaders {
    pub handler: Option<TokenLimitExceptionHandler>,
    pub model_override: Option<String>,
}

fn parse_header_string(
    headers: &HeaderMap,
    key: &str,
) -> Result<Option<String>, InvalidRequestError> {
    headers
        .get(key)
        .map(|value| value.to_str().map(str::trim).map(str::to_string))
        .transpose()
        .map_err(InvalidRequestError::InvalidRequestHeader)
        .map(|value| value.filter(|value| !value.is_empty()))
}

fn parse_handler_value(
    value: &str,
) -> Result<TokenLimitExceptionHandler, InvalidRequestError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "truncate" => Ok(TokenLimitExceptionHandler::Truncate),
        "middle-out" => Ok(TokenLimitExceptionHandler::MiddleOut),
        "fallback" => Ok(TokenLimitExceptionHandler::Fallback),
        _ => Err(InvalidRequestError::InvalidLargeContextHandler(
            value.to_string(),
        )),
    }
}

pub fn parse_large_context_headers(
    headers: &HeaderMap,
) -> Result<LargeContextHeaders, InvalidRequestError> {
    let handler = parse_header_string(headers, ALEPHANT_HANDLER_HEADER)?
        .map(|value| parse_handler_value(&value))
        .transpose()?;
    let model_override =
        parse_header_string(headers, ALEPHANT_MODEL_OVERRIDE_HEADER)?;

    Ok(LargeContextHeaders {
        handler,
        model_override,
    })
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue};

    use super::{
        ALEPHANT_HANDLER_HEADER, ALEPHANT_MODEL_OVERRIDE_HEADER,
        LargeContextHeaders, TokenLimitExceptionHandler,
        parse_large_context_headers,
    };
    use crate::error::invalid_req::InvalidRequestError;

    #[test]
    fn reads_alephant_handler_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ALEPHANT_HANDLER_HEADER,
            HeaderValue::from_static("truncate"),
        );

        let parsed = parse_large_context_headers(&headers).unwrap();
        assert_eq!(
            parsed,
            LargeContextHeaders {
                handler: Some(TokenLimitExceptionHandler::Truncate),
                model_override: None,
            }
        );
    }

    #[test]
    fn reads_alephant_model_override_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ALEPHANT_MODEL_OVERRIDE_HEADER,
            HeaderValue::from_static("openai/gpt-4o-mini"),
        );

        let parsed = parse_large_context_headers(&headers).unwrap();
        assert_eq!(
            parsed.model_override.as_deref(),
            Some("openai/gpt-4o-mini")
        );
    }

    #[test]
    fn ignores_legacy_override_headers_when_alephant_headers_absent() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Legacy-Token-Limit-Exception-Handler",
            HeaderValue::from_static("fallback"),
        );
        headers.insert(
            "Legacy-Model-Override",
            HeaderValue::from_static("openai/gpt-4o-mini,openai/gpt-4o"),
        );

        let parsed = parse_large_context_headers(&headers).unwrap();
        assert_eq!(parsed.handler, None);
        assert_eq!(parsed.model_override, None);
    }

    #[test]
    fn handler_is_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ALEPHANT_HANDLER_HEADER,
            HeaderValue::from_static("Middle-Out"),
        );

        let parsed = parse_large_context_headers(&headers).unwrap();
        assert_eq!(parsed.handler, Some(TokenLimitExceptionHandler::MiddleOut));
    }

    #[test]
    fn rejects_invalid_handler() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ALEPHANT_HANDLER_HEADER,
            HeaderValue::from_static("explode"),
        );

        let error = parse_large_context_headers(&headers).unwrap_err();
        assert!(matches!(
            error,
            InvalidRequestError::InvalidLargeContextHandler(_)
        ));
    }
}
