"""Mastra workflow event example for Alephant Agent Gateway."""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from framework_common import (  # noqa: E402
    AgentEventClient,
    base_event,
    default_agent_name,
    default_run_id,
    sleep_briefly,
)


SOURCE = "mastra"


def build_events(
    *,
    agent_id: str,
    run_id: str,
    agent_name: str | None = None,
) -> list[dict[str, Any]]:
    trace_id = f"trace_{run_id}"
    return [
        base_event(
            "workflow.run.started",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            traceId=trace_id,
            spanId="span_run",
            metadata={"workflow_name": "Support workflow"},
        ),
        base_event(
            "tool.call.started",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            traceId=trace_id,
            spanId="span_tool",
            toolName="kb.search",
            metadata={"query": "refund policy"},
        ),
        base_event(
            "llm.call.started",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            traceId=trace_id,
            spanId="span_llm",
            metadata={"model": "gpt-4o-mini", "provider": "openai"},
        ),
    ]


def main() -> None:
    agent_id = os.getenv("ALEPHANT_AGENT_ID", "mastra-demo-agent")
    run_id = default_run_id("run_mastra")
    agent_name = default_agent_name("Mastra Demo Agent")
    client = AgentEventClient.from_env()
    events = build_events(agent_id=agent_id, run_id=run_id, agent_name=agent_name)
    response = client.emit_events(source=SOURCE, events=events)
    sleep_briefly()
    print({"source": SOURCE, "run_id": run_id, "accepted": response.get("accepted")})


if __name__ == "__main__":
    main()
