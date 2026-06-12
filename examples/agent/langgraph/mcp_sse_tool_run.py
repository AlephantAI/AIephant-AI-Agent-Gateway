#!/usr/bin/env python3
"""LangGraph/LangChain-compatible example for Alephant MCP SSE tools."""

from __future__ import annotations

import json
import os
import urllib.request
from pathlib import Path
from typing import Any


FRAMEWORK = "langgraph-compatible"
ADAPTER = "langgraph"
AGENT_ID = os.getenv("ALEPHANT_AGENT_ID", "mcp-sse-demo-agent")
AGENT_NAME = os.getenv("ALEPHANT_AGENT_NAME", "MCP SSE Demo Agent")
RUN_ID = os.getenv("ALEPHANT_RUN_ID", os.getenv("RUN_ID", "run_mcp_sse_demo"))
STEP_ID = os.getenv("STEP_ID", "step_mcp_sse_langgraph")


def load_env() -> None:
    env_file = Path(__file__).resolve().parents[3] / ".env"
    if not env_file.exists():
        return
    for raw_line in env_file.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.removeprefix("export ").strip()
        value = value.strip().strip("\"'")
        os.environ.setdefault(key, value)


load_env()

GATEWAY_URL = os.getenv(
    "GATEWAY_URL",
    os.getenv(
        "ALEPHANT_GATEWAY_URL",
        os.getenv("AI_GATEWAY_BASE_URL", "http://127.0.0.1:8080"),
    ),
)
API_KEY = os.getenv(
    "ALEPHANT_API_KEY",
    os.getenv(
        "API_KEY",
        os.getenv(
            "ALEPHANT_CONTROL_OPENROUTER_API_KEY",
            os.getenv("OPENAI_API_KEY", ""),
        ),
    ),
)

if not API_KEY:
    raise SystemExit(
        "set ALEPHANT_API_KEY, API_KEY, ALEPHANT_CONTROL_OPENROUTER_API_KEY, or OPENAI_API_KEY first"
    )


def request_json(path: str, payload: dict[str, Any]) -> dict[str, Any]:
    request = urllib.request.Request(
        GATEWAY_URL.rstrip("/") + path,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "authorization": f"Bearer {API_KEY}",
            "accept": "application/json",
            "content-type": "application/json",
            "Alephant-Agent-Id": AGENT_ID,
            "Alephant-Agent-Name": AGENT_NAME,
            "Alephant-Run-Id": RUN_ID,
            "Alephant-Step-Id": STEP_ID,
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read() or b"{}")


def sanitize(value: Any) -> Any:
    forbidden = {
        "targetHash",
        "target_hash",
        "mcpSessionId",
        "mcp_session_id",
        "messageEndpoint",
        "message_endpoint",
        "rawEvent",
        "raw_event",
    }
    if isinstance(value, dict):
        return {
            key: sanitize(inner)
            for key, inner in value.items()
            if key not in forbidden
        }
    if isinstance(value, list):
        return [sanitize(item) for item in value]
    return value


def main() -> None:
    tools = request_json(
        "/v1/agent/tools/list",
        {"agent_id": AGENT_ID, "adapter": ADAPTER},
    ).get("tools", [])
    if not tools:
        raise SystemExit("no tools returned from /v1/agent/tools/list")

    first_tool = tools[0]
    call = request_json(
        "/v1/agent/tools/call",
        {
            "source": FRAMEWORK,
            "agent_id": AGENT_ID,
            "agent_name": AGENT_NAME,
            "run_id": RUN_ID,
            "step_id": STEP_ID,
            "tool_call_id": "call_mcp_sse_langgraph",
            "tool_id": first_tool["toolId"],
            "arguments": {"query": "refund policy"},
        },
    )
    print(
        json.dumps(
            {
                "framework": FRAMEWORK,
                "registered_tool_name": first_tool.get("frameworkToolName"),
                "target_kind": first_tool.get("metadata", {}).get("targetKind"),
                "call": sanitize(call),
            },
            indent=2,
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
