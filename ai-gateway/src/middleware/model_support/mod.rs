//! Pre-auth validation: supported provider/model (Redis then DB).

mod parse;
mod service;

pub(crate) use parse::{
    catalog_redis_key, model_field_from_json_body, split_provider_model,
};
pub use service::{ModelSupportLayer, ModelSupportService};
