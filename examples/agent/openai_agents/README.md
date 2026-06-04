# OpenAI Agents SDK example

This example builds a real OpenAI Agents SDK `Agent`, attaches a
`function_tool`, points the model at the local Alephant gateway, and sends
OpenAI Agents-style native events to Alephant Agent Gateway:

- `agent_thinking`
- `plan_created`
- `tool_called`
- `tool_output`
- `llm_request_started`
- `handoff_requested`

Run a dry shape check without calling an LLM or a running gateway. The dry run
still emits a deterministic think -> plan -> tool -> LLM preview:

```bash
ALEPHANT_AGENT_DRY_RUN=true AI_GATEWAY_DEBUG_BODY=true \
python3 examples/agent/openai_agents/openai_agents_run.py
```

Run against a gateway:

```bash
export GATEWAY_BASE=http://127.0.0.1:8080
export ALEPHANT_API_KEY=<virtual-key>
export ALEPHANT_MODEL=openai/gpt-4o-mini
python3 examples/agent/openai_agents/openai_agents_run.py
```

The SDK model uses `OpenAIChatCompletionsModel` with an `AsyncOpenAI` client
pointed at `$GATEWAY_BASE/v1`, so the LLM request goes through the gateway
instead of directly calling OpenAI.
