# OpenAPI Agent Tool demos

These examples demonstrate the OpenAPI Agent Tool target path:

- `/v1/agent/tools/list` returns framework-safe descriptors.
- `/v1/agent/tools/call` executes the gateway-owned OpenAPI target.
- The gateway emits requested and terminal Agent Event logs with operation and billing metadata.
- Policy denied, schema invalid, stale snapshot, timeout, and upstream errors are returned as stable envelopes.

## Gateway config

Start the support mock:

```bash
python3 examples/agent/openapi/support_api_mock.py
```

Configure the gateway with static OpenAPI tools that point to the mock API:

```yaml
agent:
  enabled: true
  tools:
    enabled: true
    egress-policy:
      https-only: false
      block-loopback: false
      block-private-network: false
    targets:
      - tool-id: support.get_ticket
        name: Get support ticket
        description: Fetch a support ticket by id.
        kind: openapi
        method: GET
        url: http://127.0.0.1:9108/v1/tickets/{ticket_id}
        service-slug: support-api
        operation-id: getTicket
        operation-slug: get_ticket
        input-schema:
          type: object
          required: [ticket_id]
          properties:
            ticket_id:
              type: string
        rate-card:
          currency: USD
          fixed-micros: 4200
      - tool-id: support.review_refund
        name: Review refund
        description: Review a refund request.
        kind: openapi
        method: POST
        url: http://127.0.0.1:9108/v1/refund-reviews
        service-slug: support-api
        operation-id: reviewRefund
        operation-slug: review_refund
        input-schema:
          type: object
          required: [ticket_id, amount_cents]
          properties:
            ticket_id:
              type: string
            amount_cents:
              type: integer
            reason:
              type: string
        rate-card:
          currency: USD
          fixed-micros: 9500
      - tool-id: support.slow
        name: Slow support check
        description: Demonstrate timeout handling.
        kind: openapi
        method: GET
        url: http://127.0.0.1:9108/v1/slow
        service-slug: support-api
        operation-id: slowCheck
        operation-slug: slow_check
        timeout-ms: 250
        input-schema:
          type: object
        rate-card:
          currency: USD
          fixed-micros: 1000
```

## Environment

The scripts load `.env` automatically.

```bash
export AI_GATEWAY_BASE_URL=http://127.0.0.1:3000
export ALEPHANT_API_KEY=<virtual-key-or-api-key>
export ALEPHANT_AGENT_ID=openapi-demo-agent
export ALEPHANT_DEBUG_BODY=true
```

## Run

```bash
bash examples/agent/openapi/openapi_list_tools.sh
bash examples/agent/openapi/openapi_call_success.sh
bash examples/agent/openapi/openapi_call_schema_invalid.sh
bash examples/agent/openapi/openapi_call_policy_blocked.sh
bash examples/agent/openapi/openapi_call_upstream_error.sh
bash examples/agent/openapi/openapi_call_timeout.sh
python3 examples/agent/langgraph/openapi_tool_run.py
python3 examples/agent/openai_agents/openapi_tool_run.py
python3 examples/agent/crewai/openapi_tool_run.py
python3 examples/agent/mastra/openapi_tool_run.py
npx tsx examples/agent/mastra/openapi_tool_run.ts
```

`openapi_call_policy_blocked.sh` is self-checking by default. It exits non-zero unless the response is `status=blocked` with `error.code=openapi_policy_blocked`. Set `EXPECT_POLICY_BLOCKED=false` only when you intentionally run without the matching policy rule.

`examples/agent/mastra/openapi_tool_run.ts` is a Mastra TypeScript adapter reference. Run it with your TypeScript runner, for example `npx tsx examples/agent/mastra/openapi_tool_run.ts`, after installing the local Mastra example dependencies.

## Compatibility matrix

| Scenario | LangGraph | OpenAI Agents | CrewAI | Mastra |
| --- | --- | --- | --- | --- |
| tools/list descriptor conversion | expected pass | expected pass | expected pass | expected pass |
| framework_tool_name registration | expected pass | expected pass | expected pass | expected pass |
| tool_id call key | expected pass | expected pass | expected pass | expected pass |
| schema_invalid recoverable result | expected pass | expected pass | expected pass | expected pass |
| policy_blocked stops further calls | expected pass | expected pass | expected pass | expected pass |
| snapshot stale refresh_tools | expected pass | expected pass | expected pass | expected pass |
| timeout/upstream_5xx no crash | expected pass | expected pass | expected pass | expected pass |
| timeline requested + terminal event | expected pass | expected pass | expected pass | expected pass |

## Verification notes

- Descriptor conversion and `framework_tool_name` registration: run each framework script and inspect `registered_tool_name` or `registered_tool`.
- `tool_id` call key: framework scripts call `/v1/agent/tools/call` with the descriptor `toolId`, not the framework-safe name.
- `schema_invalid recoverable result`: each framework script also sends `ticket_id` as a number and prints the normalized error envelope.
- `policy_blocked stops further calls`: configure policy to block `support.review_refund` for `amount_cents >= 75000`, then run `openapi_call_policy_blocked.sh`.
- `snapshot stale refresh_tools`: set stale snapshot guard fields in a call payload; framework examples use `call_with_refresh_once` and retry after `agentAction=refresh_tools`.
- `timeout/upstream_5xx no crash`: run `openapi_call_timeout.sh` and `openapi_call_upstream_error.sh`.
- `timeline requested + terminal event`: enable agent event logging and inspect downstream logs for the same `toolExecutionId` on `tool.call.requested` and terminal `tool.result.received`.
