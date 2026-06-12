pub mod json_rpc;
pub mod lifecycle;
pub mod session;
pub mod sse;
pub mod target_hash;
#[cfg(test)]
pub mod test_support;
pub mod transport;

pub async fn execute_mcp_streamable_http_tool(
    ctx: &crate::agent::tools::executor::ToolExecutionContext,
    target: &crate::config::agent::AgentToolTargetConfig,
    request: &crate::agent::tools::types::ToolCallRequest,
    egress_policy: &crate::config::agent::AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
) -> Result<
    crate::agent::tools::types::ToolCallResponse,
    crate::agent::tools::executor::ToolExecutionErrorKind,
> {
    lifecycle::execute_lifecycle(
        ctx,
        target,
        request,
        egress_policy,
        default_timeout_ms,
        max_response_bytes,
    )
    .await
}

#[cfg(test)]
pub async fn execute_mcp_streamable_http_tool_with_cache(
    ctx: &crate::agent::tools::executor::ToolExecutionContext,
    target: &crate::config::agent::AgentToolTargetConfig,
    request: &crate::agent::tools::types::ToolCallRequest,
    egress_policy: &crate::config::agent::AgentToolEgressPolicyConfig,
    default_timeout_ms: u64,
    max_response_bytes: usize,
    cache: &dyn session::McpSessionCache,
) -> Result<
    crate::agent::tools::types::ToolCallResponse,
    crate::agent::tools::executor::ToolExecutionErrorKind,
> {
    lifecycle::execute_lifecycle_with_cache(
        ctx,
        target,
        request,
        egress_policy,
        default_timeout_ms,
        max_response_bytes,
        cache,
    )
    .await
}

#[cfg(test)]
mod tests {
    use http::{HeaderName, HeaderValue, StatusCode};

    use crate::agent::tools::{
        executor::execute_tool_with_context,
        mcp_streamable_http::{
            execute_mcp_streamable_http_tool, execute_mcp_streamable_http_tool_with_cache,
            session::{InMemoryMcpSessionCache, McpSessionCache, session_key},
            test_support::{
                StreamableFixture, initialize_response, initialize_response_for_execution,
                json_rpc_error_response, response_json, sse_raw_response, sse_response,
                status_response, test_context, test_egress_policy, test_request,
                test_session_for_ctx, test_streamable_target, tool_result_response,
                tool_result_response_for_execution,
            },
        },
        types::{ToolCallRequest, ToolExecutionStatus},
    };

    const DEFAULT_TIMEOUT_MS: u64 = 8000;
    const MAX_REQUEST_BYTES: usize = 65_536;
    const MAX_RESPONSE_BYTES: usize = 65_536;

    #[tokio::test]
    async fn streamable_http_initializes_sends_initialized_and_calls_tool_json() {
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response("session-1"),
            response_json(serde_json::json!({})),
            tool_result_response(serde_json::json!({
                "content": [{ "type": "text", "text": "found docs" }]
            })),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);
        let request = test_request();

        let response = execute_tool_with_context(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("streamable http target executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(response.tool_execution_id, "exec-1");
        assert_eq!(response.output["content"][0]["text"], "found docs");
        assert_eq!(response.billing.reason, "success");
        assert!(response.billing.billable);
        let metadata = response
            .gateway_metadata
            .expect("streamable metadata is present");
        assert_eq!(metadata.execution_source, "gateway_executed");
        assert_eq!(metadata.target_kind, "mcp-streamable-http");
        assert_eq!(metadata.target_id, target.tool_id);
        assert_eq!(metadata.target_hash, ctx.target_hash);
        assert_eq!(metadata.auth_revision, ctx.auth_revision);
        assert!(!metadata.cache_hit);
        assert!(!metadata.reinitialized);
        assert_eq!(metadata.protocol_version.as_deref(), Some("2025-06-18"));
        assert!(!metadata.sse_used);
        assert_eq!(metadata.failure_class, None);
        assert!(!metadata.blocked_before_dispatch);
        assert!(metadata.latency_ms.is_some());

        let requests = fixture.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].body_json()["method"], "initialize");
        assert_eq!(
            requests[1].body_json()["method"],
            "notifications/initialized"
        );
        assert_eq!(requests[2].body_json()["method"], "tools/call");
        assert_eq!(
            requests[1].headers[HeaderName::from_static("mcp-session-id")],
            HeaderValue::from_static("session-1")
        );
        assert_eq!(
            requests[2].headers[HeaderName::from_static("mcp-session-id")],
            HeaderValue::from_static("session-1")
        );
        assert_eq!(requests[2].body_json()["params"]["name"], "docs.search");
        assert_eq!(
            requests[2].body_json()["params"]["arguments"]["query"],
            "mcp"
        );
    }

    #[tokio::test]
    async fn streamable_http_initialize_accepts_sse_response() {
        let initialize_event = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init_exec-1",
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": { "name": "streamable-fixture" }
            }
        });
        let Some(fixture) = StreamableFixture::start(vec![
            sse_response(&[&initialize_event.to_string()]).header(
                HeaderName::from_static("mcp-session-id"),
                HeaderValue::from_static("session-sse"),
            ),
            response_json(serde_json::json!({})),
            tool_result_response(serde_json::json!({ "ok": true })),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_tool_with_context(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("sse initialize executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output["ok"], true);
        let metadata = response.gateway_metadata.expect("metadata");
        assert!(metadata.sse_used);
        assert_eq!(metadata.protocol_version.as_deref(), Some("2025-06-18"));
    }

    #[tokio::test]
    async fn streamable_http_tools_call_accepts_sse_response() {
        let tool_event = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "exec_sse",
            "result": {
                "content": [],
                "structuredContent": {
                    "answer": "from-sse"
                },
                "isError": false
            }
        });
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response_for_execution("exec_sse", "session-sse-call"),
            response_json(serde_json::json!({})),
            sse_response(&[&tool_event.to_string()]),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            tool_execution_id: Some("exec_sse".to_string()),
            arguments: serde_json::json!({ "query": "refund" }),
            ..ToolCallRequest::default()
        };
        let ctx = test_context(&target);

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("streamable sse call returns envelope");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output["structuredContent"]["answer"], "from-sse");
        assert!(!response.output.to_string().contains("sseUsed"));
        let metadata = response.gateway_metadata.expect("metadata");
        assert!(metadata.sse_used);
    }

    #[tokio::test]
    async fn streamable_http_business_error_completes_and_remains_billable() {
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response("session-business-error"),
            response_json(serde_json::json!({})),
            tool_result_response(serde_json::json!({
                "content": [{ "type": "text", "text": "business error" }],
                "isError": true
            })),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("business error returns completed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output["isError"], true);
        assert_eq!(response.billing.reason, "tool_business_error");
        assert!(response.billing.billable);
        assert_eq!(response.billing.cost_micros, target.rate_card.fixed_micros);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(metadata.target_kind, "mcp-streamable-http");
        assert_eq!(metadata.failure_class, None);
        assert!(!metadata.blocked_before_dispatch);
    }

    #[tokio::test]
    async fn streamable_http_json_rpc_error_fails_with_explainable_billing() {
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response("session-json-rpc-error"),
            response_json(serde_json::json!({})),
            json_rpc_error_response(-32603, "upstream failed"),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("json-rpc error returns failed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_call_failed")
        );
        assert_eq!(response.billing.reason, "json_rpc_error");
        assert!(!response.billing.billable);
        assert_eq!(response.billing.cost_micros, 0);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(metadata.target_kind, "mcp-streamable-http");
        assert_eq!(metadata.failure_class.as_deref(), Some("mcp_call_failed"));
        assert!(!metadata.blocked_before_dispatch);
    }

    #[tokio::test]
    async fn streamable_http_sse_server_request_returns_failed_response() {
        let server_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "server_1",
            "method": "sampling/createMessage",
            "params": {}
        });
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response_for_execution("exec_server_request", "session-server-request"),
            response_json(serde_json::json!({})),
            sse_response(&[&server_request.to_string()]),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            tool_execution_id: Some("exec_server_request".to_string()),
            arguments: serde_json::json!({}),
            ..ToolCallRequest::default()
        };
        let ctx = test_context(&target);

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("server request maps to failed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("mcp_server_request_unsupported")
        );
        assert!(!response.output.to_string().contains("Mcp-Session-Id"));
        let metadata = response.gateway_metadata.expect("metadata");
        assert!(metadata.sse_used);
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_server_request_unsupported")
        );
    }

    #[tokio::test]
    async fn streamable_http_sse_idle_timeout_returns_failed_envelope() {
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response("session-idle"),
            response_json(serde_json::json!({})),
            sse_raw_response("event: message\n")
                .hold_open_after_body(std::time::Duration::from_millis(500)),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let mut ctx = test_context(&target);
        ctx.mcp_sse_idle_timeout_ms = 25;

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("idle timeout maps to failed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("mcp_sse_idle_timeout")
        );
        assert_eq!(response.billing.reason, "mcp_sse_idle_timeout");
        assert!(!response.billing.billable);
        assert_eq!(response.billing.cost_micros, 0);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_sse_idle_timeout")
        );
        assert!(metadata.sse_used);
    }

    #[tokio::test]
    async fn streamable_http_sse_incomplete_returns_failed_envelope() {
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response("session-incomplete"),
            response_json(serde_json::json!({})),
            sse_response(&[]),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("incomplete stream maps to failed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|err| err.code.as_str()),
            Some("mcp_sse_incomplete")
        );
        assert_eq!(response.billing.reason, "mcp_sse_incomplete");
        assert!(!response.billing.billable);
        assert_eq!(response.billing.cost_micros, 0);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_sse_incomplete")
        );
        assert!(metadata.sse_used);
    }

    #[tokio::test]
    async fn streamable_http_sse_matching_response_stops_reading() {
        let Some(fixture) = StreamableFixture::start(vec![
            initialize_response("session-early"),
            response_json(serde_json::json!({})),
            sse_raw_response(concat!(
                "event: message\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":\"exec-1\",\"result\":",
                "{\"content\":[],\"structuredContent\":{\"answer\":\"early\"},",
                "\"isError\":false}}\n\n",
                ": keepalive\n"
            ))
            .hold_open_after_body(std::time::Duration::from_millis(500)),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_mcp_streamable_http_tool(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            250,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("matching response completes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output["structuredContent"]["answer"], "early");
    }

    #[tokio::test]
    async fn streamable_http_unsupported_protocol_version_fails_before_initialized() {
        let unsupported_initialize = response_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init_exec-1",
            "result": {
                "protocolVersion": "1999-01-01",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": { "name": "legacy-fixture" }
            }
        }))
        .header(
            HeaderName::from_static("mcp-session-id"),
            HeaderValue::from_static("session-legacy"),
        );
        let Some(fixture) = StreamableFixture::start(vec![unsupported_initialize]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_tool_with_context(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("unsupported version maps to failed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_unsupported_protocol_version")
        );
        assert_eq!(response.output["error"]["retryable"], false);
        assert_eq!(response.billing.reason, "mcp_unsupported_protocol_version");
        assert!(!response.billing.billable);
        assert_eq!(response.billing.cost_micros, 0);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_unsupported_protocol_version")
        );
        assert_eq!(metadata.protocol_version.as_deref(), Some("1999-01-01"));
        assert!(!metadata.blocked_before_dispatch);

        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body_json()["method"], "initialize");
    }

    #[tokio::test]
    async fn streamable_http_missing_tools_capability_fails_before_initialized() {
        let missing_tools = response_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init_exec-1",
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": { "name": "no-tools-fixture" }
            }
        }))
        .header(
            HeaderName::from_static("mcp-session-id"),
            HeaderValue::from_static("session-no-tools"),
        );
        let Some(fixture) = StreamableFixture::start(vec![missing_tools]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let response = execute_tool_with_context(
            &ctx,
            &target,
            &test_request(),
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_REQUEST_BYTES,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("missing tools capability maps to failed envelope");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().map(|error| error.code.as_str()),
            Some("mcp_tools_capability_missing")
        );
        assert_eq!(response.output["error"]["retryable"], false);
        assert_eq!(response.billing.reason, "mcp_tools_capability_missing");
        assert!(!response.billing.billable);
        assert_eq!(response.billing.cost_micros, 0);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(
            metadata.failure_class.as_deref(),
            Some("mcp_tools_capability_missing")
        );
        assert_eq!(metadata.protocol_version.as_deref(), Some("2025-06-18"));

        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body_json()["method"], "initialize");
    }

    #[tokio::test]
    async fn cached_session_hit_skips_initialize_and_marks_metadata() {
        let Some(fixture) =
            StreamableFixture::start(vec![tool_result_response(serde_json::json!({
                "content": [{ "type": "text", "text": "cached docs" }]
            }))])
        else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);
        let request = test_request();
        let cache = InMemoryMcpSessionCache::default();
        cache
            .store(
                &session_key(&ctx),
                &test_session_for_ctx(&ctx, "cached-session"),
                600,
            )
            .await;

        let response = execute_mcp_streamable_http_tool_with_cache(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
            &cache,
        )
        .await
        .expect("cached streamable call returns envelope");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output["content"][0]["text"], "cached docs");
        assert_eq!(response.billing.dedupe_key, "exec-1");
        assert_output_hides_internal_metadata(&response.output);
        let metadata = response.gateway_metadata.expect("metadata");
        assert!(metadata.cache_hit);
        assert!(!metadata.reinitialized);

        let requests = fixture.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body_json()["method"], "tools/call");
        assert_eq!(
            requests[0].headers[HeaderName::from_static("mcp-session-id")],
            HeaderValue::from_static("cached-session")
        );
        assert_eq!(
            requests[0].headers[HeaderName::from_static("mcp-protocol-version")],
            HeaderValue::from_static("2025-06-18")
        );
    }

    #[tokio::test]
    async fn cached_session_404_reinitializes_once_with_same_execution_id() {
        let Some(fixture) = StreamableFixture::start(vec![
            status_response(StatusCode::NOT_FOUND),
            initialize_response("fresh-session"),
            response_json(serde_json::json!({})),
            tool_result_response(serde_json::json!({
                "content": [{ "type": "text", "text": "fresh docs" }]
            })),
        ]) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let request = ToolCallRequest {
            tool_id: "docs.search".to_string(),
            tool_call_id: Some("call-1".to_string()),
            tool_execution_id: Some("exec-1".to_string()),
            arguments: serde_json::json!({ "query": "refund" }),
            ..ToolCallRequest::default()
        };
        let ctx = test_context(&target);
        let cache = InMemoryMcpSessionCache::default();
        cache
            .store(
                &session_key(&ctx),
                &test_session_for_ctx(&ctx, "expired-session"),
                600,
            )
            .await;

        let response = execute_mcp_streamable_http_tool_with_cache(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
            &cache,
        )
        .await
        .expect("streamable call returns envelope");

        assert_eq!(response.tool_execution_id, "exec-1");
        assert_eq!(response.billing.dedupe_key, "exec-1");
        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.output["content"][0]["text"], "fresh docs");
        assert_output_hides_internal_metadata(&response.output);
        let metadata = response.gateway_metadata.expect("metadata");
        assert!(!metadata.cache_hit);
        assert!(metadata.reinitialized);
        let refreshed = cache
            .load(&session_key(&ctx))
            .await
            .expect("fresh session is stored");
        assert_eq!(refreshed.session_id, "fresh-session");

        let requests = fixture.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body_json()["method"] == "initialize")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body_json()["method"] == "tools/call")
                .count(),
            2
        );
        assert_eq!(
            requests[0].headers[HeaderName::from_static("mcp-session-id")],
            HeaderValue::from_static("expired-session")
        );
        assert_eq!(
            requests[3].headers[HeaderName::from_static("mcp-session-id")],
            HeaderValue::from_static("fresh-session")
        );
        assert!(requests.iter().all(|request| {
            request.body_json()["id"].as_str() != Some("exec-1")
                || request.body_json()["method"] == "tools/call"
        }));
    }

    #[tokio::test]
    async fn concurrent_cache_miss_initializes_once_and_losers_use_cache() {
        let mut responses = vec![
            initialize_response("shared-session"),
            response_json(serde_json::json!({})),
        ];
        const CONCURRENT_CALLS: usize = 8;
        responses.extend((0..CONCURRENT_CALLS).map(|_| {
            tool_result_response_for_execution(
                "exec-1",
                serde_json::json!({
                    "content": [{ "type": "text", "text": "shared docs" }]
                }),
            )
        }));
        let Some(fixture) = StreamableFixture::start(responses) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let mut joins = Vec::new();
        for _ in 0..CONCURRENT_CALLS {
            let target = target.clone();
            let ctx = ctx.clone();
            joins.push(tokio::spawn(async move {
                execute_mcp_streamable_http_tool(
                    &ctx,
                    &target,
                    &test_request(),
                    &test_egress_policy(),
                    DEFAULT_TIMEOUT_MS,
                    MAX_RESPONSE_BYTES,
                )
                .await
            }));
        }
        for join in joins {
            let response = join.await.expect("join").expect("tool response");
            assert_eq!(response.status, ToolExecutionStatus::Completed);
            assert_output_hides_internal_metadata(&response.output);
        }

        let requests = fixture.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body_json()["method"] == "initialize")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body_json()["method"] == "tools/call")
                .count(),
            CONCURRENT_CALLS
        );
    }

    #[tokio::test]
    async fn concurrent_cache_miss_preserves_sse_initialize_metadata() {
        let initialize_event = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "init_exec-1",
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": { "name": "streamable-fixture" }
            }
        });
        let mut responses = vec![
            sse_response(&[&initialize_event.to_string()]).header(
                HeaderName::from_static("mcp-session-id"),
                HeaderValue::from_static("shared-sse-session"),
            ),
            response_json(serde_json::json!({})),
        ];
        const CONCURRENT_CALLS: usize = 8;
        responses.extend((0..CONCURRENT_CALLS).map(|_| {
            tool_result_response_for_execution(
                "exec-1",
                serde_json::json!({
                    "content": [{ "type": "text", "text": "shared docs" }]
                }),
            )
        }));
        let Some(fixture) = StreamableFixture::start(responses) else {
            return;
        };
        let target = test_streamable_target(fixture.url());
        let ctx = test_context(&target);

        let mut joins = Vec::new();
        for _ in 0..CONCURRENT_CALLS {
            let target = target.clone();
            let ctx = ctx.clone();
            joins.push(tokio::spawn(async move {
                execute_mcp_streamable_http_tool(
                    &ctx,
                    &target,
                    &test_request(),
                    &test_egress_policy(),
                    DEFAULT_TIMEOUT_MS,
                    MAX_RESPONSE_BYTES,
                )
                .await
            }));
        }
        for join in joins {
            let response = join.await.expect("join").expect("tool response");
            assert_eq!(response.status, ToolExecutionStatus::Completed);
            let metadata = response.gateway_metadata.expect("metadata");
            assert!(metadata.sse_used);
            assert_eq!(metadata.protocol_version.as_deref(), Some("2025-06-18"));
        }

        let requests = fixture.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body_json()["method"] == "initialize")
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.body_json()["method"] == "tools/call")
                .count(),
            CONCURRENT_CALLS
        );
    }

    fn assert_output_hides_internal_metadata(output: &serde_json::Value) {
        let output = output.to_string();

        assert!(!output.contains("targetHash"));
        assert!(!output.contains("cacheHit"));
        assert!(!output.contains("Mcp-Session-Id"));
        assert!(!output.contains("Authorization"));
        assert!(!output.contains("data:"));
    }
}
