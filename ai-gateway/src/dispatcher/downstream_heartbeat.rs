//! Opt-in downstream keepalives. Stream state is owned by the response body.
use crate::{
    error::{api::ApiError, internal::InternalError, invalid_req::InvalidRequestError},
    types::{body::Body, response::Response},
};
use bytes::Bytes;
use futures::{Stream, StreamExt, future::BoxFuture};
use http::{StatusCode, header};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, pin::Pin, time::Duration};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct HeartbeatConfig {
    pub interval_ms: u64,
    pub early_sse: bool,
    pub json_whitespace: bool,
    pub max_frame_bytes: usize,
    pub paths: Vec<String>,
}
impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_ms: 15000,
            early_sse: false,
            json_whitespace: false,
            max_frame_bytes: 262144,
            paths: vec!["chat/completions".into(), "responses".into()],
        }
    }
}
impl HeartbeatConfig {
    pub fn validate(&self) -> Result<(), crate::error::init::InitError> {
        if !(10..=60000).contains(&self.interval_ms)
            || !(1024..=4 * 1024 * 1024).contains(&self.max_frame_bytes)
            || self.paths.is_empty()
            || self.paths.iter().any(|p| {
                !matches!(
                    p.as_str(),
                    "chat/completions"
                        | "completions"
                        | "responses"
                        | "embeddings"
                        | "images/generations"
                )
            })
        {
            return Err(crate::error::init::InitError::InvalidPayloadValidation(
                "invalid downstream heartbeat bounds or endpoint".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Sse,
    Json,
}
#[derive(Debug, Clone)]
pub(crate) struct Active {
    pub config: HeartbeatConfig,
    pub mode: Mode,
    pub sent: Option<opentelemetry::metrics::Counter<u64>>,
}
pub(crate) fn select(config: Option<&HeartbeatConfig>, path: &str, body: &[u8]) -> Option<Active> {
    let config = config?;
    if !config.paths.iter().any(|p| p == path) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let stream = value
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mode = if stream {
        Mode::Sse
    } else if config.json_whitespace {
        Mode::Json
    } else {
        return None;
    };
    Some(Active {
        config: config.clone(),
        mode,
        sent: None,
    })
}
fn fail(code: &'static str) -> ApiError {
    InvalidRequestError::PayloadValidation {
        status: StatusCode::BAD_GATEWAY,
        code,
    }
    .into()
}
pub(crate) type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, ApiError>> + Send>>;
struct FrameState {
    source: ByteStream,
    chunk: Bytes,
    pending: Vec<u8>,
    scan: usize,
    line_start: usize,
    limit: usize,
}
impl FrameState {
    fn take_frame(&mut self, eof: bool) -> Option<Bytes> {
        while self.scan < self.pending.len() {
            let i = self.scan;
            let end = match self.pending[i] {
                b'\n' => i + 1,
                b'\r' if i + 1 == self.pending.len() && !eof => return None,
                b'\r' => i + 1 + usize::from(self.pending.get(i + 1) == Some(&b'\n')),
                _ => {
                    self.scan += 1;
                    continue;
                }
            };
            if i == self.line_start {
                let bytes = Bytes::from(self.pending.drain(..end).collect::<Vec<_>>());
                self.scan = 0;
                self.line_start = 0;
                return Some(bytes);
            }
            self.scan = end;
            self.line_start = end;
        }
        None
    }
}
/// Keeps one transport chunk and at most `limit` unfinished frame bytes; polls only on demand.
pub(crate) fn frames(source: ByteStream, limit: usize) -> ByteStream {
    let state = FrameState {
        source,
        chunk: Bytes::new(),
        pending: Vec::new(),
        scan: 0,
        line_start: 0,
        limit,
    };
    Box::pin(futures::stream::try_unfold(state, |mut s| async move {
        loop {
            if let Some(frame) = s.take_frame(false) {
                return Ok(Some((frame, s)));
            }
            if s.pending.len() >= s.limit {
                return Err(fail("sse_frame_limit"));
            }
            if !s.chunk.is_empty() {
                let n = (s.limit - s.pending.len()).min(s.chunk.len());
                s.pending.extend_from_slice(&s.chunk.split_to(n));
                continue;
            }
            match s.source.next().await {
                Some(Ok(bytes)) => s.chunk = bytes,
                Some(Err(e)) => return Err(e),
                None if s.pending.is_empty() => return Ok(None),
                None => {
                    if let Some(frame) = s.take_frame(true) {
                        return Ok(Some((frame, s)));
                    }
                    return Err(fail("incomplete_sse_frame"));
                }
            }
        }
    }))
}
/// Bounded alternative to EventSource's background pump. Does not reconnect or replay calls.
pub(crate) async fn upstream_sse(
    builder: reqwest::RequestBuilder,
    body: impl Into<reqwest::Body>,
    limit: usize,
) -> Result<ByteStream, ApiError> {
    let response = builder
        .body(body)
        .send()
        .await
        .map_err(InternalError::ReqwestError)?;
    if !response.status().is_success() {
        return Err(crate::error::stream::StreamError::UpstreamError {
            status_code: response.status(),
            body: "upstream stream request failed".into(),
        }
        .into());
    }
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            s.split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("text/event-stream"))
        })
    {
        return Err(fail("invalid_sse_content_type"));
    }
    let source = Box::pin(
        response
            .bytes_stream()
            .map(|r| r.map_err(|e| InternalError::ReqwestError(e).into())),
    );
    let frames = frames(source, limit);
    Ok(Box::pin(futures::stream::try_unfold(
        (frames, false),
        |(mut frames, terminal)| async move {
            if terminal {
                return Ok(None);
            }
            while let Some(frame) = frames.next().await {
                let frame = frame?;
                let text = std::str::from_utf8(&frame).map_err(|_| fail("invalid_sse_utf8"))?;
                let data = text
                    .trim_start_matches('\u{feff}')
                    .split(['\r', '\n'])
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if data == "[DONE]" {
                    return Ok(None);
                }
                if !data.is_empty() {
                    let terminal = serde_json::from_str::<serde_json::Value>(&data)
                        .ok()
                        .is_some_and(|v| {
                            v.get("error").is_some()
                                || matches!(
                                    v.get("type").and_then(|v| v.as_str()),
                                    Some(
                                        "response.completed"
                                            | "response.failed"
                                            | "response.incomplete"
                                            | "message_stop"
                                    )
                                )
                        });
                    return Ok(Some((Bytes::from(data), (frames, terminal))));
                }
            }
            Err(fail("upstream_stream_incomplete"))
        },
    )))
}
fn heartbeat(mode: Mode, sent: Option<&opentelemetry::metrics::Counter<u64>>) -> Bytes {
    if let Some(sent) = sent {
        sent.add(1, &[]);
    }
    match mode {
        Mode::Sse => Bytes::from_static(b": alephant heartbeat\n\n"),
        Mode::Json => Bytes::from_static(b"\n"),
    }
}
fn error_json(status: StatusCode) -> Bytes {
    Bytes::from(serde_json::to_vec(&serde_json::json!({"error":{"type":"server_error","code":"upstream_request_failed","message":"The upstream request did not complete successfully."},"upstream_status":status.as_u16()})).expect("error JSON"))
}
fn terminal_sse(status: StatusCode) -> Bytes {
    let mut bytes = b"data: ".to_vec();
    bytes.extend_from_slice(&error_json(status));
    bytes.extend_from_slice(b"\n\ndata: [DONE]\n\n");
    Bytes::from(bytes)
}
fn sse_body(body: Body, active: &Active) -> Body {
    let source = Box::pin(
        body.into_data_stream()
            .map(|r| r.map_err(|e| InternalError::CollectBodyError(e).into())),
    );
    let stream = frames(source, active.config.max_frame_bytes);
    let interval = Duration::from_millis(active.config.interval_ms);
    let sent = active.sent.clone();
    let output = futures::stream::unfold(Some(stream), move |state| {
        let sent = sent.clone();
        async move {
            let mut stream = state?;
            tokio::select! {biased;
             frame=stream.next()=>match frame {
              Some(Ok(frame))=>Some((Ok::<_,Infallible>(frame),Some(stream))),
              Some(Err(_))=>Some((Ok(terminal_sse(StatusCode::BAD_GATEWAY)),None)),
              None=>None,
             },
             _=tokio::time::sleep(interval)=>Some((Ok(heartbeat(Mode::Sse,sent.as_ref())),Some(stream))),
            }
        }
    });
    Body::from_stream(output)
}
fn established(mut response: Response, active: &Active) -> Response {
    if active.mode == Mode::Sse
        && response.status().is_success()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
    {
        let body = std::mem::replace(response.body_mut(), Body::empty());
        *response.body_mut() = sse_body(body, active);
        response.headers_mut().remove(header::CONTENT_LENGTH);
        response
            .headers_mut()
            .insert("x-accel-buffering", http::HeaderValue::from_static("no"));
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            http::HeaderValue::from_static("no-store, no-transform"),
        );
    }
    response
}
async fn final_json(response: Response) -> Bytes {
    let status = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim() == "application/json")
        });
    if !status.is_success() {
        if let Ok(body) = http_body_util::Limited::new(
            response.into_body(),
            super::upstream_outcome::MAX_PROBE_BYTES,
        )
        .collect()
        .await
        {
            if let Some(failure) =
                super::upstream_outcome::classify_business_failure(&body.to_bytes())
            {
                let mut value = failure.public_body();
                value["upstream_status"] = status.as_u16().into();
                return Bytes::from(serde_json::to_vec(&value).expect("classified error JSON"));
            }
        }
        return error_json(status);
    }
    if !is_json {
        return error_json(StatusCode::BAD_GATEWAY);
    }
    match http_body_util::Limited::new(response.into_body(), 32 * 1024 * 1024)
        .collect()
        .await
    {
        Ok(body) => {
            let bytes = body.to_bytes();
            if serde_json::from_slice::<serde_json::Value>(&bytes).is_ok() {
                bytes
            } else {
                error_json(StatusCode::BAD_GATEWAY)
            }
        }
        Err(_) => error_json(StatusCode::BAD_GATEWAY),
    }
}
pub(crate) async fn respond(
    mut future: BoxFuture<'static, Result<Response, ApiError>>,
    active: Active,
) -> Result<Response, ApiError> {
    if active.mode == Mode::Json {
        // The timer covers delayed body bytes too, not only upstream response headers.
        let pending = future;
        future = Box::pin(async move {
            let response = pending.await?;
            let (parts, body) = response.into_parts();
            let body = http_body_util::Limited::new(body, 32 * 1024 * 1024)
                .collect()
                .await
                .map_err(|_| fail("response_body_limit"))?
                .to_bytes();
            Ok(http::Response::from_parts(parts, Body::from(body)))
        });
    }
    let early = active.mode == Mode::Json || active.config.early_sse;
    if !early {
        return future.await.map(|r| established(r, &active));
    }
    let interval = Duration::from_millis(active.config.interval_ms);
    tokio::select! {biased;
     response=&mut future=>return response.map(|r|established(r,&active)),
     _=tokio::time::sleep(interval)=>{}
    }
    // The body owns the pending operation; dropping it cancels I/O and bounded forwarding.
    enum State {
        Waiting(BoxFuture<'static, Result<Response, ApiError>>, bool),
        Sse(ByteStream),
        Done,
    }
    let mode = active.mode;
    let final_active = active.clone();
    let stream = futures::stream::unfold(State::Waiting(future, true), move |state| {
        let active = final_active.clone();
        async move {
            match state {
                State::Waiting(future, true) => Some((
                    Ok::<_, Infallible>(heartbeat(mode, active.sent.as_ref())),
                    State::Waiting(future, false),
                )),
                State::Waiting(mut future, false) => {
                    tokio::select! {biased;
                     response=&mut future=>{
                      let response=match response {Ok(r)=>r,Err(_)=>http::Response::builder().status(502).body(Body::empty()).unwrap()};
                      if mode==Mode::Json {Some((Ok(final_json(response).await),State::Done))}
                      else if !response.status().is_success() {Some((Ok(terminal_sse(response.status())),State::Done))}
                      else {
                       let wrapped=established(response,&active);
                       let mut stream:ByteStream=Box::pin(wrapped.into_body().into_data_stream().map(|r|r.map_err(|e|InternalError::CollectBodyError(e).into())));
                       match stream.next().await {Some(Ok(bytes))=>Some((Ok(bytes),State::Sse(stream))),Some(Err(_))=>Some((Ok(terminal_sse(StatusCode::BAD_GATEWAY)),State::Done)),None=>None}
                      }
                     },
                     _=tokio::time::sleep(interval)=>Some((Ok(heartbeat(mode, active.sent.as_ref())),State::Waiting(future,false))),
                    }
                }
                State::Sse(mut stream) => match stream.next().await {
                    Some(Ok(bytes)) => Some((Ok(bytes), State::Sse(stream))),
                    Some(Err(_)) => Some((Ok(terminal_sse(StatusCode::BAD_GATEWAY)), State::Done)),
                    None => None,
                },
                State::Done => None,
            }
        }
    });
    Ok(http::Response::builder()
        .status(200)
        .header(
            header::CONTENT_TYPE,
            if mode == Mode::Sse {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .header(header::CACHE_CONTROL, "no-store, no-transform")
        .header("x-accel-buffering", "no")
        .header("x-alephant-heartbeat", "true")
        .body(Body::from_stream(stream))
        .expect("static response headers"))
}

#[cfg(test)]
#[path = "downstream_heartbeat_tests.rs"]
mod tests;
