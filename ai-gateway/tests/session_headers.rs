use ai_gateway::{
    error::invalid_req::InvalidRequestError,
    session_headers::{
        ALEPHANT_SESSION_ID_PROPERTY, ALEPHANT_SESSION_NAME_PROPERTY,
        ALEPHANT_SESSION_PATH_PROPERTY, SessionHeaders,
        inject_session_properties, parse_session_headers,
        remove_session_headers,
    },
};
use http::{HeaderMap, HeaderValue};
use indexmap::IndexMap;

#[test]
fn parse_reads_alephant_session_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "alephant-session-id",
        HeaderValue::from_static("alephant-session"),
    );
    headers.insert(
        "alephant-session-name",
        HeaderValue::from_static("Alephant Flow"),
    );
    headers.insert(
        "alephant-session-path",
        HeaderValue::from_static("root/child"),
    );

    let parsed = parse_session_headers(&headers).expect("parse succeeds");

    assert_eq!(
        parsed,
        Some(SessionHeaders {
            session_id: "alephant-session".to_string(),
            session_path: Some("/root/child".to_string()),
            session_name: Some("Alephant Flow".to_string()),
        })
    );
}

#[test]
fn parse_returns_none_without_nonempty_session_id() {
    let mut headers = HeaderMap::new();
    headers.insert("alephant-session-id", HeaderValue::from_static("   "));
    headers.insert(
        "alephant-session-name",
        HeaderValue::from_static("Session Without Id"),
    );

    let parsed = parse_session_headers(&headers).expect("parse succeeds");

    assert_eq!(parsed, None);
}

#[test]
fn parse_normalizes_session_path_with_leading_slash() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "alephant-session-id",
        HeaderValue::from_static("session-123"),
    );
    headers.insert(
        "alephant-session-path",
        HeaderValue::from_static("workflow/step-1"),
    );

    let parsed = parse_session_headers(&headers).expect("parse succeeds");

    assert_eq!(
        parsed
            .expect("session should exist")
            .session_path
            .as_deref(),
        Some("/workflow/step-1")
    );
}

#[test]
fn parse_rejects_non_utf8_header_values() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "alephant-session-id",
        HeaderValue::from_bytes(&[0x80]).expect("invalid utf8 header"),
    );

    let error = parse_session_headers(&headers).expect_err("parse should fail");

    assert!(matches!(
        error,
        InvalidRequestError::InvalidRequestHeader(_)
    ));
}

#[test]
fn remove_session_headers_removes_all_session_header_aliases() {
    let mut headers = HeaderMap::new();
    headers
        .insert("alephant-session-id", HeaderValue::from_static("session-1"));
    headers.insert(
        "alephant-session-name",
        HeaderValue::from_static("session-name"),
    );
    headers.insert(
        "alephant-session-path",
        HeaderValue::from_static("/workflow"),
    );
    headers.insert("x-keep-me", HeaderValue::from_static("alive"));

    remove_session_headers(&mut headers);

    assert!(!headers.contains_key("alephant-session-id"));
    assert!(!headers.contains_key("alephant-session-name"));
    assert!(!headers.contains_key("alephant-session-path"));
    assert_eq!(
        headers
            .get("x-keep-me")
            .and_then(|value| value.to_str().ok()),
        Some("alive")
    );
}

#[test]
fn inject_session_properties_writes_canonical_alephant_keys() {
    let session = SessionHeaders {
        session_id: "session-123".to_string(),
        session_path: Some("/workflow/child".to_string()),
        session_name: Some("Planner".to_string()),
    };
    let mut properties = IndexMap::from([
        (
            ALEPHANT_SESSION_ID_PROPERTY.to_string(),
            "stale".to_string(),
        ),
        ("alephant-property-custom".to_string(), "keep".to_string()),
    ]);

    inject_session_properties(&mut properties, &session);

    assert_eq!(
        properties
            .get(ALEPHANT_SESSION_ID_PROPERTY)
            .map(String::as_str),
        Some("session-123")
    );
    assert_eq!(
        properties
            .get(ALEPHANT_SESSION_PATH_PROPERTY)
            .map(String::as_str),
        Some("/workflow/child")
    );
    assert_eq!(
        properties
            .get(ALEPHANT_SESSION_NAME_PROPERTY)
            .map(String::as_str),
        Some("Planner")
    );
    assert_eq!(
        properties
            .get("alephant-property-custom")
            .map(String::as_str),
        Some("keep")
    );
}
