# Mastra agent example

This directory has two Mastra examples:

- `mastra_run.mjs`: a Node.js Mastra SDK agent using `@mastra/core`, a local tool, and the current gateway as an OpenAI-compatible model provider.
- `mastra_run.py`: a lightweight Python event-shape smoke test kept for the shared framework adapter tests.

The SDK example sends Mastra-style workflow events to Alephant Agent Gateway:

- `workflow.run.started`
- `agent.thinking.started`
- `agent.plan.created`
- `tool.call.started`
- `tool.call.finished`
- `llm.call.started`

Install dependencies:

```bash
cd examples/agent/mastra
npm install
```

Run a dry SDK shape check:

```bash
ALEPHANT_AGENT_DRY_RUN=true AI_GATEWAY_DEBUG_BODY=true npm run dry-run
```

Run the real Mastra agent through the current gateway:

```bash
ALEPHANT_API_KEY=<vk-or-api-key> GATEWAY_BASE=http://127.0.0.1:8080 npm run run
```

Run the legacy Python shape check:

```bash
ALEPHANT_AGENT_DRY_RUN=true AI_GATEWAY_DEBUG_BODY=true \
python3 examples/agent/mastra/mastra_run.py
```
