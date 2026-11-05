use super::upstream_outcome::{MAX_PROBE_BYTES, classify_business_failure};
use bytes::Bytes;
use futures::StreamExt;
use futures::TryStreamExt;
use tokio::sync::{mpsc, oneshot};

use crate::{
    error::{api::ApiError, internal::InternalError},
    types::body::{BodyReader, TfftTrigger},
};

pub(super) type SyncDispatchResponse = (
    http::Response<crate::types::body::Body>,
    crate::types::body::BodyReader,
    oneshot::Receiver<()>,
);

pub(super) async fn dispatch_sync(
    request_builder: &reqwest::RequestBuilder,
    req_body_bytes: Bytes,
    cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    check_business_errors: bool,
) -> Result<SyncDispatchResponse, ApiError> {
    let request_builder = request_builder.try_clone().ok_or_else(|| {
        // in theory, this should never happen, as we'll have already
        // collected the request body
        tracing::error!("failed to clone request builder, cannot dispatch stream");
        ApiError::Internal(InternalError::Internal)
    })?;
    let mut response: reqwest::Response = request_builder
        .body(req_body_bytes)
        .send()
        .await
        .map_err(InternalError::ReqwestError)?;

    let mut status = response.status();
    let mut headers = response.headers().clone();
    let mut prefix = Vec::new();
    let json_response = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            let mime = v
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            mime == "application/json"
                || mime.starts_with("application/") && mime.ends_with("+json")
        });
    if check_business_errors && status.is_success() && json_response {
        let mut length = 0usize;
        let complete = loop {
            match response
                .chunk()
                .await
                .map_err(InternalError::ReqwestError)?
            {
                Some(chunk) => {
                    length = length.saturating_add(chunk.len());
                    prefix.push(chunk);
                    if length > MAX_PROBE_BYTES {
                        break false;
                    }
                }
                None => break true,
            }
        };
        if complete {
            let body: Vec<u8> = prefix
                .iter()
                .flat_map(|chunk| chunk.iter().copied())
                .collect();
            if let Some(failure) = classify_business_failure(&body) {
                tracing::warn!(upstream_status = status.as_u16(), business_failure = ?failure.kind,
                    upstream_code = %failure.code, "upstream returned a business failure");
                headers.insert(
                    "x-alephant-upstream-status",
                    http::HeaderValue::from_str(status.as_str()).expect("valid upstream status"),
                );
                status = failure.status();
                prefix = vec![Bytes::from(
                    serde_json::to_vec(&failure.public_body()).expect("public error JSON"),
                )];
                for name in [
                    "content-length",
                    "etag",
                    "last-modified",
                    "content-encoding",
                    "transfer-encoding",
                ] {
                    headers.remove(name);
                }
                headers.insert(
                    http::header::CONTENT_TYPE,
                    http::HeaderValue::from_static("application/json"),
                );
            }
        }
    }
    let mut resp_builder = http::Response::builder().status(status);
    *resp_builder.headers_mut().unwrap() = headers;
    let (user_resp_body, body_reader, tfft_rx) = BodyReader::wrap_stream(
        futures::stream::iter(prefix.into_iter().map(Ok::<Bytes, ApiError>)).chain(
            response
                .bytes_stream()
                .map_err(|e| InternalError::ReqwestError(e).into()),
        ),
        false,
        TfftTrigger::Never,
        cache_tap,
    );
    let response = resp_builder
        .body(user_resp_body)
        .map_err(InternalError::HttpError)?;
    Ok((response, body_reader, tfft_rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn forward(
        status: u16,
        body: Vec<u8>,
        enabled: bool,
    ) -> http::Response<crate::types::body::Body> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            stream.read(&mut request).await.unwrap();
            let headers = format!(
                "HTTP/1.1 {status} Upstream\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nETag: original\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        dispatch_sync(
            &client.post(format!("http://{address}")),
            Bytes::new(),
            None,
            enabled,
        )
        .await
        .unwrap()
        .0
    }

    #[tokio::test]
    async fn semantic_failure_preserves_actual_upstream_status_and_invalidates_entity_headers() {
        let response = forward(
            201,
            br#"{"error":{"code":"invalid_request","message":"private-value"}}"#.to_vec(),
            true,
        )
        .await;
        assert_eq!(response.status(), 400);
        assert_eq!(response.headers()["x-alephant-upstream-status"], "201");
        assert!(!response.headers().contains_key("etag"));
        assert!(!response.headers().contains_key("content-length"));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&body).contains("private-value"));
    }

    #[tokio::test]
    async fn disabled_checks_preserve_business_fields_and_entity_headers() {
        let original =
            br#"{"error":{"code":"invalid_request","message":"business-data"}}"#.to_vec();
        let response = forward(200, original.clone(), false).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["etag"], "original");
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            original
        );
    }

    #[tokio::test]
    async fn successful_body_larger_than_probe_limit_is_forwarded_without_truncation() {
        let original =
            serde_json::to_vec(&serde_json::json!({"error": "x".repeat(MAX_PROBE_BYTES * 3)}))
                .unwrap();
        let response = forward(200, original.clone(), true).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["etag"], "original");
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            original
        );
    }
}
