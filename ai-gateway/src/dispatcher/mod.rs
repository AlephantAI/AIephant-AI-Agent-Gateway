pub mod anthropic_client;
mod bedrock_client;
pub(crate) mod cache_coordinator;
pub mod client;
pub(crate) mod dispatch_logger;
mod extensions;
pub(crate) mod fallback_executor;
pub mod ollama_client;
pub mod openai_compatible_client;
mod provider_allowlist;
pub(crate) mod regional_endpoint;
mod regional_retry_executor;
mod request_builder;
pub mod service;
mod sync_dispatch;
pub(crate) mod target_endpoint;
pub(crate) mod upstream_auth;

use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;

pub use self::service::{Dispatcher, DispatcherService};
use crate::error::api::ApiError;

pub(crate) type BoxTryStream<I> =
    Pin<Box<dyn Stream<Item = Result<I, ApiError>> + Send>>;
pub(crate) type SSEStream = BoxTryStream<Bytes>;
