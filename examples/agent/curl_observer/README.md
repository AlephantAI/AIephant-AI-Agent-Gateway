# Curl Observer Examples

These examples exercise gateway-observed Agent events parsed from LLM
responses. They call normal OpenAI-compatible model endpoints, not
`/v1/agent/events`.

## Setup

The scripts automatically load `.env` from the repository root. You can also
override values from the shell:

```bash
export GATEWAY_URL="http://127.0.0.1:8080"
export API_KEY="your-gateway-key"
export MODEL="gpt-4.1-mini"
export RUN_ID="run_curl_observer_001"
```

Supported `.env` fallbacks:

```text
ALEPHANT_GATEWAY_URL -> GATEWAY_URL
ALEPHANT_API_KEY -> API_KEY
ALEPHANT_CONTROL_OPENROUTER_API_KEY -> API_KEY
OPENAI_API_KEY -> API_KEY
```

`AGENT_ID` is generated automatically as `curl-observer-agent-<random>` unless
you set it explicitly.

Optional debug output:

```bash
export DEBUG_BODY=true
```

For OpenRouter `/v1/responses` tool-call checks, use an OpenRouter virtual key
and a provider-qualified model:

```bash
API_KEY="$ALEPHANT_CONTROL_OPENROUTER_API_KEY" \
MODEL="openai/o4-mini" \
bash examples/agent/curl_observer/responses_tool_nonstream.sh
```

This path exercises OpenRouter's native Responses API. The upstream request
must keep `tool_choice.type=function`; otherwise OpenRouter returns
`invalid_prompt`.

## Run

```bash
bash examples/agent/curl_observer/chat_tool_nonstream.sh
bash examples/agent/curl_observer/chat_tool_stream.sh
bash examples/agent/curl_observer/responses_tool_nonstream.sh
bash examples/agent/curl_observer/responses_tool_stream.sh
bash examples/agent/curl_observer/responses_text_stream.sh
```

## Expected Observed Events

Query downstream agent event logs by:

```text
alephantAgentId = curl-observer-agent
alephantRunId = run_curl_observer_001
eventSourceTrust = gateway_observed
stepSource = gateway
```

Expected event types include:

```text
tool.call.observed
llm.reasoning.observed
llm.response.completed.observed
error.observed
```

`chat_tool_stream.sh` exercises the ChatCompletions SSE Tool Observer. When
the model emits `delta.tool_calls`, downstream Agent event logs should include:

```text
eventType = tool.call.observed
observer = chat_completions_stream_tool_observer
sourceWire = chat_completions_sse
observedOnly = true
runtimeConfirmed = false
```

This event means the model proposed a tool call. It does not mean the tool was
executed by the gateway.

`responses_text_stream.sh` is a negative control. It should not produce a
`tool.call.observed` event.
