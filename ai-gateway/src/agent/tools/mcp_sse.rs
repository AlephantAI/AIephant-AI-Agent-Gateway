pub mod endpoint;
pub mod lifecycle;
pub mod target_hash;
#[cfg(test)]
pub mod test_support;
pub mod transport;

pub async fn execute_mcp_sse_tool(
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
mod tests {
    use crate::agent::tools::{
        executor::execute_tool_with_context,
        mcp_sse::test_support::{
            McpSseFixture, sse_json_rpc_response, sse_raw_data, test_context, test_egress_policy,
            test_mcp_sse_target, test_request,
        },
        mcp_streamable_http::json_rpc::CLIENT_PROTOCOL_VERSION,
        types::ToolExecutionStatus,
    };

    const DEFAULT_TIMEOUT_MS: u64 = 8000;
    const MAX_REQUEST_BYTES: usize = 65_536;
    const MAX_RESPONSE_BYTES: usize = 65_536;

    #[tokio::test]
    async fn mcp_sse_initializes_and_calls_tool() {
        let Some(fixture) = McpSseFixture::start(vec![
            sse_json_rpc_response(
                "init-exec-1",
                serde_json::json!({
                    "protocolVersion": CLIENT_PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fixture", "version": "1"}
                }),
            ),
            sse_json_rpc_response(
                "call-exec-1",
                serde_json::json!({
                    "content": [{"type": "text", "text": "found docs"}],
                    "isError": false
                }),
            ),
        ]) else {
            return;
        };
        let target = test_mcp_sse_target(fixture.sse_url());
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
        .expect("mcp sse target executes");

        assert_eq!(response.status, ToolExecutionStatus::Completed);
        assert_eq!(response.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(response.tool_execution_id, "exec-1");
        assert_eq!(response.output["content"][0]["text"], "found docs");
        assert_eq!(response.billing.reason, "success");
        assert!(response.billing.billable);
        let metadata = response.gateway_metadata.expect("metadata");
        assert_eq!(metadata.target_kind, "mcp-sse");
        assert_eq!(metadata.execution_source, "gateway_executed");
        assert_eq!(metadata.sse_used, true);
        assert_eq!(metadata.blocked_before_dispatch, false);
    }

    #[tokio::test]
    async fn mcp_sse_idle_timeout_returns_timeout_response() {
        let Some(fixture) = McpSseFixture::start_without_call_response() else {
            return;
        };
        let target = test_mcp_sse_target(fixture.sse_url());
        let ctx = test_context(&target);
        let mut request = test_request();
        request.timeout_ms = Some(250);

        let response = super::execute_mcp_sse_tool(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            250,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("timeout response");

        assert_eq!(response.status, ToolExecutionStatus::Timeout);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            "mcp_sse_idle_timeout"
        );
        assert_eq!(response.billing.billable, false);
        assert_eq!(response.billing.reason, "timeout");
        assert_eq!(
            response
                .gateway_metadata
                .as_ref()
                .unwrap()
                .failure_class
                .as_deref(),
            Some("mcp_sse_idle_timeout")
        );
    }

    #[tokio::test]
    async fn mcp_sse_rejects_cross_origin_message_endpoint_before_post() {
        let Some(fixture) =
            McpSseFixture::start_with_endpoint("https://evil.example.com/message", vec![])
        else {
            return;
        };
        let target = test_mcp_sse_target(fixture.sse_url());
        let ctx = test_context(&target);
        let request = test_request();

        let response = super::execute_mcp_sse_tool(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("failed response");

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            "mcp_sse_message_endpoint_invalid"
        );
        assert_eq!(fixture.requests().len(), 0);
    }

    #[tokio::test]
    async fn mcp_sse_server_request_is_unsupported() {
        let Some(fixture) = McpSseFixture::start(vec![sse_raw_data(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "server-1",
            "method": "sampling/createMessage",
            "params": {}
        }))]) else {
            return;
        };

        let response = execute_fixture_call(fixture).await;

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            "mcp_sse_server_request_unsupported"
        );
        assert_eq!(response.billing.billable, false);
    }

    #[tokio::test]
    async fn mcp_sse_response_too_large_is_waived() {
        let Some(fixture) = McpSseFixture::start_with_large_event(70_000) else {
            return;
        };

        let response = execute_fixture_call(fixture).await;

        assert_eq!(response.status, ToolExecutionStatus::Failed);
        assert_eq!(
            response.error.as_ref().unwrap().code,
            "mcp_sse_response_too_large"
        );
        assert_eq!(response.cost.source, "waived");
    }

    async fn execute_fixture_call(
        fixture: McpSseFixture,
    ) -> crate::agent::tools::types::ToolCallResponse {
        let target = test_mcp_sse_target(fixture.sse_url());
        let ctx = test_context(&target);
        let request = test_request();

        super::execute_mcp_sse_tool(
            &ctx,
            &target,
            &request,
            &test_egress_policy(),
            DEFAULT_TIMEOUT_MS,
            MAX_RESPONSE_BYTES,
        )
        .await
        .expect("mcp sse response")
    }
}
