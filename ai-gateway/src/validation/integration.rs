use super::{PreparedValidation, ValidationError, ValidationPolicy};
use crate::{
    config::Config,
    error::{api::ApiError, init::InitError, invalid_req::InvalidRequestError},
    types::provider::InferenceProvider,
};
use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use http_body_util::BodyExt;
use std::{collections::HashSet, sync::Arc};

pub(crate) fn validate_config(config: &Config) -> Result<(), InitError> {
    let mut paths = HashSet::new();
    for rule in &config.dispatcher.validation_rules {
        if !rule.path.starts_with("v1/")
            || rule.path.contains('?')
            || !paths.insert((rule.provider.clone(), rule.path.clone()))
        {
            return Err(InitError::InvalidPayloadValidation(
                "validation paths must be unique v1 endpoints without queries".into(),
            ));
        }
        ValidationPolicy::for_route(rule.validation.as_ref(), rule.request_contract.as_ref())
            .map_err(|e| InitError::InvalidPayloadValidation(e.to_string()))?;
    }
    for target in &config.agent.tools.targets {
        ValidationPolicy::for_route(target.validation.as_ref(), target.request_contract.as_ref())
            .map_err(|e| InitError::InvalidPayloadValidation(e.to_string()))?;
    }
    Ok(())
}
pub(crate) fn api_error(error: ValidationError, response: bool) -> ApiError {
    let status = if response {
        StatusCode::BAD_GATEWAY
    } else {
        match error.code {
            "validation_busy" | "validation_unavailable" => StatusCode::SERVICE_UNAVAILABLE,
            "body_too_large" | "image_too_large" | "image_work_limit" => {
                StatusCode::PAYLOAD_TOO_LARGE
            }
            "unsupported_image_format" => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            _ => StatusCode::BAD_REQUEST,
        }
    };
    // Error paths, values, decoder diagnostics and input data never enter the public error or logs.
    InvalidRequestError::PayloadValidation {
        status,
        code: error.code,
    }
    .into()
}
pub(crate) async fn prepare_policy(
    policy: ValidationPolicy,
    headers: &mut HeaderMap,
    body: Bytes,
) -> Result<PreparedValidation, ApiError> {
    let incoming = headers.clone();
    policy.apply_upstream_headers(&incoming, headers);
    let permit = if policy.has_media_checks() {
        Some(super::admit().map_err(|e| api_error(e, false))?)
    } else {
        None
    };
    let body = if policy.has_media_checks()
        && headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("multipart/form-data"))
    {
        use base64::Engine;
        let form = crate::endpoints::multipart::Multipart::parse(
            body.clone(),
            headers[http::header::CONTENT_TYPE].to_str().unwrap(),
        )?;
        let mut files = serde_json::Map::new();
        for part in &form.parts {
            if part.filename.is_some() {
                let value = serde_json::json!({"filename":part.filename,"data_base64":base64::engine::general_purpose::STANDARD.encode(&body[part.body.clone()])});
                files
                    .entry(part.name.clone())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                    .as_array_mut()
                    .unwrap()
                    .push(value);
            }
        }
        Bytes::from(
            serde_json::to_vec(&serde_json::json!({"multipart":files}))
                .expect("multipart projection JSON"),
        )
    } else {
        body
    };
    super::prepare(Arc::new(policy), headers.clone(), body, permit)
        .await
        .map_err(|e| api_error(e, false))
}
pub(crate) async fn prepare_endpoint(
    config: &Config,
    provider: &InferenceProvider,
    path: &str,
    headers: &mut HeaderMap,
    body: Bytes,
    is_stream: bool,
) -> Result<Option<PreparedValidation>, ApiError> {
    let path = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_start_matches('/');
    let Some(rule) = config
        .dispatcher
        .validation_rules
        .iter()
        .find(|r| &r.provider == provider && r.path == path)
    else {
        return Ok(None);
    };
    let policy =
        ValidationPolicy::for_route(rule.validation.as_ref(), rule.request_contract.as_ref())
            .map_err(|_| InvalidRequestError::PayloadValidation {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "invalid_validation_config",
            })?;
    let Some(policy) = policy else {
        return Ok(None);
    };
    if is_stream && policy.has_response_checks() {
        return Err(InvalidRequestError::PayloadValidation {
            status: StatusCode::BAD_REQUEST,
            code: "response_validation_requires_non_streaming",
        }
        .into());
    }
    prepare_policy(policy, headers, body).await.map(Some)
}
pub(crate) async fn check_endpoint_response(
    prepared: Option<&PreparedValidation>,
    response: http::Response<crate::types::body::Body>,
) -> Result<http::Response<crate::types::body::Body>, ApiError> {
    let Some(prepared) = prepared.filter(|p| p.policy.has_response_checks()) else {
        return Ok(response);
    };
    if !response.status().is_success() {
        return Ok(response);
    };
    let (parts, body) = response.into_parts();
    let bytes = http_body_util::Limited::new(body, super::MAX_BODY_BYTES)
        .collect()
        .await
        .map_err(|_| InvalidRequestError::PayloadValidation {
            status: StatusCode::BAD_GATEWAY,
            code: "response_body_limit",
        })?
        .to_bytes();
    super::response(prepared, parts.status, bytes.clone())
        .await
        .map_err(|e| api_error(e, true))?;
    Ok(http::Response::from_parts(
        parts,
        crate::types::body::Body::from(bytes),
    ))
}
