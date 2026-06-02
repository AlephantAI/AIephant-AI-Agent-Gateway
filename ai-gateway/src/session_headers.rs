use http::HeaderMap;
use indexmap::IndexMap;

use crate::error::invalid_req::InvalidRequestError;

pub const ALEPHANT_SESSION_ID_HEADER: &str = "alephant-session-id";
pub const ALEPHANT_SESSION_PATH_HEADER: &str = "alephant-session-path";
pub const ALEPHANT_SESSION_NAME_HEADER: &str = "alephant-session-name";

pub const ALEPHANT_SESSION_ID_PROPERTY: &str = "Alephant-Session-Id";
pub const ALEPHANT_SESSION_PATH_PROPERTY: &str = "Alephant-Session-Path";
pub const ALEPHANT_SESSION_NAME_PROPERTY: &str = "Alephant-Session-Name";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHeaders {
    pub session_id: String,
    pub session_path: Option<String>,
    pub session_name: Option<String>,
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

fn normalize_session_path(path: Option<String>) -> Option<String> {
    path.map(|value| {
        if value.starts_with('/') {
            value
        } else {
            format!("/{value}")
        }
    })
}

pub fn parse_session_headers(
    headers: &HeaderMap,
) -> Result<Option<SessionHeaders>, InvalidRequestError> {
    let Some(session_id) =
        parse_header_string(headers, ALEPHANT_SESSION_ID_HEADER)?
    else {
        return Ok(None);
    };

    let session_path = normalize_session_path(parse_header_string(
        headers,
        ALEPHANT_SESSION_PATH_HEADER,
    )?);
    let session_name =
        parse_header_string(headers, ALEPHANT_SESSION_NAME_HEADER)?;

    Ok(Some(SessionHeaders {
        session_id,
        session_path,
        session_name,
    }))
}

pub fn remove_session_headers(headers: &mut HeaderMap) {
    headers.remove(ALEPHANT_SESSION_ID_HEADER);
    headers.remove(ALEPHANT_SESSION_PATH_HEADER);
    headers.remove(ALEPHANT_SESSION_NAME_HEADER);
}

pub fn inject_session_properties(
    properties: &mut IndexMap<String, String>,
    session: &SessionHeaders,
) {
    properties.insert(
        ALEPHANT_SESSION_ID_PROPERTY.to_string(),
        session.session_id.clone(),
    );
    if let Some(session_path) = &session.session_path {
        properties.insert(
            ALEPHANT_SESSION_PATH_PROPERTY.to_string(),
            session_path.clone(),
        );
    }
    if let Some(session_name) = &session.session_name {
        properties.insert(
            ALEPHANT_SESSION_NAME_PROPERTY.to_string(),
            session_name.clone(),
        );
    }
}
