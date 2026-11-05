//! OpenAI-compatible media wire handling: no chat conversion or usage estimates.
use crate::{
    error::{api::ApiError, invalid_req::InvalidRequestError},
    types::provider::InferenceProvider,
};
use bytes::Bytes;
use http::{HeaderMap, StatusCode};
#[derive(Debug, Clone)]
pub(crate) struct MediaRequestContext {
    pub model: String,
}
pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub(crate) fn invalid(status: StatusCode, message: &'static str) -> ApiError {
    InvalidRequestError::MediaRequest { status, message }.into()
}
pub(crate) fn is_media_path(path: &str) -> bool {
    matches!(
        path.trim_start_matches('/').trim_start_matches("v1/"),
        "audio/speech" | "audio/transcriptions" | "images/edits"
    )
}
pub(crate) fn supported(provider: &InferenceProvider) -> bool {
    matches!(
        provider,
        InferenceProvider::OpenAI | InferenceProvider::Named(_) | InferenceProvider::Custom
    )
}
pub(crate) fn prepare(
    path: &str,
    body: Bytes,
    headers: &HeaderMap,
    provider: &InferenceProvider,
) -> Result<(String, Bytes), ApiError> {
    if !supported(provider) {
        return Err(InvalidRequestError::UnsupportedEndpoint(
            "media requires an OpenAI-compatible provider".into(),
        )
        .into());
    }
    if body.len() > MAX_REQUEST_BYTES {
        return Err(invalid(
            StatusCode::PAYLOAD_TOO_LARGE,
            "media upload exceeds body limit",
        ));
    }
    let path = path.trim_start_matches('/').trim_start_matches("v1/");
    let content_type = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (model, form) = match path {
        "audio/speech" => {
            if !content_type
                .split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
            {
                return Err(invalid(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "application/json required",
                ));
            }
            let v: serde_json::Value =
                serde_json::from_slice(&body).map_err(InvalidRequestError::InvalidRequestBody)?;
            for key in ["model", "input", "voice"] {
                if !v
                    .get(key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.trim().is_empty())
                {
                    return Err(invalid(
                        StatusCode::BAD_REQUEST,
                        "model, input and voice are required strings",
                    ));
                }
            }
            if v.get("stream").is_some_and(|s| s != false)
                || v.get("stream_format").is_some_and(|s| s != "audio")
            {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "only raw audio speech is supported",
                ));
            }
            (v["model"].as_str().unwrap().to_string(), None)
        }
        "images/edits" => {
            let form = super::multipart::Multipart::parse(body.clone(), content_type)?;
            form.validate_singletons(&["image", "image[]"])?;
            form.unique_text("prompt")?;
            let count = form.file_count("image") + form.file_count("image[]");
            if count == 0 || count > 16 {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "one to sixteen nonempty image files are required",
                ));
            }
            if form.parts.iter().any(|p| {
                matches!(p.name.as_str(), "image" | "image[]" | "mask")
                    && (p.filename.is_none() || p.body.is_empty())
            }) {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "image and mask must be nonempty files",
                ));
            }
            if form.parts.iter().any(|p| p.name == "stream")
                && form.unique_text("stream")? != "false"
            {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "streaming image edits are not supported",
                ));
            }
            (form.unique_text("model")?.to_string(), Some(form))
        }
        "audio/transcriptions" => {
            let form = super::multipart::Multipart::parse(body.clone(), content_type)?;
            form.validate_singletons(&[
                "timestamp_granularities[]",
                "include[]",
                "known_speaker_names[]",
                "known_speaker_references[]",
            ])?;
            if form.file_count("file") != 1 {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "one nonempty audio file is required",
                ));
            }
            if form.parts.iter().any(|p| p.name == "stream")
                && form.unique_text("stream")? != "false"
            {
                return Err(invalid(
                    StatusCode::BAD_REQUEST,
                    "streaming transcription is not supported",
                ));
            }
            (form.unique_text("model")?.to_string(), Some(form))
        }
        _ => return Err(InvalidRequestError::UnsupportedEndpoint(path.into()).into()),
    };
    let model = model.trim().to_string();
    let prefix = format!("{}/", provider.as_provider_code());
    let upstream = model.strip_prefix(&prefix).unwrap_or(&model);
    if upstream.is_empty() {
        return Err(InvalidRequestError::MissingModelId.into());
    }
    let out = if let Some(form) = form {
        form.rewrite_model(upstream)?
    } else {
        let mut v: serde_json::Value =
            serde_json::from_slice(&body).map_err(InvalidRequestError::InvalidRequestBody)?;
        v["model"] = upstream.into();
        Bytes::from(serde_json::to_vec(&v).map_err(InvalidRequestError::InvalidRequestBody)?)
    };
    Ok((model, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_checks_protocol_required_fields_and_bounds_before_sending() {
        let headers = HeaderMap::from_iter([(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        )]);
        let valid =
            Bytes::from_static(br#"{"model":"openai/tts-1","input":"hello","voice":"alloy"}"#);
        assert!(
            prepare(
                "audio/speech",
                valid.clone(),
                &headers,
                &InferenceProvider::Anthropic
            )
            .is_err()
        );
        assert!(
            prepare(
                "audio/speech",
                Bytes::from_static(br#"{"model":"tts-1"}"#),
                &headers,
                &InferenceProvider::OpenAI
            )
            .is_err()
        );
        assert!(
            prepare(
                "audio/speech",
                Bytes::from(vec![0; MAX_REQUEST_BYTES + 1]),
                &headers,
                &InferenceProvider::OpenAI
            )
            .is_err()
        );
        let (model, out) =
            prepare("audio/speech", valid, &headers, &InferenceProvider::OpenAI).unwrap();
        assert_eq!(model, "openai/tts-1");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out).unwrap()["model"],
            "tts-1"
        );
        assert!(
            prepare(
                "audio/transcriptions",
                Bytes::from_static(b"bad"),
                &headers,
                &InferenceProvider::OpenAI
            )
            .is_err()
        );
        let headers = HeaderMap::from_iter([(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("multipart/form-data; boundary=b"),
        )]);
        assert!(prepare("audio/transcriptions",Bytes::from_static(b"--b\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nwhisper-1\r\n--b--\r\n"),&headers,&InferenceProvider::OpenAI).is_err());
    }
}
