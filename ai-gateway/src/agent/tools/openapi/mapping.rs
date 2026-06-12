use http::HeaderName;
use serde_json::Value;
use url::Url;

use crate::agent::tools::openapi::types::{
    OpenApiParameterLocation, OpenApiValueSource, RuntimeOpenApiTarget,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiRequestPlan {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Value>,
    pub request_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenApiMappingError {
    #[error("OpenAPI request mapping failed: {0}")]
    MappingFailed(String),
    #[error("OpenAPI request mapping failed: unsupported secret source")]
    UnsupportedSecret,
}

impl OpenApiMappingError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MappingFailed(_) => "mapping_failed",
            Self::UnsupportedSecret => "unsupported_secret",
        }
    }
}

pub fn build_request_plan(
    method: &str,
    target: &RuntimeOpenApiTarget,
    arguments: &Value,
) -> Result<OpenApiRequestPlan, OpenApiMappingError> {
    let method = if method.trim().is_empty() {
        "GET".to_string()
    } else {
        method.trim().to_ascii_uppercase()
    };
    let mut path = target.path_template.clone();
    let mut query = Vec::new();
    let mut headers = Vec::new();

    for mapping in &target.parameter_mapping {
        let Some(value) = resolve_value(&mapping.source, arguments)? else {
            if mapping.required {
                return Err(mapping_failed(format!(
                    "required parameter `{}` is missing",
                    mapping.name
                )));
            }
            continue;
        };

        match mapping.location {
            OpenApiParameterLocation::Path => {
                let placeholder = format!("{{{}}}", mapping.name);
                if !path.contains(&placeholder) {
                    return Err(mapping_failed(format!(
                        "path parameter `{}` has no matching path template \
                         placeholder",
                        mapping.name
                    )));
                }
                let scalar = scalar_to_string(value).ok_or_else(|| {
                    mapping_failed(format!("path parameter `{}` is not scalar", mapping.name))
                })?;
                if contains_forbidden_path_value(&scalar) {
                    return Err(mapping_failed(format!(
                        "path parameter `{}` contains forbidden path \
                         characters",
                        mapping.name
                    )));
                }
                path = path.replace(&placeholder, &percent_encode_path_segment(&scalar));
            }
            OpenApiParameterLocation::Query => {
                let Some(scalar) = scalar_to_string(value) else {
                    if mapping.required {
                        return Err(mapping_failed(format!(
                            "query parameter `{}` is not scalar",
                            mapping.name
                        )));
                    }
                    continue;
                };
                query.push((mapping.name.clone(), scalar));
            }
            OpenApiParameterLocation::Header => {
                let header_name = normalize_header_name(&mapping.name)?;
                let Some(scalar) = scalar_to_string(value) else {
                    if mapping.required {
                        return Err(mapping_failed(format!(
                            "header `{}` is not scalar",
                            mapping.name
                        )));
                    }
                    continue;
                };
                headers.push((header_name, scalar));
            }
        }
    }

    if has_unresolved_path_template_parameter(&path) {
        return Err(mapping_failed(
            "path template contains unresolved parameters",
        ));
    }

    let mut url = Url::parse(&target.base_url)
        .map_err(|err| mapping_failed(format!("invalid base URL: {err}")))?;
    let joined_path = join_base_and_template_path(url.path(), &path);
    url.set_path(&joined_path);
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in &query {
            pairs.append_pair(name, value);
        }
    }

    let body = target
        .request_body_mapping
        .as_ref()
        .map(|mapping| resolve_value(&mapping.source, arguments))
        .transpose()?
        .flatten()
        .cloned();

    let request_bytes = estimate_request_bytes(&method, &url, &headers, body.as_ref());

    Ok(OpenApiRequestPlan {
        method,
        url,
        headers,
        body,
        request_bytes,
    })
}

fn resolve_value<'a>(
    source: &'a OpenApiValueSource,
    arguments: &'a Value,
) -> Result<Option<&'a Value>, OpenApiMappingError> {
    if source.secret_ref.is_some() {
        return Err(OpenApiMappingError::UnsupportedSecret);
    }
    if let Some(literal) = &source.literal {
        return Ok(Some(literal));
    }
    if let Some(path) = &source.argument_path {
        return resolve_argument_path(arguments, path);
    }
    Ok(None)
}

fn resolve_argument_path<'a>(
    arguments: &'a Value,
    path: &str,
) -> Result<Option<&'a Value>, OpenApiMappingError> {
    let Some(rest) = path.strip_prefix("$.") else {
        return Err(mapping_failed(format!(
            "unsupported argument path `{path}`"
        )));
    };
    if rest.is_empty() || rest.split('.').any(str::is_empty) {
        return Err(mapping_failed(format!(
            "unsupported argument path `{path}`"
        )));
    }

    let mut current = arguments;
    for segment in rest.split('.') {
        let Some(next) = current.get(segment) else {
            return Ok(None);
        };
        current = next;
    }
    if current.is_null() {
        Ok(None)
    } else {
        Ok(Some(current))
    }
}

fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn normalize_header_name(name: &str) -> Result<String, OpenApiMappingError> {
    let parsed = HeaderName::from_bytes(name.as_bytes())
        .map_err(|err| mapping_failed(format!("invalid header name `{name}`: {err}")))?;
    let normalized = parsed.as_str().to_string();
    if is_reserved_header(&normalized) {
        return Err(mapping_failed(format!(
            "reserved header `{normalized}` cannot be mapped"
        )));
    }
    Ok(normalized)
}

fn is_reserved_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "authorization"
            | "cookie"
            | "content-length"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-proto"
    )
}

fn contains_forbidden_path_value(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('?')
        || value.contains('#')
        || value.to_ascii_lowercase().contains("%2f")
}

fn has_unresolved_path_template_parameter(path: &str) -> bool {
    path.contains('{') || path.contains('}')
}

fn join_base_and_template_path(base_path: &str, template_path: &str) -> String {
    let base = base_path.trim_end_matches('/');
    let template = template_path.trim_start_matches('/');

    match (base, template) {
        ("", "") => "/".to_string(),
        ("", template) => format!("/{template}"),
        (base, "") => base.to_string(),
        (base, template) => format!("{base}/{template}"),
    }
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn estimate_request_bytes(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: Option<&Value>,
) -> u64 {
    let header_bytes: usize = headers
        .iter()
        .map(|(name, value)| name.len() + value.len() + 4)
        .sum();
    let body_bytes = body.map(serde_json::to_string).transpose().ok().flatten();
    (method.len() + url.as_str().len() + header_bytes + body_bytes.as_ref().map_or(0, String::len))
        as u64
}

fn mapping_failed(message: impl Into<String>) -> OpenApiMappingError {
    OpenApiMappingError::MappingFailed(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::openapi::types::{
        OpenApiBodyMapping, OpenApiParameterLocation, OpenApiParameterMapping, OpenApiValueSource,
        RuntimeOpenApiTarget,
    };

    fn base_target() -> RuntimeOpenApiTarget {
        RuntimeOpenApiTarget {
            base_url: "https://api.example.test".to_string(),
            canonical_host: "api.example.test".to_string(),
            allowed_scheme: "https".to_string(),
            allowed_port: 443,
            path_template: "/v1/tickets".to_string(),
            ..Default::default()
        }
    }

    fn source(path: &str) -> OpenApiValueSource {
        OpenApiValueSource {
            argument_path: Some(path.to_string()),
            ..Default::default()
        }
    }

    fn literal(value: serde_json::Value) -> OpenApiValueSource {
        OpenApiValueSource {
            literal: Some(value),
            ..Default::default()
        }
    }

    #[test]
    fn path_parameter_is_percent_encoded() {
        let mut target = base_target();
        target.path_template = "/v1/tickets/{ticket_id}".to_string();
        target.parameter_mapping = vec![OpenApiParameterMapping {
            location: OpenApiParameterLocation::Path,
            name: "ticket_id".to_string(),
            source: source("$.ticket_id"),
            required: true,
        }];

        let plan = build_request_plan(
            "GET",
            &target,
            &serde_json::json!({
                "ticket_id": "abc 123"
            }),
        )
        .unwrap();

        assert_eq!(
            plan.url.as_str(),
            "https://api.example.test/v1/tickets/abc%20123"
        );
    }

    #[test]
    fn base_url_path_is_preserved_when_joining_template_path() {
        let mut target = base_target();
        target.base_url = "https://api.example.test/api/v1".to_string();
        target.path_template = "/tickets/{ticket_id}".to_string();
        target.parameter_mapping = vec![OpenApiParameterMapping {
            location: OpenApiParameterLocation::Path,
            name: "ticket_id".to_string(),
            source: source("$.ticket_id"),
            required: true,
        }];

        let plan = build_request_plan(
            "GET",
            &target,
            &serde_json::json!({
                "ticket_id": "T-123"
            }),
        )
        .unwrap();

        assert_eq!(
            plan.url.as_str(),
            "https://api.example.test/api/v1/tickets/T-123"
        );
    }

    #[test]
    fn query_scalar_mapping_appends_values() {
        let mut target = base_target();
        target.parameter_mapping = vec![
            OpenApiParameterMapping {
                location: OpenApiParameterLocation::Query,
                name: "status".to_string(),
                source: source("$.filter.status"),
                required: true,
            },
            OpenApiParameterMapping {
                location: OpenApiParameterLocation::Query,
                name: "limit".to_string(),
                source: source("$.limit"),
                required: true,
            },
            OpenApiParameterMapping {
                location: OpenApiParameterLocation::Query,
                name: "debug".to_string(),
                source: literal(serde_json::json!(true)),
                required: false,
            },
        ];

        let plan = build_request_plan(
            "GET",
            &target,
            &serde_json::json!({
                "filter": { "status": "open" },
                "limit": 25
            }),
        )
        .unwrap();

        assert_eq!(
            plan.url.as_str(),
            "https://api.example.test/v1/tickets?status=open&limit=25&debug=true"
        );
    }

    #[test]
    fn header_allowlist_mapping_accepts_non_reserved_headers() {
        let mut target = base_target();
        target.parameter_mapping = vec![OpenApiParameterMapping {
            location: OpenApiParameterLocation::Header,
            name: "x-customer-id".to_string(),
            source: source("$.customer_id"),
            required: true,
        }];

        let plan = build_request_plan(
            "GET",
            &target,
            &serde_json::json!({
                "customer_id": "cust_123"
            }),
        )
        .unwrap();

        assert_eq!(
            plan.headers,
            vec![("x-customer-id".to_string(), "cust_123".to_string())]
        );
    }

    #[test]
    fn json_body_mapping_uses_selected_value() {
        let mut target = base_target();
        target.request_body_mapping = Some(OpenApiBodyMapping {
            source: source("$.payload"),
        });

        let plan = build_request_plan(
            "POST",
            &target,
            &serde_json::json!({
                "payload": {
                    "message": "hello",
                    "urgent": true
                }
            }),
        )
        .unwrap();

        assert_eq!(plan.method, "POST");
        assert_eq!(
            plan.body,
            Some(serde_json::json!({
                "message": "hello",
                "urgent": true
            }))
        );
    }

    #[test]
    fn missing_required_argument_returns_mapping_failed() {
        let mut target = base_target();
        target.parameter_mapping = vec![OpenApiParameterMapping {
            location: OpenApiParameterLocation::Query,
            name: "status".to_string(),
            source: source("$.status"),
            required: true,
        }];

        let err = build_request_plan("GET", &target, &serde_json::json!({})).unwrap_err();

        assert_eq!(err.code(), "mapping_failed");
    }

    #[test]
    fn reserved_headers_are_rejected() {
        for header in [
            "host",
            "authorization",
            "cookie",
            "content-length",
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
            "Authorization",
            "HOST",
            "X-Forwarded-For",
        ] {
            let mut target = base_target();
            target.parameter_mapping = vec![OpenApiParameterMapping {
                location: OpenApiParameterLocation::Header,
                name: header.to_string(),
                source: literal(serde_json::json!("value")),
                required: true,
            }];

            let err = build_request_plan("GET", &target, &serde_json::json!({})).unwrap_err();

            assert_eq!(err.code(), "mapping_failed", "{header}");
        }
    }

    #[test]
    fn path_values_that_can_change_path_or_fragment_are_rejected() {
        for value in [".", "..", "a/b", "a?b", "a#b", "a%2fb", "a%2Fb"] {
            let mut target = base_target();
            target.path_template = "/v1/tickets/{ticket_id}".to_string();
            target.parameter_mapping = vec![OpenApiParameterMapping {
                location: OpenApiParameterLocation::Path,
                name: "ticket_id".to_string(),
                source: literal(serde_json::json!(value)),
                required: true,
            }];

            let err = build_request_plan("GET", &target, &serde_json::json!({})).unwrap_err();

            assert_eq!(err.code(), "mapping_failed", "{value}");
        }
    }

    #[test]
    fn unresolved_path_template_parameter_is_rejected() {
        let mut target = base_target();
        target.path_template = "/v1/tickets/{ticket_id}".to_string();

        let err = build_request_plan("GET", &target, &serde_json::json!({})).unwrap_err();

        assert_eq!(err.code(), "mapping_failed");
    }

    #[test]
    fn path_mapping_without_matching_placeholder_is_rejected() {
        let mut target = base_target();
        target.path_template = "/v1/tickets".to_string();
        target.parameter_mapping = vec![OpenApiParameterMapping {
            location: OpenApiParameterLocation::Path,
            name: "ticket_id".to_string(),
            source: source("$.ticket_id"),
            required: true,
        }];

        let err = build_request_plan(
            "GET",
            &target,
            &serde_json::json!({
                "ticket_id": "T-123"
            }),
        )
        .unwrap_err();

        assert_eq!(err.code(), "mapping_failed");
    }
}
