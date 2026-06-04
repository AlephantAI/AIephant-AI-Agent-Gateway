"""n8n workflow event example for Alephant Agent Gateway."""

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


SOURCE = "n8n"


def build_events(
    *,
    agent_id: str,
    run_id: str,
    agent_name: str | None = None,
) -> list[dict[str, Any]]:
    workflow_id = "wf_support_triage"
    execution_id = run_id
    return [
        base_event(
            "execution.started",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            workflowId=workflow_id,
            executionId=execution_id,
            metadata={"workflow_name": "Support triage"},
        ),
        base_event(
            "node.started",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            workflowId=workflow_id,
            executionId=execution_id,
            nodeId="node_fetch_ticket",
            nodeName="Fetch ticket",
            nodeType="n8n-nodes-langchain.tool",
            metadata={"tool_name": "zendesk.get_ticket"},
        ),
        base_event(
            "node.finished",
            agent_id=agent_id,
            run_id=run_id,
            agent_name=agent_name,
            workflowId=workflow_id,
            executionId=execution_id,
            nodeId="node_fetch_ticket",
            nodeName="Fetch ticket",
            nodeType="n8n-nodes-langchain.tool",
            metadata={"result_preview": "ticket #1234"},
        ),
    ]


def main() -> None:
    agent_id = os.getenv("ALEPHANT_AGENT_ID", "n8n-demo-agent")
    run_id = default_run_id("exec_n8n")
    agent_name = default_agent_name("n8n Workflow Demo Agent")
    client = AgentEventClient.from_env()
    events = build_events(agent_id=agent_id, run_id=run_id, agent_name=agent_name)
    response = client.emit_events(source=SOURCE, events=events)
    sleep_briefly()
    print({"source": SOURCE, "run_id": run_id, "accepted": response.get("accepted")})


if __name__ == "__main__":
    main()
