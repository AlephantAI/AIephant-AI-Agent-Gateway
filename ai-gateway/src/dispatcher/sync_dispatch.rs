use bytes::Bytes;
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
) -> Result<SyncDispatchResponse, ApiError> {
    let request_builder = request_builder.try_clone().ok_or_else(|| {
        // in theory, this should never happen, as we'll have already
        // collected the request body
        tracing::error!("failed to clone request builder, cannot dispatch stream");
        ApiError::Internal(InternalError::Internal)
    })?;
    let response: reqwest::Response = request_builder
        .body(req_body_bytes)
        .send()
        .await
        .map_err(InternalError::ReqwestError)?;

    let status = response.status();
    let mut resp_builder = http::Response::builder().status(status);
    *resp_builder.headers_mut().unwrap() = response.headers().clone();

    // this is compiled out in release builds
    #[cfg(debug_assertions)]
    if status.is_server_error() || status.is_client_error() {
        let body = response.text().await.map_err(InternalError::ReqwestError)?;
        tracing::debug!(
            status_code = %status,
            error_resp_len = body.len(),
            "received error response"
        );
        let bytes = bytes::Bytes::from(body);
        let stream = futures::stream::once(futures::future::ok::<_, ApiError>(bytes));
        let (error_body, error_reader, tfft_rx) =
            BodyReader::wrap_stream(stream, false, TfftTrigger::Never, cache_tap.clone());
        let response = resp_builder
            .body(error_body)
            .map_err(InternalError::HttpError)?;

        return Ok((response, error_reader, tfft_rx));
    }

    let (user_resp_body, body_reader, tfft_rx) = BodyReader::wrap_stream(
        response
            .bytes_stream()
            .map_err(|e| InternalError::ReqwestError(e).into()),
        false,
        TfftTrigger::Never,
        cache_tap,
    );
    let response = resp_builder
        .body(user_resp_body)
        .map_err(InternalError::HttpError)?;
    Ok((response, body_reader, tfft_rx))
}
