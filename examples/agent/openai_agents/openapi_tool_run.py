"""OpenAI Agents SDK-compatible OpenAPI Agent Tool demo."""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Any


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "openapi"))

from client import (  # noqa: E402
    OpenApiToolClient,
    descriptor_by_tool_id,
    envelope_to_text,
    print_json,
)


def raw_adapter_demo(client: OpenApiToolClient) -> dict[str, Any]:
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
        step_id="step_openai_agents_schema_invalid",
    )
    return {
        "framework": "openai-agents-compatible",
        "sdk_installed": False,
        "registered_tool_name": descriptor.get("frameworkToolName"),
        "tool_id": tool_id,
        "tool_output": envelope_to_text(response),
        "schema_invalid": envelope_to_text(schema_invalid),
    }


def sdk_adapter_demo(client: OpenApiToolClient) -> dict[str, Any]:
    try:
        from agents import function_tool  # type: ignore
    except ImportError:
        return raw_adapter_demo(client)

    tool_id = os.getenv("OPENAPI_TOOL_ID", "support.get_ticket")
    listed = client.list_tools()
    descriptor = descriptor_by_tool_id(listed, tool_id)

    @function_tool(name_override=descriptor.get("frameworkToolName"))
    def gateway_openapi_tool(ticket_id: str) -> str:
        response = client.call_with_refresh_once(
            tool_id=tool_id,
            arguments={"ticket_id": ticket_id},
        )
        return envelope_to_text(response)

    response = client.call_with_refresh_once(
        tool_id=tool_id,
        arguments={"ticket_id": os.getenv("OPENAPI_TICKET_ID", "T-1001")},
    )
    schema_invalid = client.call_tool(
        tool_id=tool_id,
        arguments={"ticket_id": 12345},
        step_id="step_openai_agents_schema_invalid",
    )
    return {
        "framework": "openai-agents",
        "sdk_installed": True,
        "registered_tool_name": descriptor.get("frameworkToolName"),
        "tool_id": tool_id,
        "tool_registered": str(gateway_openapi_tool),
        "tool_output": envelope_to_text(response),
        "schema_invalid": envelope_to_text(schema_invalid),
    }


def main() -> None:
    print_json(sdk_adapter_demo(OpenApiToolClient()))


if __name__ == "__main__":
    main()
