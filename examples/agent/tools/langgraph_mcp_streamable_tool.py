#!/usr/bin/env python3
"""LangGraph/LangChain tool registration example for Alephant MCP tools."""

from __future__ import annotations

import json
import os
import urllib.request
from pathlib import Path
from typing import Any

try:
    from langchain_core.tools import StructuredTool
except Exception:
    StructuredTool = None


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

DEFAULT_HTTP_USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36 "
    "AlephantAgentExample/1.0"
)
GATEWAY_URL = os.getenv(
    "GATEWAY_URL",
    os.getenv("ALEPHANT_GATEWAY_URL", os.getenv("AI_GATEWAY_URL", "http://127.0.0.1:8080")),
)
API_KEY = os.getenv(
    "ALEPHANT_API_KEY",
    os.getenv(
        "API_KEY",
        os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY", os.getenv("OPENAI_API_KEY", "sk-test")),
    ),
)
RUN_ID = os.getenv("RUN_ID", "run_langgraph_demo")
STEP_ID = os.getenv("STEP_ID", "step_tool_1")
HTTP_USER_AGENT = os.getenv("ALEPHANT_HTTP_USER_AGENT", DEFAULT_HTTP_USER_AGENT)


def request_json(path: str, payload: dict[str, Any]) -> dict[str, Any]:
    request = urllib.request.Request(
        GATEWAY_URL.rstrip("/") + path,
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "authorization": f"Bearer {API_KEY}",
            "accept": "application/json",
            "content-type": "application/json",
            "user-agent": HTTP_USER_AGENT,
            "Alephant-Agent-Id": "langgraph-demo",
            "Alephant-Run-Id": RUN_ID,
            "Alephant-Step-Id": STEP_ID,
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read() or b"{}")


def list_tools() -> list[dict[str, Any]]:
    return request_json("/v1/agent/tools/list", {}).get("tools", [])


def call_alephant_tool(tool_id: str, arguments: dict[str, Any]) -> dict[str, Any]:
    result = request_json(
        "/v1/agent/tools/call",
        {
            "tool_id": tool_id,
            "tool_call_id": f"call_langgraph_{tool_id.replace('.', '_')}",
            "run_id": RUN_ID,
            "step_id": STEP_ID,
            "arguments": arguments,
        },
    )
    text = json.dumps(result)
    for forbidden in ["Mcp-Session-Id", "initialize", "text/event-stream", "cacheHit", "targetHash"]:
        assert forbidden not in text
    return result


def build_langgraph_structured_tools() -> dict[str, Any]:
    tools = {}
    for descriptor in list_tools():
        framework_name = descriptor["frameworkToolName"]
        canonical_tool_id = descriptor["toolId"]

        def handler(query: str, tool_id: str = canonical_tool_id) -> dict[str, Any]:
            return call_alephant_tool(tool_id, {"query": query})

        handler.__name__ = framework_name
        if StructuredTool is not None:
            tools[framework_name] = StructuredTool.from_function(
                func=handler,
                name=framework_name,
                description=descriptor.get("description", ""),
            )
        else:
            tools[framework_name] = handler
    return tools


if __name__ == "__main__":
    registered_tools = build_langgraph_structured_tools()
    print(json.dumps(registered_tools["docs_search"]("refund policy"), indent=2))
