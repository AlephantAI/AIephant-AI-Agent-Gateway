use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct RuntimeOpenApiTarget {
    pub target_hash: String,
    pub service_slug: String,
    pub operation_id: String,
    pub operation_slug: String,
    pub auth_config_ref: String,
    pub auth_revision: u64,
    pub base_url: String,
    pub canonical_host: String,
    pub allowed_scheme: String,
    pub allowed_port: u16,
    pub path_template: String,
    pub max_response_bytes: u64,
    pub response_content_types: Vec<String>,
    pub parameter_mapping: Vec<OpenApiParameterMapping>,
    pub request_body_mapping: Option<OpenApiBodyMapping>,
}

impl RuntimeOpenApiTarget {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenApiParameterMapping {
    pub location: OpenApiParameterLocation,
    pub name: String,
    pub source: OpenApiValueSource,
    pub required: bool,
}

impl Default for OpenApiParameterMapping {
    fn default() -> Self {
        Self {
            location: OpenApiParameterLocation::Query,
            name: String::new(),
            source: OpenApiValueSource::default(),
            required: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum OpenApiParameterLocation {
    Path,
    #[default]
    Query,
    Header,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenApiBodyMapping {
    pub source: OpenApiValueSource,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct OpenApiValueSource {
    pub argument_path: Option<String>,
    pub literal: Option<serde_json::Value>,
    pub secret_ref: Option<String>,
}
