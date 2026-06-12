use serde::{Deserialize, Serialize};

pub const JSON_RPC_VERSION: &str = "2.0";
pub const CLIENT_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[CLIENT_PROTOCOL_VERSION];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: String,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpInitializeResult {
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub server_info: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcResponse<T> {
    pub jsonrpc: Option<String>,
    pub id: Option<serde_json::Value>,
    pub result: Option<T>,
    pub error: Option<McpJsonRpcError>,
}

pub fn validate_json_rpc_envelope<T>(
    response: &McpJsonRpcResponse<T>,
    expected_id: &str,
) -> Result<(), JsonRpcProtocolError> {
    if response.jsonrpc.as_deref() != Some(JSON_RPC_VERSION) {
        return Err(JsonRpcProtocolError::InvalidEnvelope);
    }
    if response.id.as_ref() != Some(&serde_json::json!(expected_id)) {
        return Err(JsonRpcProtocolError::InvalidEnvelope);
    }

    Ok(())
}

pub fn validate_supported_protocol_version(
    protocol_version: &str,
) -> Result<(), JsonRpcProtocolError> {
    if SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol_version) {
        return Ok(());
    }

    Err(JsonRpcProtocolError::UnsupportedProtocolVersion)
}

pub fn validate_tools_capability(
    capabilities: &serde_json::Value,
) -> Result<(), JsonRpcProtocolError> {
    if matches!(
        capabilities.get("tools"),
        Some(serde_json::Value::Object(_))
    ) {
        return Ok(());
    }

    Err(JsonRpcProtocolError::ToolsCapabilityMissing)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JsonRpcProtocolError {
    #[error("invalid json-rpc envelope")]
    InvalidEnvelope,
    #[error("unsupported negotiated protocol version")]
    UnsupportedProtocolVersion,
    #[error("mcp tools capability is missing")]
    ToolsCapabilityMissing,
}

pub fn mcp_error_retryable(code: i64) -> bool {
    !matches!(code, -32700 | -32600 | -32601 | -32602)
}
