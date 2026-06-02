// prost/tonic: `include!` pulls generated code from `OUT_DIR`; clippy sees the
// physical path. Generated files are hard to annotate, hence this crate-level
// allow.
#![allow(
    clippy::similar_names,
    clippy::doc_markdown,
    clippy::default_trait_access
)]

pub mod agent;
pub mod app;
pub mod app_redis;
pub mod app_state;
pub mod cli;
pub mod config;
pub mod content_filter;
pub mod default_model;
pub mod policy_proto {
    tonic::include_proto!("policy.v1");
}
pub mod payment_proto {
    tonic::include_proto!("payment.v1");
}
pub mod google {
    pub mod protobuf {
        #[derive(Clone, PartialEq, ::prost::Message)]
        pub struct Timestamp {
            #[prost(int64, tag = "1")]
            pub seconds: i64,
            #[prost(int32, tag = "2")]
            pub nanos: i32,
        }
    }
}
pub mod crypto;
pub mod discover;
pub(crate) mod dispatcher;
pub mod endpoints;
pub mod error;
pub mod fallback;
pub mod ide_adapation;
pub mod llm_kv_cache;
pub mod logger;
pub mod metrics;
pub mod middleware;
pub(crate) mod router;
pub mod semantic_cache;
pub mod session_headers;
pub mod store;
#[cfg(feature = "testing")]
pub mod tests;
pub mod types;
pub mod utils;
pub mod virtual_key;
pub mod x402;

#[cfg(test)]
mod gate_archival;
