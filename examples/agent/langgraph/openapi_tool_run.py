"""LangGraph-compatible OpenAPI Agent Tool demo."""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any, TypedDict


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "openapi"))

from client import (  # noqa: E402
    OpenApiToolClient,
    descriptor_by_tool_id,
    envelope_to_text,
    print_json,
)


class OpenApiState(TypedDict, total=False):
    ticket_id: str
    tool_id: str
    framework_tool_name: str
    result: str
    schema_invalid: str


def run_without_langgraph(client: OpenApiToolClient) -> dict[str, Any]:
    tool_id = os.getenv("OPENAPI_TOOL_ID", "support.get_ticket")
    listed = client.list_tools()
    descriptor = descriptor_by_tool_id(listed, tool_id)
    response = client.call_with_refresh_once(
        tool_id=tool_id,
        arguments={"ticket_id": os.getenv("OPENAPI_TICKET_ID", "T-1001")},
    )
    schema_invalid = client.call_tool(
        tool_id=tool_id,
        arguments={"ticket_id": 12345},
        step_id="step_langgraph_schema_invalid",
    )
    return {
        "framework": "langgraph-compatible",
        "tool_id": tool_id,
        "framework_tool_name": descriptor.get("frameworkToolName"),
        "result": envelope_to_text(response),
        "schema_invalid": envelope_to_text(schema_invalid),
    }


def build_graph(client: OpenApiToolClient):
    try:
        from langgraph.graph import END, StateGraph
    except ImportError:
        return None

    def list_node(state: OpenApiState) -> OpenApiState:
        listed = client.list_tools()
        descriptor = descriptor_by_tool_id(listed, state["tool_id"])
        return {"framework_tool_name": descriptor.get("frameworkToolName", "")}

    def call_node(state: OpenApiState) -> OpenApiState:
        response = client.call_with_refresh_once(
            tool_id=state["tool_id"],
            arguments={"ticket_id": state["ticket_id"]},
        )
        return {"result": envelope_to_text(response)}

    graph = StateGraph(OpenApiState)
    graph.add_node("list_tools", list_node)
    graph.add_node("call_tool", call_node)
    graph.set_entry_point("list_tools")
    graph.add_edge("list_tools", "call_tool")
    graph.add_edge("call_tool", END)
    return graph.compile()


def main() -> None:
    client = OpenApiToolClient()
    graph = build_graph(client)
    if graph is None:
        print_json(run_without_langgraph(client))
        return

    result = graph.invoke(
        {
            "ticket_id": os.getenv("OPENAPI_TICKET_ID", "T-1001"),
            "tool_id": os.getenv("OPENAPI_TOOL_ID", "support.get_ticket"),
        }
    )
    print_json({"framework": "langgraph", **result})


if __name__ == "__main__":
    main()
