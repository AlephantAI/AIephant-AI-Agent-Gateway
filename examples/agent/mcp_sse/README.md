# MCP SSE Agent Tool Demo

This demo validates traditional MCP SSE through the Alephant Agent Tools API.
The Agent calls `/v1/agent/tools/list` and `/v1/agent/tools/call`; it never sees
the MCP SSE session or message endpoint.

## Gateway Target

Configure an Agent Tool target similar to:

```yaml
agent:
  enabled: true
  tools:
    enabled: true
    egress-policy:
      https-only: false
      block-loopback: false
    targets:
      - tool-id: docs.search
        name: Search docs
        description: Search product docs through traditional MCP SSE
        kind: mcp-sse
        url: http://127.0.0.1:9118/sse
        method: GET
        rate-card:
          fixed-micros: 4200
          currency: USD
      - tool-id: docs.search-egress-blocked
        name: Search docs egress blocked
        description: Egress-blocked variant for verification
        kind: mcp-sse
        url: http://169.254.169.254/sse
        method: GET
```

The `docs.search-egress-blocked` target is optional and only needed for the
egress-blocked script.

## Run

Start the mock MCP SSE server in one terminal:

```bash
python3 examples/agent/mcp_sse/mcp_sse_mock_server.py
```

Run the gateway with an API key that can access the configured Agent Tools
targets, then run:

```bash
bash examples/agent/mcp_sse/list_tools.sh
bash examples/agent/mcp_sse/call_success.sh
bash examples/agent/mcp_sse/call_is_error.sh
bash examples/agent/mcp_sse/call_timeout.sh
bash examples/agent/mcp_sse/call_policy_blocked.sh
bash examples/agent/mcp_sse/call_egress_blocked.sh
bash examples/agent/mcp_sse/demo_finance_timeline.sh
python3 examples/agent/langgraph/mcp_sse_tool_run.py
python3 examples/agent/openai_agents/mcp_sse_tool_run.py
python3 examples/agent/crewai/mcp_sse_tool_run.py
python3 examples/agent/mastra/mcp_sse_tool_run.py
```

Required environment:

```bash
export GATEWAY_URL=http://127.0.0.1:8080
export ALEPHANT_API_KEY=sk-your-key
```

The scripts also read `.env` from the repository root.

## Expected Timeline

- `tool.call.requested`
- `tool.result.received` with `metadata.gateway.targetKind=mcp-sse`
- success call has settled/billable billing snapshot
- policy blocked call has waived billing and does not dispatch to target
- timeout call has retryable error and waived billing
- MCP `isError=true` remains a completed business result

`call_policy_blocked.sh` expects a policy service rule that blocks the
`policy-blocked` query. Without that policy, the target may execute normally.
