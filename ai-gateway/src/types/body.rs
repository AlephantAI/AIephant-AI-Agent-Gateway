use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

pub use axum_core::body::Body;
use bytes::{BufMut, Bytes, BytesMut};
use futures::{Stream, StreamExt};
use hyper::body::{Body as _, Frame, SizeHint};
use tokio::sync::{
    mpsc::{self, UnboundedReceiver},
    oneshot,
};

use crate::error::api::ApiError;

/// When to signal “time to first token” for metrics and request logs.
#[derive(Debug, Clone, Copy)]
pub enum TfftTrigger {
    /// Do not signal; TTFT is reported as **0** (non-streaming bodies).
    Never,
    /// First forwarded body chunk (legacy / generic streams).
    FirstChunk,
    /// First SSE JSON chunk that contains non-empty model text.
    FirstModelToken,
}

/// Reads a stream of HTTP data frames as `Bytes` from a channel.
#[derive(Debug)]
pub struct BodyReader {
    rx: UnboundedReceiver<Bytes>,
    tfft_tx: Option<oneshot::Sender<()>>,
    tfft_trigger: TfftTrigger,
    is_end_stream: bool,
    size_hint: SizeHint,
    append_newlines: bool,
}

impl BodyReader {
    #[must_use]
    pub fn new(
        rx: UnboundedReceiver<Bytes>,
        tfft_tx: oneshot::Sender<()>,
        size_hint: SizeHint,
        append_newlines: bool,
        tfft_trigger: TfftTrigger,
    ) -> Self {
        Self {
            rx,
            tfft_tx: Some(tfft_tx),
            tfft_trigger,
            is_end_stream: false,
            size_hint,
            append_newlines,
        }
    }

    /// `append_newlines` is used to support LLM response logging with Alephant
    /// for streaming responses.
    pub fn wrap_stream(
        stream: impl Stream<Item = Result<Bytes, ApiError>> + Send + 'static,
        append_newlines: bool,
        tfft_trigger: TfftTrigger,
        cache_tap: Option<mpsc::UnboundedSender<Bytes>>,
    ) -> (axum_core::body::Body, BodyReader, oneshot::Receiver<()>) {
        // unbounded channel is okay since we limit memory usage higher in the
        // stack by limiting concurrency and request/response body size.
        let (tx, rx) = mpsc::unbounded_channel();
        let (tfft_tx, tfft_rx) = oneshot::channel();
        let s = stream.map(move |b| {
            if let Ok(chunk) = &b {
                let c = chunk.clone();
                if let Some(ref tap) = cache_tap {
                    let _ = tap.send(c.clone());
                }
                if let Err(e) = tx.send(c) {
                    tracing::error!(error = %e, "BodyReader dropped before stream ended");
                }
            }
            b
        });
        let client_response = axum_core::body::Body::from_stream(s);
        let size_hint = client_response.size_hint();
        let response_body_for_logger =
            BodyReader::new(rx, tfft_tx, size_hint, append_newlines, tfft_trigger);
        (client_response, response_body_for_logger, tfft_rx)
    }
}

impl hyper::body::Body for BodyReader {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.rx).poll_recv(cx) {
            Poll::Ready(Some(bytes)) => {
                let should_signal = match self.tfft_trigger {
                    TfftTrigger::Never => false,
                    TfftTrigger::FirstChunk => self.tfft_tx.is_some(),
                    TfftTrigger::FirstModelToken => {
                        self.tfft_tx.is_some()
                            && crate::logger::first_token::chunk_has_first_model_token(&bytes)
                    }
                };
                if should_signal
                    && let Some(tfft_tx) = self.tfft_tx.take()
                    && let Err(()) = tfft_tx.send(())
                {
                    tracing::error!("Failed to send TFFT signal");
                }

                if self.append_newlines {
                    let mut new_bytes = BytesMut::new();
                    new_bytes.put("data: ".as_bytes());
                    new_bytes.put(bytes);
                    new_bytes.put("\n\n".as_bytes());
                    Poll::Ready(Some(Ok(Frame::data(new_bytes.freeze()))))
                } else {
                    Poll::Ready(Some(Ok(Frame::data(bytes))))
                }
            }
            Poll::Ready(None) => {
                self.is_end_stream = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.is_end_stream
    }

    fn size_hint(&self) -> SizeHint {
        self.size_hint.clone()
    }
}
