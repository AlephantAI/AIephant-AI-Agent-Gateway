//! IDE client ingress adaptation (normalizing request bodies by
//! `ClientProfile`, etc.).
//!
//! Decoupled from generic mapper conversion so per-IDE preprocessing can evolve
//! independently.

pub mod client_profile;
pub mod cursor_responses_openrouter_bridge;
pub mod ide_ingress_adjust;
pub(crate) mod mapper_service_hooks;
pub mod responses_ingress_normalize;
pub(crate) mod responses_strategy;
pub mod responses_upstream_strategy;
pub(crate) mod unified_chat_routing;
pub(crate) mod unified_responses_chat_compat;

pub(crate) use unified_chat_routing::{
    apply_chat_completions_body_redirect_if_needed,
    unified_chat_completions_routing_model,
};
