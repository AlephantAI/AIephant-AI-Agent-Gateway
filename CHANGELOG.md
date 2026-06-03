# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### x402 / Payment Sidecar

- Added x402 policy/payment protobuf contracts for policy validation, payment verification, settlement, and related gRPC flows.
- Added x402 configuration for the payment service, policy service, logging, authentication keys, and related runtime settings.
- Added the `/x402/agents/{slug}` route so x402 agent endpoints can enter the sidecar flow.
- Bypassed normal gateway authentication for x402 routes so the sidecar owns the payment and authorization boundary.
- Added x402 route kinds to distinguish agent routes, API routes, and other x402 route types.
- Added endpoint snapshot support to build x402-ready snapshots from endpoint and router data.
- Added endpoint type metadata to snapshots to determine whether `/x402/agents/*` or `/x402/api/*` matches.
- Enforced endpoint type routing to prevent agent endpoints from being used through API routes.
- Implemented the sidecar payment flow: unpaid requests return `402`; paid requests are verified, settled, and then proxied upstream.
- Added a payment client for verification, settlement, and payment status checks.
- Added a policy client to decide whether requests may enter the payment and upstream flow.
- Added upstream proxy helpers for forwarding upstream requests, responses, headers, and bodies.
- Added a header stripping policy to remove auth, cookie, and policy headers that should not be forwarded upstream.
- Added an x402 header allowlist so only safe headers are passed through.
- Added x402 forward signatures to generate and validate signed forwarded requests.
- Added body schema validation before x402 request bodies enter policy evaluation.
- Hardened schema defaults in snapshots so missing schema values are safer by default.
- Sanitized schema validation logs to avoid leaking sensitive fields.
- Defined the x402 payment log payload schema.
- Added x402 payment log delivery through Redis streams.
- Added x402 payment log delivery over HTTP.
- Added authentication controls for the HTTP payment log endpoint through headers and environment variables.
- Added configurable base URLs for x402 log delivery.
- Added x402 tests for sidecar routing, auth bypass, `402` responses, schemas, header allowlists, signatures, and endpoint types.

### AI Agent Gateway

- Added an agent module with standalone components for agent context, events, services, sinks, policies, and log payloads.
- Added agent configuration for `agent.enabled`, header context, metadata redaction, conflict actions, TTL, and related controls.
- Added `AgentContext` to carry `agent_id`, `run_id`, `step_id`, `step_kind`, `tool_call_id`, `graph_node`, and related context.
- Added an agent header parser for `Alephant-Agent-Id`, `Alephant-Run-Id`, `Alephant-Step-Id`, and related headers.
- Hardened empty and invalid agent headers so empty values, overlong values, and invalid numbers do not pollute context.
- Stripped agent identification headers before forwarding requests to upstream LLM providers.
- Integrated agent context into request extensions.
- Enhanced LLM request logs with agent, run, step, tool, and graph node fields.
- Fixed fallback log context so failed and fallback paths preserve agent context.
- Bypassed normal model-support body reading for `/v1/agent/events`.
- Added the `/v1/agent/events` route for agent event ingestion.
- Added an event ingestion service that accepts single events and batches.
- Defined event schemas for `AgentEventInput`, envelopes, responses, and decisions.
- Added baseline metadata redaction for sensitive keys.
- Added step state tracking by workspace, agent, run, and step to detect step conflicts.
- Added step fingerprints based on parent, kind, node, tool call, attempt, and input hash to identify `step_id` conflicts.
- Added configurable context conflict handling for header context and payload conflicts: warn, strict, or disabled.
- Added an event sink so normalized events can enter the log delivery pipeline.

### Agent Policy Validation

- Synced the `ValidateAgentPolicy` protobuf contract.
- Added an independent timeout configuration for agent policy calls.
- Built policy requests with workspace, agent, run, step, model, provider, tool, and metadata fields.
- Defined `agent_id` precedence as authenticated bound agent, `agent_uid`, virtual key, then payload `agent_id`.
- Added tool name fallback from the event name when `tool_name` is missing from metadata.
- Returned SDK-facing policy response fields including `allowed`, `policyDecision`, `reason`, `violations`, and `warnings`.
- Attached policy decisions to event metadata for downstream log consumption.
- Protected against forged policy metadata by moving client-provided policy fields into `original` instead of trusting them.
- Compacted policy metadata when it exceeds configured limits.
- Preserved core policy fields for denied-event audit records when metadata is too large.
- Returned stable gateway errors when the policy service is unavailable.
- Emitted `policy_unavailable` audit event logs when the policy service is unavailable.
- Skipped policy calls for completed events such as `run.completed`; those events are logged only.
- Gated policy calls by phase so only `before` + `pre_action` events with medium or high confidence call policy.
- Skipped policy calls for status updates, result events, and low-confidence events; these are recorded with skipped decisions.
- Returned stable error codes when event log delivery fails.

### Agent Event Log / ClickHouse Downstream

- Defined an agent event log payload independent from the LLM request log payload.
- Added Redis stream transport for `lc:stream:alephant-agent-events`.
- Added HTTP fallback delivery to downstream `/v1/log/agent-event` when Redis is unavailable.
- Protected fallback tokens so debug output does not expose HTTP auth tokens.
- Added explicit error handling for Redis failures, HTTP failures, and timeouts.
- Added log payload fields for workspace, agent, run, step, event, status, severity, and metadata.
- Emitted camelCase downstream fields such as `alephantAgentId`, `eventType`, and `policyDecision`.
- Added policy field mappings for `policyReason`, `policyBlockedBy`, `policyScope`, and `policySnapshotRevision`.
- Added phase field mappings for `eventPhase`, `policyStage`, `policyMode`, and `eventSourceTrust`.
- Recorded `sinkStatus=sent` for each delivered event.
- Recorded `sinkStatus` for policy-unavailable events.
- Added agent name fields to event logs, including `alephantAgentName`, name source, and trust level.

### Agent Name Identification

- Added support for the `Alephant-Agent-Name` header.
- Added support for payload `agent_name`.
- Added support for camelCase payload `agentName`.
- Rejected misspelled `agentNam` to avoid ambiguous context pollution.
- Added virtual-key label parsing so `label=agent:Test Agent` can infer the registered platform agent name.
- Defined name precedence as registered platform virtual-key name before self-reported payload/header names.
- Recorded name conflict metadata when the registered platform name and self-reported name conflict, without allowing self-reported names to overwrite registered names.
- Added agent names to LLM request logs.
- Added agent names to agent event logs.

### Popular Agent Framework Event Adapters

- Added OpenAI Agents SDK source aliases such as `openai_agents` and `openai-agent-sdk`.
- Mapped OpenAI `tool_called` to `tool.call.requested`.
- Mapped OpenAI `tool_output` to `tool.result.received`.
- Mapped OpenAI handoff, approval, and reasoning events.
- Added the `n8n` source alias.
- Mapped n8n execution started, success, and error events to the run lifecycle.
- Mapped n8n tool node started and finished events to tool calls.
- Mapped non-tool n8n nodes to normal steps to avoid false tool classification.
- Added CrewAI source aliases such as `crewai`, `crew_ai`, and `crew-ai`.
- Mapped CrewAI kickoff, task, and tool usage events to run, step, and tool events.
- Added the `mastra` source alias.
- Mapped Mastra workflow, tool, LLM, and checkpoint events.
- Mapped LangGraph raw events such as `on_tool_start` and `on_llm_start`.
- Handled unknown events conservatively as low-confidence and audit-only by default.
- Added false-positive protection for unknown events so strings such as `tooling`, `allm`, and `incomplete` are not misidentified.
- Added a framework raw-fields allowlist so only safe raw fields are retained in metadata.
