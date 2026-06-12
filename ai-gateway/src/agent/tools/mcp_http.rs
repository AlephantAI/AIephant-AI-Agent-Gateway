use std::time::Duration;

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    agent::tools::{
        egress_policy::validate_target_url,
        types::{
            ToolBillingOverride, ToolCallRequest, ToolCallResponse, ToolCost,
            ToolExecutionErrorEnvelope, ToolExecutionEvents, ToolExecutionStatus,
            ToolPolicySummary,
        },
    },
    config::agent::{AgentToolEgressPolicyConfig, AgentToolTargetConfig},
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpHttpError {
    #[error("mcp target url is missing")]
    TargetUrlMissing,
    #[error("mcp target is unavailable")]
    TargetUnavailable,
    #[error("mcp initialize request failed")]
    InitializeFailed,
    #[error("mcp server does not support tools capability")]
    CapabilityUnsupported,
    #[error("mcp protocol error")]
    ProtocolError,
    #[error("mcp tool call failed")]
    CallFailed,
    #[error("mcp response exceeds size limit")]
    ResponseTooLarge,
    #[error("mcp request timed out")]
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpInitializeResult {
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub server_info: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpJsonRpcResponse<T> {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub result: Option<T>,
    pub error: Option<McpJsonRpcError>,
}

pub fn validate_initialize_result(response: &McpInitializeResult) -> Result<(), McpHttpError> {
    if matches!(
        response.capabilities.get("tools"),
        Some(serde_json::Value::Object(_))
    ) {
        return Ok(());
    }

    Err(McpHttpError::CapabilityUnsupported)
}

pub fn map_mcp_error(
    tool_execution_id: &str,
    tool_call_id: Option<String>,
    error: McpJsonRpcError,
) -> ToolCallResponse {
    let retryable = mcp_error_retryable(error.code);
    let message = error.message;
    ToolCallResponse {
        status: ToolExecutionStatus::Failed,
        tool_call_id,
        tool_execution_id: tool_execution_id.to_string(),
        output: serde_json::json!({
            "error": {
                "code": "mcp_call_failed",
                "retryable": retryable,
                "message": message,
                "mcpCode": error.code,
                "mcpData": error.data,
            }
        }),
        error: Some(ToolExecutionErrorEnvelope {
            code: "mcp_call_failed".to_string(),
            message,
            retryable,
        }),
        gateway_metadata: None,
        billing: ToolBillingOverride {
            reason: "mcp_call_failed".to_string(),
            billable: false,
            cost_micros: 0,
            currency: "USD".to_string(),
            dedupe_key: tool_execution_id.to_string(),
        },
        cost: ToolCost {
            amount_micros: 0,
            currency: "USD".to_string(),
            source: "waived".to_string(),
        },
        policy: ToolPolicySummary {
            allowed: true,
            decision: "allowed".to_string(),
            reason: "tool_allowed".to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

fn mcp_error_retryable(code: i64) -> bool {
    !matches!(code, -32700 | -32600 | -32601 | -32602)
}

pub async fn execute_mcp_http_tool(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, McpHttpError> {
    if !target.method.eq_ignore_ascii_case("POST") {
        return Err(McpHttpError::TargetUnavailable);
    }
    let url = target
        .url
        .as_deref()
        .ok_or(McpHttpError::TargetUrlMissing)?;
    validate_target_url(url, egress_policy).map_err(|_| McpHttpError::TargetUnavailable)?;
    let parsed = url::Url::parse(url).map_err(|_| McpHttpError::TargetUnavailable)?;

    let timeout_ms =
        effective_timeout_ms(target.timeout_ms, request.timeout_ms, default_timeout_ms);
    let mut client_builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms));
    if !egress_policy.allow_environment_proxy {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .map_err(|_| McpHttpError::TargetUnavailable)?;

    let execution_id = request
        .tool_execution_id
        .clone()
        .unwrap_or_else(|| format!("exec_{}", uuid::Uuid::new_v4().simple()));

    tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        execute_mcp_http_flow(
            target,
            request,
            parsed,
            client,
            execution_id,
            max_response_bytes,
        ),
    )
    .await
    .map_err(|_| McpHttpError::Timeout)?
}

async fn execute_mcp_http_flow(
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    parsed: url::Url,
    client: reqwest::Client,
    execution_id: String,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, McpHttpError> {
    let initialize_request = McpJsonRpcRequest {
        jsonrpc: "2.0",
        id: format!("init_{execution_id}"),
        method: "initialize".to_string(),
        params: serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {
                "name": "alephant-ai-gateway",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    };
    let initialize_http_response =
        post_json_rpc(&client, parsed.clone(), &initialize_request).await?;
    let initialize_response =
        parse_limited_json_response::<McpJsonRpcResponse<McpInitializeResult>>(
            initialize_http_response,
            max_response_bytes,
        )
        .await?;
    validate_json_rpc_envelope(&initialize_response, &initialize_request.id)?;
    if let Some(error) = initialize_response.error {
        return Ok(map_mcp_error(
            &execution_id,
            request.tool_call_id.clone(),
            error,
        ));
    }
    let initialize_result = initialize_response
        .result
        .ok_or(McpHttpError::ProtocolError)?;
    validate_initialize_result(&initialize_result)?;

    let call_request = McpJsonRpcRequest {
        jsonrpc: "2.0",
        id: execution_id.clone(),
        method: "tools/call".to_string(),
        params: serde_json::json!({
            "name": target.tool_id,
            "arguments": request.arguments,
        }),
    };
    let call_http_response = post_json_rpc(&client, parsed, &call_request).await?;
    let call_response = parse_limited_json_response::<McpJsonRpcResponse<Value>>(
        call_http_response,
        max_response_bytes,
    )
    .await?;

    validate_json_rpc_envelope(&call_response, &call_request.id)?;
    if let Some(error) = call_response.error {
        return Ok(map_mcp_error(
            &execution_id,
            request.tool_call_id.clone(),
            error,
        ));
    }
    let output = call_response.result.ok_or(McpHttpError::ProtocolError)?;

    let cost = ToolCost {
        amount_micros: target.rate_card.fixed_micros,
        currency: target.rate_card.currency.clone(),
        source: "rate_card".to_string(),
    };
    Ok(ToolCallResponse {
        status: ToolExecutionStatus::Completed,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: execution_id.clone(),
        output,
        error: None,
        gateway_metadata: None,
        billing: ToolBillingOverride {
            reason: "success".to_string(),
            billable: true,
            cost_micros: cost.amount_micros,
            currency: cost.currency.clone(),
            dedupe_key: execution_id,
        },
        cost,
        policy: ToolPolicySummary {
            allowed: true,
            decision: "allowed".to_string(),
            reason: "tool_allowed".to_string(),
        },
        events: ToolExecutionEvents::default(),
    })
}

fn validate_json_rpc_envelope<T>(
    response: &McpJsonRpcResponse<T>,
    expected_id: &str,
) -> Result<(), McpHttpError> {
    if response.jsonrpc.as_deref() != Some("2.0") {
        return Err(McpHttpError::ProtocolError);
    }
    if response.id.as_ref() != Some(&serde_json::json!(expected_id)) {
        return Err(McpHttpError::ProtocolError);
    }

    Ok(())
}

async fn post_json_rpc(
    client: &reqwest::Client,
    url: url::Url,
    body: &McpJsonRpcRequest,
) -> Result<reqwest::Response, McpHttpError> {
    let response = client.post(url).json(body).send().await.map_err(|err| {
        if err.is_timeout() {
            McpHttpError::Timeout
        } else {
            McpHttpError::TargetUnavailable
        }
    })?;
    if !response.status().is_success() {
        return Err(McpHttpError::CallFailed);
    }

    Ok(response)
}

async fn parse_limited_json_response<T>(
    response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<T, McpHttpError>
where
    T: for<'de> Deserialize<'de>,
{
    if response
        .content_length()
        .is_some_and(|len| len > max_response_bytes as u64)
    {
        return Err(McpHttpError::ResponseTooLarge);
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.try_next().await.map_err(|err| {
        if err.is_timeout() {
            McpHttpError::Timeout
        } else {
            McpHttpError::TargetUnavailable
        }
    })? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or(McpHttpError::ResponseTooLarge)?;
        if next_len > max_response_bytes {
            return Err(McpHttpError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body).map_err(|_| McpHttpError::ProtocolError)
}

fn effective_timeout_ms(
    target_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
    default_timeout_ms: u64,
) -> u64 {
    let configured_timeout_ms = target_timeout_ms.unwrap_or(default_timeout_ms).max(1);
    request_timeout_ms
        .map(|request_timeout_ms| request_timeout_ms.max(1).min(configured_timeout_ms))
        .unwrap_or(configured_timeout_ms)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
    };

    use super::*;
    use crate::{
        agent::tools::types::ToolCallRequest,
        config::agent::{
            AgentToolEgressPolicyConfig, AgentToolRateCardConfig, AgentToolTargetConfig,
            AgentToolTargetKind,
        },
    };

    const DEFAULT_MCP_TIMEOUT_MS: u64 = 8000;

    #[test]
    fn mcp_error_maps_to_retryable_tool_error() {
        let error = McpJsonRpcError {
            code: -32603,
            message: "server exploded".to_string(),
            data: Some(serde_json::json!({"detail": "x"})),
        };

        let mapped = map_mcp_error("exec_1", Some("call_1".to_string()), error);

        assert_eq!(
            mapped.status,
            crate::agent::tools::types::ToolExecutionStatus::Failed
        );
        assert_eq!(mapped.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(mapped.tool_execution_id, "exec_1");
        assert_eq!(mapped.output["error"]["code"], "mcp_call_failed");
        assert_eq!(mapped.output["error"]["retryable"], true);
        assert_eq!(mapped.output["error"]["mcpCode"], -32603);
        assert_eq!(mapped.output["error"]["mcpData"]["detail"], "x");
        assert_eq!(mapped.output["error"]["message"], "server exploded");
        assert_eq!(mapped.cost.amount_micros, 0);
        assert_eq!(mapped.cost.currency, "USD");
        assert_eq!(mapped.cost.source, "waived");
        assert_eq!(mapped.policy.decision, "allowed");
        assert_eq!(mapped.policy.reason, "tool_allowed");
    }

    #[test]
    fn mcp_standard_request_errors_are_not_retryable() {
        let error = McpJsonRpcError {
            code: -32602,
            message: "invalid params".to_string(),
            data: None,
        };

        let mapped = map_mcp_error("exec_1", Some("call_1".to_string()), error);

        assert_eq!(mapped.output["error"]["retryable"], false);
        assert_eq!(mapped.output["error"]["mcpCode"], -32602);
    }

    #[test]
    fn initialize_capabilities_require_tools() {
        let response = McpInitializeResult {
            protocol_version: "2025-03-26".to_string(),
            capabilities: serde_json::json!({"tools": {}}),
            server_info: serde_json::json!({"name": "mock-mcp"}),
        };

        validate_initialize_result(&response).expect("tools capability is supported");
    }

    #[test]
    fn initialize_without_tools_capability_is_rejected() {
        let response = McpInitializeResult {
            protocol_version: "2025-03-26".to_string(),
            capabilities: serde_json::json!({"resources": {}}),
            server_info: serde_json::json!({"name": "mock-mcp"}),
        };

        let err = validate_initialize_result(&response).expect_err("tools capability is required");

        assert_eq!(err, McpHttpError::CapabilityUnsupported);
    }

    #[test]
    fn initialize_rejects_non_object_tools_capability() {
        let response = McpInitializeResult {
            protocol_version: "2025-03-26".to_string(),
            capabilities: serde_json::json!({"tools": false}),
            server_info: serde_json::json!({"name": "mock-mcp"}),
        };

        let err =
            validate_initialize_result(&response).expect_err("tools capability must be an object");

        assert_eq!(err, McpHttpError::CapabilityUnsupported);
    }

    #[test]
    fn json_rpc_request_serializes_required_field_shape() {
        let request = McpJsonRpcRequest {
            jsonrpc: "2.0",
            id: "init_1".to_string(),
            method: "initialize".to_string(),
            params: serde_json::json!({"protocolVersion": "2025-03-26"}),
        };

        let value = serde_json::to_value(request).expect("json-rpc request should serialize");

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], "init_1");
        assert_eq!(value["method"], "initialize");
        assert_eq!(value["params"]["protocolVersion"], "2025-03-26");
    }

    #[test]
    fn initialize_result_defaults_optional_json_objects() {
        let response: McpInitializeResult =
            serde_json::from_value(serde_json::json!({"protocolVersion": "2025-03-26"}))
                .expect("initialize result should default missing objects");

        assert_eq!(response.protocol_version, "2025-03-26");
        assert_eq!(response.capabilities, serde_json::Value::Null);
        assert_eq!(response.server_info, serde_json::Value::Null);
    }

    #[test]
    fn json_rpc_envelope_requires_version_and_matching_id() {
        let response = McpJsonRpcResponse::<serde_json::Value> {
            jsonrpc: Some("2.0".to_string()),
            id: Some(serde_json::json!("exec_1")),
            result: Some(serde_json::json!({ "ok": true })),
            error: None,
        };

        validate_json_rpc_envelope(&response, "exec_1")
            .expect("matching JSON-RPC envelope is valid");
    }

    #[test]
    fn json_rpc_envelope_rejects_missing_jsonrpc() {
        let response = McpJsonRpcResponse::<serde_json::Value> {
            jsonrpc: None,
            id: Some(serde_json::json!("exec_1")),
            result: Some(serde_json::json!({ "ok": true })),
            error: None,
        };

        let err = validate_json_rpc_envelope(&response, "exec_1")
            .expect_err("missing jsonrpc is invalid");

        assert_eq!(err, McpHttpError::ProtocolError);
    }

    #[test]
    fn json_rpc_envelope_rejects_mismatched_id() {
        let response = McpJsonRpcResponse::<serde_json::Value> {
            jsonrpc: Some("2.0".to_string()),
            id: Some(serde_json::json!("other_exec")),
            result: Some(serde_json::json!({ "ok": true })),
            error: None,
        };

        let err =
            validate_json_rpc_envelope(&response, "exec_1").expect_err("mismatched id is invalid");

        assert_eq!(err, McpHttpError::ProtocolError);
    }

    #[tokio::test]
    async fn mcp_http_target_initializes_and_calls_tool() {
        let Some(fixture) = McpHttpFixture::start() else {
            return;
        };
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            name: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpHttp,
            url: Some(fixture.url()),
            rate_card: AgentToolRateCardConfig {
                fixed_micros: 375,
                currency: "USD".to_string(),
            },
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            tool_call_id: Some("call_1".to_string()),
            tool_execution_id: Some("exec_1".to_string()),
            arguments: serde_json::json!({ "query": "refund policy" }),
            ..ToolCallRequest::default()
        };
        let egress_policy = AgentToolEgressPolicyConfig {
            https_only: false,
            block_loopback: false,
            ..AgentToolEgressPolicyConfig::default()
        };

        let response = execute_mcp_http_tool(
            &target,
            &request,
            &egress_policy,
            DEFAULT_MCP_TIMEOUT_MS,
            65_536,
        )
        .await
        .expect("mcp http target executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.tool_execution_id, "exec_1");
        assert_eq!(response.output["content"][0]["text"], "doc result");
        assert_eq!(response.cost.amount_micros, 375);
        assert_eq!(response.cost.currency, "USD");
        assert_eq!(response.cost.source, "rate_card");
    }

    #[tokio::test]
    async fn mcp_http_rejects_non_post_target_method() {
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            name: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpHttp,
            method: "GET".to_string(),
            url: Some("https://mcp.example.com/mcp".to_string()),
            ..AgentToolTargetConfig::default()
        };

        let err = execute_mcp_http_tool(
            &target,
            &test_mcp_request("call_method", "exec_method"),
            &AgentToolEgressPolicyConfig::default(),
            DEFAULT_MCP_TIMEOUT_MS,
            65_536,
        )
        .await
        .expect_err("mcp-http only supports POST targets");

        assert_eq!(err, McpHttpError::TargetUnavailable);
    }

    #[tokio::test]
    async fn mcp_http_uses_tool_id_as_upstream_tool_name() {
        let Some(fixture) = McpHttpFixture::start_asserting_tool_name("docs.search") else {
            return;
        };
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            name: "Search Docs Display Name".to_string(),
            kind: AgentToolTargetKind::McpHttp,
            url: Some(fixture.url()),
            ..AgentToolTargetConfig::default()
        };

        let response = execute_mcp_http_tool(
            &target,
            &test_mcp_request("call_name", "exec_name"),
            &test_egress_policy(),
            DEFAULT_MCP_TIMEOUT_MS,
            65_536,
        )
        .await
        .expect("mcp http target executes with published upstream name");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(
            response.output["structuredContent"]["toolName"],
            "docs.search"
        );
    }

    #[tokio::test]
    async fn mcp_http_initialize_error_preserves_tool_call_id() {
        let Some(fixture) = McpHttpFixture::start_with_initialize_error() else {
            return;
        };
        let target = AgentToolTargetConfig {
            tool_id: "docs.search".to_string(),
            name: "docs.search".to_string(),
            kind: AgentToolTargetKind::McpHttp,
            url: Some(fixture.url()),
            ..AgentToolTargetConfig::default()
        };
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            tool_call_id: Some("call_init".to_string()),
            tool_execution_id: Some("exec_init".to_string()),
            arguments: serde_json::json!({ "query": "refund policy" }),
            ..ToolCallRequest::default()
        };
        let egress_policy = AgentToolEgressPolicyConfig {
            https_only: false,
            block_loopback: false,
            ..AgentToolEgressPolicyConfig::default()
        };

        let response = execute_mcp_http_tool(
            &target,
            &request,
            &egress_policy,
            DEFAULT_MCP_TIMEOUT_MS,
            65_536,
        )
        .await
        .expect("initialize json-rpc error maps to tool response");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(response.tool_call_id.as_deref(), Some("call_init"));
        assert_eq!(response.tool_execution_id, "exec_init");
        assert_eq!(response.output["error"]["code"], "mcp_call_failed");
        assert_eq!(response.output["error"]["mcpCode"], -32603);
        assert_eq!(response.output["error"]["message"], "init failed");
        assert_eq!(response.cost.source, "waived");
    }

    #[tokio::test]
    async fn mcp_http_rejects_initialize_response_without_jsonrpc() {
        let Some(fixture) = McpHttpFixture::start_with_initialize_response(serde_json::json!({
            "id": "init_exec_bad_init",
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mock-mcp" }
            }
        })) else {
            return;
        };

        let err = execute_mcp_http_tool(
            &test_mcp_target(&fixture, "docs.search"),
            &test_mcp_request("call_bad_init", "exec_bad_init"),
            &test_egress_policy(),
            DEFAULT_MCP_TIMEOUT_MS,
            65_536,
        )
        .await
        .expect_err("missing jsonrpc is a protocol error");

        assert_eq!(err, McpHttpError::ProtocolError);
    }

    #[tokio::test]
    async fn mcp_http_rejects_call_response_with_mismatched_id() {
        let Some(fixture) = McpHttpFixture::start_with_call_response(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "other_exec",
            "result": {
                "content": [
                    { "type": "text", "text": "doc result" }
                ]
            }
        })) else {
            return;
        };

        let err = execute_mcp_http_tool(
            &test_mcp_target(&fixture, "docs.search"),
            &test_mcp_request("call_bad_call", "exec_bad_call"),
            &test_egress_policy(),
            DEFAULT_MCP_TIMEOUT_MS,
            65_536,
        )
        .await
        .expect_err("mismatched id is a protocol error");

        assert_eq!(err, McpHttpError::ProtocolError);
    }

    #[tokio::test]
    async fn mcp_http_enforces_overall_execution_timeout() {
        let Some(fixture) = McpHttpFixture::start_with_delayed_success(
            Duration::from_millis(40),
            Duration::from_millis(40),
        ) else {
            return;
        };

        let err = execute_mcp_http_tool(
            &test_mcp_target(&fixture, "docs.search"),
            &test_mcp_request("call_timeout", "exec_timeout"),
            &test_egress_policy(),
            60,
            65_536,
        )
        .await
        .expect_err("combined initialize and call budget should time out");

        assert_eq!(err, McpHttpError::Timeout);
    }

    struct McpHttpFixture {
        url: String,
    }

    impl McpHttpFixture {
        fn start() -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping MCP HTTP loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind test server: {err}"),
            };
            let addr = listener.local_addr().expect("test server addr");
            thread::spawn(move || {
                let initialize_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "init_exec_1",
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "mock-mcp" }
                    }
                });
                serve_json_response(&listener, initialize_body);

                let tool_call_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "exec_1",
                    "result": {
                        "content": [
                            { "type": "text", "text": "doc result" }
                        ]
                    }
                });
                serve_json_response(&listener, tool_call_body);
            });

            Some(Self {
                url: format!("http://{addr}/mcp"),
            })
        }

        fn start_with_initialize_error() -> Option<Self> {
            Self::start_with_initialize_response(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "init_exec_init",
                "error": {
                    "code": -32603,
                    "message": "init failed",
                    "data": { "stage": "initialize" }
                }
            }))
        }

        fn start_with_initialize_response(initialize_body: serde_json::Value) -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping MCP HTTP loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind test server: {err}"),
            };
            let addr = listener.local_addr().expect("test server addr");
            thread::spawn(move || {
                serve_json_response(&listener, initialize_body);
            });

            Some(Self {
                url: format!("http://{addr}/mcp"),
            })
        }

        fn start_with_call_response(call_body: serde_json::Value) -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping MCP HTTP loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind test server: {err}"),
            };
            let addr = listener.local_addr().expect("test server addr");
            thread::spawn(move || {
                let initialize_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "init_exec_bad_call",
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "mock-mcp" }
                    }
                });
                serve_json_response(&listener, initialize_body);
                serve_json_response(&listener, call_body);
            });

            Some(Self {
                url: format!("http://{addr}/mcp"),
            })
        }

        fn start_asserting_tool_name(expected_tool_name: &str) -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping MCP HTTP loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind test server: {err}"),
            };
            let expected_tool_name = expected_tool_name.to_string();
            let addr = listener.local_addr().expect("test server addr");
            thread::spawn(move || {
                let initialize_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "init_exec_name",
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "mock-mcp" }
                    }
                });
                serve_json_response(&listener, initialize_body);

                let (mut stream, _) = listener.accept().expect("accept tools/call request");
                let request: serde_json::Value =
                    serde_json::from_str(&read_http_request_body(&mut stream))
                        .expect("tools/call request JSON");
                let tool_name = request["params"]["name"]
                    .as_str()
                    .expect("tools/call name should be a string");
                assert_eq!(tool_name, expected_tool_name);
                let tool_call_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "exec_name",
                    "result": {
                        "content": [
                            { "type": "text", "text": "doc result" }
                        ],
                        "structuredContent": {
                            "toolName": tool_name
                        }
                    }
                });
                write_json_response(&mut stream, tool_call_body);
            });

            Some(Self {
                url: format!("http://{addr}/mcp"),
            })
        }

        fn start_with_delayed_success(
            initialize_delay: Duration,
            call_delay: Duration,
        ) -> Option<Self> {
            let listener = match TcpListener::bind("127.0.0.1:0") {
                Ok(listener) => listener,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("skipping MCP HTTP loopback fixture: {err}");
                    return None;
                }
                Err(err) => panic!("bind test server: {err}"),
            };
            let addr = listener.local_addr().expect("test server addr");
            thread::spawn(move || {
                let initialize_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "init_exec_timeout",
                    "result": {
                        "protocolVersion": "2025-03-26",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "mock-mcp" }
                    }
                });
                serve_delayed_json_response(&listener, initialize_body, initialize_delay);

                let tool_call_body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "exec_timeout",
                    "result": {
                        "content": [
                            { "type": "text", "text": "doc result" }
                        ]
                    }
                });
                serve_delayed_json_response(&listener, tool_call_body, call_delay);
            });

            Some(Self {
                url: format!("http://{addr}/mcp"),
            })
        }

        fn url(&self) -> String {
            self.url.clone()
        }
    }

    fn test_mcp_target(fixture: &McpHttpFixture, name: &str) -> AgentToolTargetConfig {
        AgentToolTargetConfig {
            tool_id: name.to_string(),
            name: name.to_string(),
            kind: AgentToolTargetKind::McpHttp,
            url: Some(fixture.url()),
            ..AgentToolTargetConfig::default()
        }
    }

    fn test_mcp_request(tool_call_id: &str, tool_execution_id: &str) -> ToolCallRequest {
        ToolCallRequest {
            tool_id: "docs.search".to_string(),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_execution_id: Some(tool_execution_id.to_string()),
            arguments: serde_json::json!({ "query": "refund policy" }),
            ..ToolCallRequest::default()
        }
    }

    fn test_egress_policy() -> AgentToolEgressPolicyConfig {
        AgentToolEgressPolicyConfig {
            https_only: false,
            block_loopback: false,
            ..AgentToolEgressPolicyConfig::default()
        }
    }

    fn serve_json_response(listener: &TcpListener, body: serde_json::Value) {
        serve_delayed_json_response(listener, body, Duration::ZERO);
    }

    fn serve_delayed_json_response(
        listener: &TcpListener,
        body: serde_json::Value,
        delay: Duration,
    ) {
        let (mut stream, _) = listener.accept().expect("accept mcp request");
        let _request_body = read_http_request_body(&mut stream);
        thread::sleep(delay);
        let response_body = serde_json::to_string(&body).expect("response JSON");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: \
             application/json\r\nContent-Length: {}\r\nConnection: \
             close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write mcp response");
    }

    fn write_json_response(stream: &mut TcpStream, body: serde_json::Value) {
        let response_body = serde_json::to_string(&body).expect("response JSON");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: \
             application/json\r\nContent-Length: {}\r\nConnection: \
             close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write mcp response");
    }

    fn read_http_request_body(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).expect("read mcp request");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(body) = body_from_complete_request(&bytes) {
                return body;
            }
        }
        String::new()
    }

    fn body_from_complete_request(bytes: &[u8]) -> Option<String> {
        let request = String::from_utf8_lossy(bytes);
        let (headers, body) = request.split_once("\r\n\r\n")?;
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })?;
        (body.len() >= content_length).then(|| body[..content_length].to_string())
    }
}
