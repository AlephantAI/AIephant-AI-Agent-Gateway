# n8n workflow example

This directory has two n8n examples:

- `workflow.json`: an importable n8n workflow that calls the current gateway
  `/v1/chat/completions`, then posts n8n-style events to `/v1/agent/events`.
- `n8n_run.py`: a lightweight Python event-shape smoke test kept for the shared
  framework adapter tests.

The workflow sends n8n-style events to Alephant Agent Gateway:

- `execution.started`
- `node.started`
- `node.finished`

The `nodeType` contains a clear `tool` token so the gateway adapter maps the
node event to a tool preflight event.

Run the real workflow:

1. Import `examples/agent/n8n/workflow.json` into n8n.
2. Set environment variables visible to n8n:

```bash
export GATEWAY_BASE=http://127.0.0.1:8080
export ALEPHANT_API_KEY=<vk-or-api-key>
export ALEPHANT_MODEL=openai/gpt-4o-mini
```

3. Execute the workflow manually from n8n.

The first HTTP Request node calls the current gateway as an OpenAI-compatible
LLM endpoint. The second HTTP Request node emits agent events back to the
gateway.

Run a dry shape check:

```bash
ALEPHANT_AGENT_DRY_RUN=true AI_GATEWAY_DEBUG_BODY=true \
python3 examples/agent/n8n/n8n_run.py
```
