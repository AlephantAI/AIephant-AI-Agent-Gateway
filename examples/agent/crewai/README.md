# CrewAI agent example

This example builds a real CrewAI `Agent`, `Task`, `Crew`, and local tool, then
sends CrewAI-style native events to Alephant Agent Gateway:

- `CrewKickoffStartedEvent`
- `AgentThinkingStartedEvent`
- `AgentPlanCreatedEvent`
- `ToolUsageStartedEvent`
- `ToolUsageFinishedEvent`

Run a dry shape check without calling an LLM or a running gateway. The dry run
still emits the deterministic think -> plan -> tool preview so the gateway
payload shows a complete agent loop:

```bash
ALEPHANT_AGENT_DRY_RUN=true AI_GATEWAY_DEBUG_BODY=true \
python3 examples/agent/crewai/crewai_run.py
```

Run against a gateway and execute `crew.kickoff()`:

```bash
export GATEWAY_BASE=http://127.0.0.1:8080
export ALEPHANT_API_KEY=<virtual-key>
export ALEPHANT_MODEL=openai/gpt-4o-mini
python3 examples/agent/crewai/crewai_run.py
```

CrewAI is configured with an OpenAI-compatible LLM pointed at
`$GATEWAY_BASE/v1`, so the LLM request goes through the local Alephant gateway
instead of directly calling OpenAI.
