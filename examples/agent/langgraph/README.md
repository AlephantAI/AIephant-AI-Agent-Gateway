# LangGraph Agent Gateway examples

These examples run a real LangGraph graph while sending Alephant Agent Gateway
headers and `/v1/agent/events` telemetry.

## Setup

```bash
pip install langgraph
```

The examples automatically load the nearest `.env` file without overriding
variables already exported in your shell. To override values for one run:

```bash
export GATEWAY_BASE=http://127.0.0.1:8080
export ALEPHANT_CONTROL_OPENROUTER_API_KEY=<virtual-key>
export ALEPHANT_AGENT_NAME="Support Bot"
export ALEPHANT_MODEL=openai/gpt-4o-mini
```

The adapter also accepts `ALEPHANT_GATEWAY_URL` and `ALEPHANT_API_KEY` as
fallback names.

`ALEPHANT_AGENT_NAME` is optional. If the virtual key is registered with a label
like `agent:Support Bot`, the gateway uses that registered name first; this
environment value is the self-reported fallback for unregistered agents.
When this environment value is unset, each example sends a clear demo name such
as `LangGraph Basic Demo Agent` as the self-reported fallback.

For a local shape check without a running gateway or provider key:

```bash
export ALEPHANT_AGENT_DRY_RUN=true
python3 examples/agent/langgraph/basic_run.py
```

To print request/response headers and bodies for both `/v1/agent/events` and
LLM requests:

```bash
export AI_GATEWAY_DEBUG_HEADERS=true
export AI_GATEWAY_DEBUG_BODY=true
python3 examples/agent/langgraph/basic_run.py
```

You can also enable debug output for one request by adding request headers:

```text
alephant-debug-headers: true
alephant-debug-body: true
```

## Examples

- `basic_run.py`: one LangGraph node emits `run.*` and `step.*` events and calls
  `/v1/chat/completions` with `Alephant-Agent-*` headers.
- `tool_run.py`: planner -> mock tool -> final answer. Emits
  `tool.call.requested` and `tool.call.completed`.
- `loop_run.py`: intentionally repeats the same mock tool call and emits
  `loop.warning` evidence.

## Required gateway config

```yaml
agent:
  enabled: true
  allow-header-context: true
  event-stream-key: "lc:stream:agent_events"
  context-conflict-action: warn
  step-conflict-action: warn
  metadata-redaction: basic
```

## Validate the examples

```bash
python3 -m unittest examples.agent.langgraph.test_examples
```
