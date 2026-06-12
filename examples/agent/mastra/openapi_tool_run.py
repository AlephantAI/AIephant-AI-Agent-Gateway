"""Mastra-compatible OpenAPI Agent Tool demo.

This Python variant is intentionally dependency-light. It demonstrates the same
descriptor conversion and tool_id execution path a Mastra tool adapter would use.
"""

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


def mastra_tool_shape(descriptor: dict[str, Any]) -> dict[str, Any]:
    return {
        "id": descriptor.get("frameworkToolName"),
        "description": descriptor.get("description"),
        "inputSchema": descriptor.get("inputSchema"),
        "metadata": descriptor.get("metadata", {}),
    }


def main() -> None:
    client = OpenApiToolClient()
    tool_id = os.getenv("OPENAPI_TOOL_ID", "support.get_ticket")
    listed = client.list_tools()
    descriptor = descriptor_by_tool_id(listed, tool_id)
    tool_shape = mastra_tool_shape(descriptor)
    response = client.call_with_refresh_once(
        tool_id=tool_id,
        arguments={"ticket_id": os.getenv("OPENAPI_TICKET_ID", "T-1001")},
    )
    schema_invalid = client.call_tool(
        tool_id=tool_id,
        arguments={"ticket_id": 12345},
        step_id="step_mastra_schema_invalid",
    )
    print_json(
        {
            "framework": "mastra-compatible",
            "registered_tool": tool_shape,
            "tool_id": tool_id,
            "tool_output": envelope_to_text(response),
            "schema_invalid": envelope_to_text(schema_invalid),
        }
    )


if __name__ == "__main__":
    main()
