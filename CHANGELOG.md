
```md

# 2026-06-12 Updates

# Alephant AI Gateway Update Logs

This submission advances the Agent tool gateway capability from "observable" to a complete closed loop that is "routable, executable, governable, auditable, and locally verifiable", and simultaneously completes the MCP/OpenAPI tool targets, framework examples, x402, and configuration templates.

## Completion of Agent Tools Gateway capabilities
Added ai-gateway/src/agent/tools/* A complete set of tool gateway modules, including tool catalog, executor, auditing, policy evaluation, outbound restrictions, idempotence, runtime snapshot, schema verification and unified response structure.
## MCP Tool Target support
Three new target adaptation codes, mcp_http, mcp_streamable_http, and mcp_sse, are added, covering MCP initialization, session/lifecycle, SSE parsing, transport, target hash, test support, etc.
## OpenAPI Tool Target support
Added ai-gateway/src/agent/tools/openapi/*, including OpenAPI tool execution, request mapping, outbound control, execution result modeling and type definition.
## Agent event observation and log enhancement
Update agent/, logger/, tool_observer.rs, log_payload.rs, etc., and enhance the event recording, sink/transport, context and log fields of tool call / tool execution.
## Agent Tools E2E and examples
Added ai-gateway/examples/agent_tools_e2e_gateway.rs, as well as examples/agent/tools/, examples/agent/mcp_sse/, examples/agent/openapi/*, for locally running MCP/OpenAPI tool list/call, mock server, event sink and runtime snapshot scenarios.
## Multi-Agent framework example extension
Add MCP SSE and OpenAPI tool examples to crewai, langgraph, mastra, openai_agents, and update public framework adapter tests.
## x402 payment link update
Synchronize x402/*, including payment client, policy, proxy, service, snapshot, forward signature, log, body schema, etc.
## Configuration and template synchronization
Configuration updates such as .env.template, .gitignore, Cargo.toml, ai-gateway/config/*.yaml, etc. will be synchronized;

# 2026-06-02 Updates

# Alephant AI Gateway Update Logs

This release adds a major set of new capabilities across x402 payments, Agent Gateway runtime context, Agent Policy validation, Agent Event Logs, Agent Name detection, and popular agent framework adapters.

## x402 / Payment Sidecar

Added full x402 Payment Sidecar support for payment verification, policy checks, settlement, and upstream proxying for agent and API endpoints.

- Added x402 policy/payment gRPC protos for policy validation, payment verification, and settlement.
- Added x402 config for payment service, policy service, logging, auth keys, and related settings.
- Added `/x402/agents/{slug}` route for x402 agent endpoints.
- Added x402 route kinds to distinguish agent routes, API routes, and other x402 route types.
- Added endpoint snapshot support built from endpoint/router data.
- Added endpoint type route enforcement to prevent agent endpoints from being used as API routes.
- Added full payment flow: unpaid requests return `402`; paid requests verify, settle, then proxy upstream.
- Added payment client, policy client, and upstream proxy helpers.
- Hardened header security with header stripping, x402 header allowlist, and forward signatures.
- Added body schema validation before policy checks.
- Hardened schema defaults and sanitized schema validation logs.
- Added x402 payment log schema.
- Added x402 payment logs through Redis Stream and HTTP transport.
- Added x402 log auth and log base-url configuration.
- Expanded x402 test coverage for sidecar routes, auth bypass, `402`, schemas, header allowlists, signatures, and endpoint types.

## AI Agent Gateway

Added a dedicated Agent Gateway layer so Alephant can identify, trace, and govern agent runs, steps, tool calls, and graph nodes.

- Added agent modules for context, events, service, sink, policy, and log payloads.
- Added agent config for `agent.enabled`, header context, metadata redaction, conflict handling, TTL, and related settings.
- Added `AgentContext` with `agent_id`, `run_id`, `step_id`, `step_kind`, `tool_call_id`, `graph_node`, and more.
- Added agent header parser for `Alephant-Agent-Id`, `Alephant-Run-Id`, `Alephant-Step-Id`, and related headers.
- Hardened header handling so empty, oversized, or invalid values do not pollute context.
- Stripped agent identification headers before forwarding requests to model providers.
- Integrated `AgentContext` into request extensions.
- Enhanced LLM request logs with agent, run, step, tool, and graph node fields.
- Fixed fallback log context so failed and fallback paths preserve agent context.
- Bypassed normal model-support body reading for `/v1/agent/events`.
- Added `/v1/agent/events` endpoint for agent event ingestion.
- Added event ingestion service for single and batch events.
- Added agent event schema: `AgentEventInput`, `Envelope`, `Response`, and `Decision`.
- Added metadata redaction for sensitive keys.
- Added step state tracking by workspace, agent, run, and step.
- Added step fingerprinting using parent, kind, node, tool call, attempt, and input hash to detect `step_id` conflicts.
- Added conflict handling between header context and payload context: warn, strict, or disabled.
- Added event sink to normalize events before sending them into the logging pipeline.

## Agent Policy Validation

Added Agent Policy Validation so agent events can be checked, blocked, audited, or skipped before execution.

- Synced `ValidateAgentPolicy` proto.
- Added independent policy timeout config.
- Built policy requests with workspace, agent, run, step, model, provider, tool, and metadata.
- Defined `agent_id` priority: authenticated bound agent > `agent_uid` > virtual key > payload `agent_id`.
- Added tool name fallback from event name when metadata does not include `tool_name`.
- Returned policy response shape to SDKs: `allowed`, `policyDecision`, `reason`, `violations`, `warnings`, and more.
- Attached policy decisions to event metadata for downstream logs.
- Protected against forged policy metadata: client-provided policy data is moved to `original` and not trusted.
- Added metadata limit handling with compaction.
- Preserved core policy fields for oversized denied-event metadata.
- Returned stable gateway errors when the policy service is unavailable.
- Emitted `policy_unavailable` audit events when policy service calls fail.
- Skipped policy calls for completion events such as `run.completed`; these are logged only.
- Added phase-based policy gating: only before + pre_action + medium/high confidence events call policy.
- Added audit-only skip behavior for status updates, result events, and low-confidence events.
- Returned stable error codes when event log sink delivery fails.

## Agent Event Log / ClickHouse Downstream

Added a dedicated Agent Event Log pipeline for agent behavior, policy results, runtime phases, and sink delivery status.

- Defined Agent event log payloads independent from LLM request logs.
- Added Redis Stream transport to `lc:stream:alephant-agent-events`.
- Added HTTP fallback to `/v1/log/agent-event` when Redis is unavailable.
- Protected fallback debug output from leaking HTTP auth tokens.
- Added explicit error handling for Redis failures, HTTP failures, and timeouts.
- Added log fields for workspace, agent, run, step, event, status, severity, and metadata.
- Emitted downstream camelCase fields such as `alephantAgentId`, `eventType`, and `policyDecision`.
- Added policy field mapping: `policyReason`, `policyBlockedBy`, `policyScope`, `policySnapshotRevision`.
- Added phase field mapping: `eventPhase`, `policyStage`, `policyMode`, `eventSourceTrust`.
- Recorded `sinkStatus=sent` for each event.
- Recorded sink status for `policy_unavailable` events.
- Added agent name, name source, and trust level to Agent event logs.

## Agent Name Detection

Added Agent Name detection so logs, events, and request traces can display readable agent names.

- Added support for `Alephant-Agent-Name` header.
- Added support for payload `agent_name`.
- Added support for camelCase payload `agentName`.
- Rejected misspelled `agentNam` to avoid ambiguous pollution.
- Added VK label parsing, for example `label=agent:Test Agent`, to infer registered Agent Name.
- Defined name priority: registered VK/platform name > payload/header self-reported name.
- Added conflict metadata when registered names and self-reported names differ.
- Prevented self-reported names from overwriting registered names.
- Added agent name to LLM request logs.
- Added agent name to Agent event logs.

## Popular Agent Framework Event Adapters

Added adapters for popular agent frameworks so raw framework events can be normalized into Alephant Agent Gateway run, step, tool, and policy events.

- Added OpenAI Agents SDK source aliases: `openai_agents`, `openai-agent-sdk`, and more.
- Mapped OpenAI `tool_called` to `tool.call.requested`.
- Mapped OpenAI `tool_output` to `tool.result.received`.
- Added mappings for OpenAI handoff, approval, and reasoning events.
- Added n8n source alias support.
- Mapped n8n execution started / success / error events to run lifecycle events.
- Mapped n8n tool node started / finished events to tool calls.
- Mapped n8n non-tool nodes to normal steps to avoid false tool detection.
- Added CrewAI source aliases: `crewai`, `crew_ai`, `crew-ai`.
- Mapped CrewAI kickoff, task, and tool usage events to run, step, and tool events.
- Added Mastra source alias support.
- Mapped Mastra workflow, tool, LLM, and checkpoint events.
- Added LangGraph raw event mappings, including `on_tool_start` and `on_llm_start`.
- Defaulted unknown events to low confidence + audit-only.
- Added false-positive protection so terms like `tooling`, `allm`, and `incomplete` are not misclassified.
- Added framework raw fields allowlist so only safe raw fields are preserved in metadata.
```
