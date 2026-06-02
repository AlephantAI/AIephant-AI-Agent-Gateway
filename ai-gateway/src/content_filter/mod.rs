//! gRPC client for policy-service `policy.v1.PolicyService/Evaluate`.

mod client;
pub mod evaluate;
pub use evaluate::{ContentFilterForwardBody, ContentFilterResult, PolicyModelOverride};
pub(crate) mod estimate;
pub mod piicache;
pub mod prompt_cache;
mod reconnect;
mod request;

pub use client::{ContentFilterClientHolder, ContentFilterGrpcClient};
pub(crate) use reconnect::spawn_content_filter_reconnect_task;
