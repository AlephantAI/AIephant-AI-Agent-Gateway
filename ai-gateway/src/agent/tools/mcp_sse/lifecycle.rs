use std::{
    pin::Pin,
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures::{Stream, TryStreamExt};
use http::header;

use crate::{
    agent::tools::{
        egress_policy::validate_target_url,
        executor::{ToolExecutionContext, ToolExecutionErrorKind},
        mcp_sse::endpoint::{extract_endpoint_event, resolve_message_endpoint},
        mcp_streamable_http::{
            json_rpc::{
                CLIENT_PROTOCOL_VERSION, JSON_RPC_VERSION, McpInitializeResult,
                McpJsonRpcNotification, McpJsonRpcRequest, McpJsonRpcResponse, mcp_error_retryable,
                validate_json_rpc_envelope, validate_supported_protocol_version,
                validate_tools_capability,
            },
            sse::{SseAccumulator, SseLimits, SseParseError},
        },
        types::{
            ToolBillingOverride, ToolCallRequest, ToolCallResponse, ToolCost, ToolExecutionEvents,
            ToolExecutionStatus, ToolGatewayMetadata, ToolPolicySummary,
        },
    },
    config::agent::{AgentToolEgressPolicyConfig, AgentToolTargetConfig},
};

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum McpSseFailureClass {
    TargetUnavailable,
    ConnectFailed,
    MessageEndpointInvalid,
    EgressBlocked,
    InitializeFailed,
    CapabilityUnsupported,
    ProtocolError,
    IdleTimeout,
    ResponseTooLarge,
    ServerRequestUnsupported,
    JsonRpcError { retryable: bool },
}

impl McpSseFailureClass {
    const fn error_code(&self) -> &'static str {
        match self {
            Self::TargetUnavailable => "mcp_sse_target_unavailable",
            Self::ConnectFailed => "mcp_sse_connect_failed",
            Self::MessageEndpointInvalid => "mcp_sse_message_endpoint_invalid",
            Self::EgressBlocked => "mcp_sse_egress_blocked",
            Self::InitializeFailed => "mcp_sse_initialize_failed",
            Self::CapabilityUnsupported => "mcp_sse_capability_unsupported",
            Self::ProtocolError => "mcp_sse_protocol_error",
            Self::IdleTimeout => "mcp_sse_idle_timeout",
            Self::ResponseTooLarge => "mcp_sse_response_too_large",
            Self::ServerRequestUnsupported => "mcp_sse_server_request_unsupported",
            Self::JsonRpcError { .. } => "mcp_sse_json_rpc_error",
        }
    }

    const fn billing_reason(&self) -> &'static str {
        match self {
            Self::IdleTimeout => "timeout",
            _ => "failure",
        }
    }

    const fn status(&self) -> ToolExecutionStatus {
        match self {
            Self::IdleTimeout => ToolExecutionStatus::Timeout,
            _ => ToolExecutionStatus::Failed,
        }
    }

    const fn retryable(&self) -> bool {
        match self {
            Self::IdleTimeout | Self::TargetUnavailable | Self::ConnectFailed => true,
            Self::JsonRpcError { retryable } => *retryable,
            _ => false,
        }
    }
}

pub async fn execute_lifecycle(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, ToolExecutionErrorKind> {
    let started = Instant::now();
    let effective_timeout = Duration::from_millis(
        request
            .timeout_ms
            .or(target.timeout_ms)
            .unwrap_or(default_timeout_ms),
    );
    let result = tokio::time::timeout(
        effective_timeout,
        execute_lifecycle_inner(
            ctx,
            target,
            request,
            egress_policy,
            effective_timeout,
            max_response_bytes,
        ),
    )
    .await
    .map_err(|_| McpSseFailureClass::IdleTimeout);

    match result {
        Ok(Ok(mut response)) => {
            if let Some(metadata) = response.gateway_metadata.as_mut() {
                metadata.latency_ms = Some(started.elapsed().as_millis() as u64);
            }
            Ok(response)
        }
        Ok(Err(failure)) | Err(failure) => Ok(failure_response(
            ctx,
            target,
            request,
            failure,
            started.elapsed(),
        )),
    }
}

async fn execute_lifecycle_inner(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    egress_policy: &AgentToolEgressPolicyConfig,
    idle_timeout: Duration,
    max_response_bytes: usize,
) -> Result<ToolCallResponse, McpSseFailureClass> {
    let url = target
        .url
        .as_deref()
        .ok_or(McpSseFailureClass::TargetUnavailable)?;
    validate_target_url(url, egress_policy).map_err(|_| McpSseFailureClass::EgressBlocked)?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| McpSseFailureClass::TargetUnavailable)?;
    let sse_response = client
        .get(url)
        .send()
        .await
        .map_err(|_| McpSseFailureClass::ConnectFailed)?;
    if !sse_response.status().is_success() || !is_sse_content_type(sse_response.headers()) {
        return Err(McpSseFailureClass::TargetUnavailable);
    }

    let mut stream: ByteStream = Box::pin(sse_response.bytes_stream());
    let limits = sse_limits(ctx, max_response_bytes);
    let endpoint = read_endpoint_event(&mut stream, idle_timeout, max_response_bytes).await?;
    let message_url = resolve_message_endpoint(url, &endpoint)
        .map_err(|_| McpSseFailureClass::MessageEndpointInvalid)?;
    validate_target_url(message_url.as_str(), egress_policy)
        .map_err(|_| McpSseFailureClass::EgressBlocked)?;

    let init_id = initialize_request_id(request);
    post_json_rpc(
        &client,
        message_url.as_str(),
        &McpJsonRpcRequest {
            jsonrpc: JSON_RPC_VERSION,
            id: init_id.clone(),
            method: "initialize".to_string(),
            params: serde_json::json!({
                "protocolVersion": CLIENT_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "alephant-ai-gateway",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        },
    )
    .await?;
    let init_value = read_matching_json_rpc(&mut stream, &init_id, idle_timeout, &limits).await?;
    let init_response: McpJsonRpcResponse<McpInitializeResult> =
        serde_json::from_value(init_value).map_err(|_| McpSseFailureClass::ProtocolError)?;
    validate_json_rpc_envelope(&init_response, &init_id)
        .map_err(|_| McpSseFailureClass::ProtocolError)?;
    if init_response.error.is_some() {
        return Err(McpSseFailureClass::InitializeFailed);
    }
    let init_result = init_response
        .result
        .ok_or(McpSseFailureClass::InitializeFailed)?;
    validate_supported_protocol_version(&init_result.protocol_version)
        .map_err(|_| McpSseFailureClass::CapabilityUnsupported)?;
    validate_tools_capability(&init_result.capabilities)
        .map_err(|_| McpSseFailureClass::CapabilityUnsupported)?;

    post_json_rpc(
        &client,
        message_url.as_str(),
        &McpJsonRpcNotification {
            jsonrpc: JSON_RPC_VERSION,
            method: "notifications/initialized".to_string(),
        },
    )
    .await?;

    let call_id = tool_call_request_id(request);
    post_json_rpc(
        &client,
        message_url.as_str(),
        &McpJsonRpcRequest {
            jsonrpc: JSON_RPC_VERSION,
            id: call_id.clone(),
            method: "tools/call".to_string(),
            params: serde_json::json!({
                "name": target.tool_id,
                "arguments": request.arguments,
            }),
        },
    )
    .await?;
    let call_value = read_matching_json_rpc(&mut stream, &call_id, idle_timeout, &limits).await?;
    let call_response: McpJsonRpcResponse<serde_json::Value> =
        serde_json::from_value(call_value).map_err(|_| McpSseFailureClass::ProtocolError)?;
    validate_json_rpc_envelope(&call_response, &call_id)
        .map_err(|_| McpSseFailureClass::ProtocolError)?;
    if let Some(error) = call_response.error {
        return Err(McpSseFailureClass::JsonRpcError {
            retryable: mcp_error_retryable(error.code),
        });
    }
    let result = call_response
        .result
        .ok_or(McpSseFailureClass::ProtocolError)?;

    Ok(success_response(ctx, target, request, result, init_result))
}

fn success_response(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    output: serde_json::Value,
    init_result: McpInitializeResult,
) -> ToolCallResponse {
    let tool_execution_id = request
        .tool_execution_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let cost_micros = target.rate_card.fixed_micros;
    ToolCallResponse {
        status: ToolExecutionStatus::Completed,
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output,
        error: None,
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind: "mcp-sse".to_string(),
            target_id: target.tool_id.clone(),
            target_hash: ctx.target_hash.clone(),
            auth_revision: ctx.auth_revision.clone(),
            cache_hit: false,
            reinitialized: true,
            protocol_version: Some(init_result.protocol_version),
            sse_used: true,
            failure_class: None,
            blocked_before_dispatch: false,
            latency_ms: None,
            billing_status: Some("settled".to_string()),
            billing_reason: Some("success".to_string()),
            executed: Some(true),
            failure_stage: None,
            target_revision: Some(ctx.target_revision),
            schema_hash: None,
            rate_card_revision: Some(0),
            ..ToolGatewayMetadata::default()
        }),
        billing: ToolBillingOverride {
            reason: "success".to_string(),
            billable: true,
            cost_micros,
            currency: target.rate_card.currency.clone(),
            dedupe_key: format!("tool_execution:{tool_execution_id}"),
        },
        cost: ToolCost {
            amount_micros: cost_micros,
            currency: target.rate_card.currency.clone(),
            source: "rate_card".to_string(),
        },
        policy: ToolPolicySummary {
            allowed: true,
            decision: "allowed".to_string(),
            reason: "tool_allowed".to_string(),
        },
        events: ToolExecutionEvents::default(),
    }
}

fn failure_response(
    ctx: &ToolExecutionContext,
    target: &AgentToolTargetConfig,
    request: &ToolCallRequest,
    failure: McpSseFailureClass,
    elapsed: Duration,
) -> ToolCallResponse {
    let tool_execution_id = request
        .tool_execution_id
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let error_code = failure.error_code();
    let retryable = failure.retryable();
    let message = error_message(&failure);

    ToolCallResponse {
        status: failure.status(),
        tool_call_id: request.tool_call_id.clone(),
        tool_execution_id: tool_execution_id.clone(),
        output: serde_json::json!({
            "error": {
                "code": error_code,
                "retryable": retryable,
                "message": message,
            },
            "metadata": {
                "billing": {
                    "reason": failure.billing_reason(),
                    "billable": false,
                    "dedupeKey": format!("tool_execution:{tool_execution_id}"),
                },
                "gateway": {
                    "targetKind": "mcp-sse",
                    "executed": true,
                    "failureStage": "runtime",
                },
            },
        }),
        error: Some(crate::agent::tools::types::ToolExecutionErrorEnvelope {
            code: error_code.to_string(),
            message: message.to_string(),
            retryable,
        }),
        gateway_metadata: Some(ToolGatewayMetadata {
            execution_source: "gateway_executed".to_string(),
            target_kind: "mcp-sse".to_string(),
            target_id: target.tool_id.clone(),
            target_hash: ctx.target_hash.clone(),
            auth_revision: ctx.auth_revision.clone(),
            cache_hit: false,
            reinitialized: false,
            protocol_version: None,
            sse_used: true,
            failure_class: Some(error_code.to_string()),
            blocked_before_dispatch: false,
            latency_ms: Some(elapsed.as_millis() as u64),
            billing_status: Some("waived".to_string()),
            billing_reason: Some(failure.billing_reason().to_string()),
            executed: Some(true),
            failure_stage: Some("runtime".to_string()),
            target_revision: Some(ctx.target_revision),
            schema_hash: None,
            rate_card_revision: Some(0),
            ..ToolGatewayMetadata::default()
        }),
        billing: ToolBillingOverride {
            reason: failure.billing_reason().to_string(),
            billable: false,
            cost_micros: 0,
            currency: target.rate_card.currency.clone(),
            dedupe_key: format!("tool_execution:{tool_execution_id}"),
        },
        cost: ToolCost {
            amount_micros: 0,
            currency: target.rate_card.currency.clone(),
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

const fn error_message(failure: &McpSseFailureClass) -> &'static str {
    match failure {
        McpSseFailureClass::TargetUnavailable => "MCP SSE target is unavailable",
        McpSseFailureClass::ConnectFailed => "MCP SSE target connection failed",
        McpSseFailureClass::MessageEndpointInvalid => "MCP SSE message endpoint is invalid",
        McpSseFailureClass::EgressBlocked => "MCP SSE target was blocked by egress policy",
        McpSseFailureClass::InitializeFailed => "MCP SSE initialize failed",
        McpSseFailureClass::CapabilityUnsupported => "MCP SSE target capabilities are unsupported",
        McpSseFailureClass::ProtocolError => "MCP SSE target returned an invalid protocol response",
        McpSseFailureClass::IdleTimeout => "MCP SSE target timed out while waiting for an event",
        McpSseFailureClass::ResponseTooLarge => "MCP SSE target response exceeded the size limit",
        McpSseFailureClass::ServerRequestUnsupported => {
            "MCP SSE server-initiated requests are unsupported"
        }
        McpSseFailureClass::JsonRpcError { .. } => "MCP SSE tool call returned a JSON-RPC error",
    }
}

async fn post_json_rpc<T: serde::Serialize + ?Sized>(
    client: &reqwest::Client,
    url: &str,
    body: &T,
) -> Result<(), McpSseFailureClass> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|_| McpSseFailureClass::ConnectFailed)?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(McpSseFailureClass::TargetUnavailable)
}

async fn read_endpoint_event(
    stream: &mut ByteStream,
    idle_timeout: Duration,
    max_response_bytes: usize,
) -> Result<String, McpSseFailureClass> {
    let mut buffer = Vec::new();
    let mut total_bytes = 0_usize;
    while let Some(chunk) = next_chunk(stream, idle_timeout).await? {
        total_bytes = total_bytes
            .checked_add(chunk.len())
            .ok_or(McpSseFailureClass::ResponseTooLarge)?;
        if total_bytes > max_response_bytes {
            return Err(McpSseFailureClass::ResponseTooLarge);
        }
        buffer.extend_from_slice(&chunk);
        while let Some(split_at) = completed_event_boundary(&buffer) {
            let event = buffer[..split_at].to_vec();
            buffer = buffer[split_at..].to_vec();
            let text =
                std::str::from_utf8(&event).map_err(|_| McpSseFailureClass::ProtocolError)?;
            if let Ok(endpoint) = extract_endpoint_event(text) {
                return Ok(endpoint);
            }
        }
    }
    Err(McpSseFailureClass::TargetUnavailable)
}

async fn read_matching_json_rpc(
    stream: &mut ByteStream,
    expected_id: &str,
    idle_timeout: Duration,
    limits: &SseLimits,
) -> Result<serde_json::Value, McpSseFailureClass> {
    let mut accumulator = SseAccumulator::default();
    while let Some(chunk) = next_chunk(stream, idle_timeout).await? {
        if let Some(value) = accumulator
            .push_and_try_find(&chunk, expected_id, limits)
            .map_err(map_sse_error)?
        {
            return Ok(value);
        }
    }
    Err(McpSseFailureClass::ProtocolError)
}

async fn next_chunk(
    stream: &mut ByteStream,
    idle_timeout: Duration,
) -> Result<Option<Bytes>, McpSseFailureClass> {
    tokio::time::timeout(idle_timeout, stream.try_next())
        .await
        .map_err(|_| McpSseFailureClass::IdleTimeout)?
        .map_err(|_| McpSseFailureClass::TargetUnavailable)
}

fn map_sse_error(error: SseParseError) -> McpSseFailureClass {
    match error {
        SseParseError::TotalTooLarge
        | SseParseError::EventTooLarge
        | SseParseError::LineTooLarge
        | SseParseError::BatchTooLarge
        | SseParseError::TooManyEvents => McpSseFailureClass::ResponseTooLarge,
        SseParseError::ServerRequestUnsupported => McpSseFailureClass::ServerRequestUnsupported,
        SseParseError::InvalidUtf8
        | SseParseError::InvalidJson
        | SseParseError::MatchingResponseMissing => McpSseFailureClass::ProtocolError,
    }
}

fn completed_event_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|idx| idx + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|idx| idx + 4)
        })
}

fn sse_limits(ctx: &ToolExecutionContext, max_response_bytes: usize) -> SseLimits {
    SseLimits {
        max_total_bytes: max_response_bytes,
        max_event_bytes: ctx.mcp_sse_max_event_bytes,
        max_line_bytes: ctx.mcp_sse_max_line_bytes,
        max_events: ctx.mcp_sse_max_events,
        max_batch_items: ctx.mcp_sse_max_batch_items,
    }
}

fn is_sse_content_type(headers: &http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| {
            content_type
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        })
}

fn initialize_request_id(request: &ToolCallRequest) -> String {
    format!(
        "init-{}",
        request.tool_execution_id.as_deref().unwrap_or("unknown")
    )
}

fn tool_call_request_id(request: &ToolCallRequest) -> String {
    format!(
        "call-{}",
        request.tool_execution_id.as_deref().unwrap_or("unknown")
    )
}
