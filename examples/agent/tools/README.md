# Agent Tools Examples

These examples exercise the Alephant Agent Tool Gateway routes:

- `POST /v1/agent/tools/list`
- `POST /v1/agent/tools/call`

The gateway must be configured with `agent.enabled=true`,
`agent.tools.enabled=true`, and at least one target such as `support.echo`.

Run:

```bash
bash examples/agent/tools/list_tools.sh
bash examples/agent/tools/call_mock_tool.sh
```

MCP HTTP demo:

```bash
python3 examples/agent/tools/mcp_mock_server.py
```

Configure the gateway target as:

```yaml
- tool-id: docs.search
  name: docs.search
  kind: mcp-http
  url: http://127.0.0.1:9876/mcp
  method: POST
  rate-card:
    # fill in pricing details as needed
```

For local demo traffic, allow loopback/http egress (for example):

`agent.tools.egress-policy.https-only=false`, `agent.tools.egress-policy.block-loopback=false`.

```bash
TOOL_ID=docs.search bash examples/agent/tools/call_mcp_http_tool.sh
```

The scripts load `.env` from the repository root by default. Set
`ALEPHANT_GATEWAY_URL` and `ALEPHANT_API_KEY` in `.env` for normal runs.
`GATEWAY_URL` and `API_KEY` are still accepted as compatibility overrides.

## MCP Streamable HTTP example

This example exercises an `agent.tools.targets[].kind = mcp-streamable-http`
tool through the normal Alephant Tool API. The agent only calls
`/v1/agent/tools/call`; MCP initialize/session/SSE details stay inside the
gateway.

Start the mock MCP server:

```bash
python3 examples/agent/tools/mcp_streamable_mock_server.py
```

Configure a target:

```yaml
agent:
  enabled: true
  tools:
    enabled: true
    targets:
      - tool-id: docs.search
        name: Search docs
        description: Search product docs
        kind: mcp-streamable-http
        url: "http://127.0.0.1:8766/mcp"
        rate-card:
          currency: USD
          fixed-micros: 10000
```

For local demo traffic, allow loopback/http egress:

`agent.tools.egress-policy.https-only=false`, `agent.tools.egress-policy.block-loopback=false`.

Call through the gateway:

```bash
bash examples/agent/tools/call_mcp_streamable_http_tool.sh
```

The OpenAI Agents SDK and LangGraph examples first call `/v1/agent/tools/list`
to register framework-safe tool names, then their handlers call
`/v1/agent/tools/call` with the canonical `toolId`. They do not see MCP
initialize, `Mcp-Session-Id`, Redis cache state, or SSE details:

```bash
python3 examples/agent/tools/openai_agents_mcp_streamable_tool.py
python3 examples/agent/tools/langgraph_mcp_streamable_tool.py
```

## Real Agent Tools / MCP E2E validation

This validation proves that the gateway discovers Agent Tools, executes a tool
through a mock MCP Streamable HTTP target, and emits Agent Event logs to an HTTP
logs-collector sink.

Terminal 1, start the dedicated local Agent Tools E2E gateway. It uses the real
`AgentToolsService`, seeds an in-memory virtual key, and does not require
PostgreSQL:

```bash
unset REDIS_URL
cargo run -p ai-gateway --example agent_tools_e2e_gateway --features external,testing -- \
  --config examples/agent/tools/e2e.agent-tools.yaml \
  --port 18080 \
  --api-key sk-agent-tools-e2e
```

Terminal 2, run the E2E runner:

```bash
ALEPHANT_API_KEY=sk-agent-tools-e2e \
python3 examples/agent/tools/e2e_tool_event_loop.py \
  --gateway-url http://127.0.0.1:18080
```

The runner reads `.env` from the repository root. It accepts `--api-key`, or
one of `ALEPHANT_API_KEY`, `API_KEY`, `ALEPHANT_CONTROL_OPENROUTER_API_KEY`, or
`OPENAI_API_KEY`.

```bash
ALEPHANT_API_KEY="..." python3 examples/agent/tools/e2e_tool_event_loop.py
```

If you prefer to test a normal gateway binary instead of the dedicated local
E2E gateway, start it with `examples/agent/tools/e2e.agent-tools.yaml` and pass
a valid virtual-key API key to the runner.

Environment variables override YAML. If Redis is configured and available,
Agent Event logs may be written to Redis first instead of the HTTP sink. The
gateway also loads `.env`, so remove or override `REDIS_URL` there before
starting Terminal 1.

By default the runner accepts both a fresh MCP lifecycle
(`initialize -> notifications/initialized -> tools/call`) and a cached-session
dispatch (`tools/call` only). To require a fresh lifecycle, run with
`--require-mcp-lifecycle` and make sure Redis session cache is not configured or
does not contain a reusable MCP session for the target.

If the local gateway fails to start because it cannot connect to a database,
configure a working database/local environment first. The gateway needs a
usable `POSTGRES_DATABASE_URL` or `AI_GATEWAY__DATABASE__URL` (the default local
database is `postgres://postgres:postgres@localhost:54322/postgres`) and a
`MASTER_KEY_ENCRYPTION_KEY` whose base64-decoded value is 32 bytes. In a
restricted sandbox, a blocked database connection may appear as a database
`EPERM` error.

Expected output:

```text
Agent Tools E2E passed. Artifacts: /tmp/alephant-agent-tools-e2e/<run_id>
```

Artifacts:

- `agent-events.jsonl`
- `mock-mcp-requests.jsonl`
- `run-manifest.json`
- `timeline-summary.json`
- `run-cost-summary.json`
