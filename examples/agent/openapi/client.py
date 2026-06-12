"""Shared OpenAPI Agent Tool demo client."""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any


def load_dotenv() -> None:
    for start in (Path.cwd(), Path(__file__).resolve()):
        for directory in [start, *start.parents]:
            dotenv = directory / ".env"
            if dotenv.is_file():
                for raw_line in dotenv.read_text(encoding="utf-8").splitlines():
                    line = raw_line.strip()
                    if not line or line.startswith("#"):
                        continue
                    if line.startswith("export "):
                        line = line[len("export ") :].strip()
                    if "=" not in line:
                        continue
                    key, value = line.split("=", 1)
                    key = key.strip()
                    if key and key not in os.environ:
                        os.environ[key] = clean_env_value(value)
                return


def clean_env_value(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value


def truthy(value: object) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on"}


def new_id(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex}"


class OpenApiToolClient:
    def __init__(self) -> None:
        load_dotenv()
        self.base_url = (
            os.getenv("AI_GATEWAY_BASE_URL")
            or os.getenv("GATEWAY_URL")
            or os.getenv("ALEPHANT_GATEWAY_URL")
            or "http://127.0.0.1:3000"
        ).rstrip("/")
        self.api_key = (
            os.getenv("API_KEY")
            or os.getenv("ALEPHANT_API_KEY")
            or os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY")
            or os.getenv("OPENAI_API_KEY")
        )
        self.agent_id = os.getenv("ALEPHANT_AGENT_ID") or "openapi-demo-agent"
        self.agent_name = os.getenv("ALEPHANT_AGENT_NAME") or "OpenAPI Demo Agent"
        self.run_id = os.getenv("ALEPHANT_RUN_ID") or new_id("run_openapi")
        self.debug_body = truthy(os.getenv("AI_GATEWAY_DEBUG_BODY", "true"))
        self.debug_headers = truthy(os.getenv("AI_GATEWAY_DEBUG_HEADERS"))
        self.timeout = float(os.getenv("ALEPHANT_AGENT_TIMEOUT_SECONDS", "20"))

    def list_tools(self) -> dict[str, Any]:
        return self.post(
            "/v1/agent/tools/list",
            {
                "source": "openapi-demo",
                "agent_id": self.agent_id,
                "agent_name": self.agent_name,
                "run_id": self.run_id,
                "capabilities": {"schema_dialect": "openai_function"},
            },
            step_id="step_list_tools",
        )

    def call_tool(
        self,
        *,
        tool_id: str,
        arguments: dict[str, Any],
        step_id: str = "step_openapi_tool",
        tool_call_id: str | None = None,
    ) -> dict[str, Any]:
        tool_call_id = tool_call_id or new_id("call_openapi")
        return self.post(
            "/v1/agent/tools/call",
            {
                "source": "openapi-demo",
                "agent_id": self.agent_id,
                "agent_name": self.agent_name,
                "run_id": self.run_id,
                "step_id": step_id,
                "tool_call_id": tool_call_id,
                "tool_id": tool_id,
                "arguments": arguments,
                "idempotency_key": f"{self.run_id}:{step_id}:{tool_call_id}",
            },
            step_id=step_id,
            tool_call_id=tool_call_id,
        )

    def call_with_refresh_once(
        self,
        *,
        tool_id: str,
        arguments: dict[str, Any],
    ) -> dict[str, Any]:
        response = self.call_tool(tool_id=tool_id, arguments=arguments)
        if response.get("agentAction") != "refresh_tools":
            return response
        self.list_tools()
        return self.call_tool(
            tool_id=tool_id,
            arguments=arguments,
            step_id="step_openapi_tool_retry",
        )

    def post(
        self,
        path: str,
        payload: dict[str, Any],
        *,
        step_id: str,
        tool_call_id: str | None = None,
    ) -> dict[str, Any]:
        if not self.api_key:
            raise SystemExit(
                "Set API_KEY, ALEPHANT_API_KEY, ALEPHANT_CONTROL_OPENROUTER_API_KEY, or OPENAI_API_KEY"
            )
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "Authorization": f"Bearer {self.api_key}",
            "Alephant-Agent-Id": self.agent_id,
            "Alephant-Agent-Name": self.agent_name,
            "Alephant-Run-Id": self.run_id,
            "Alephant-Step-Id": step_id,
            "User-Agent": "AlephantOpenApiAgentExample/1.0",
        }
        if tool_call_id:
            headers["Alephant-Tool-Call-Id"] = tool_call_id
        if self.debug_body:
            headers["alephant-debug-body"] = "true"
        if self.debug_headers:
            headers["alephant-debug-headers"] = "true"

        data = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=data,
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                body = response.read().decode("utf-8")
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"{path} failed: HTTP {exc.code}: {detail}") from exc
        return json.loads(body) if body else {}


def print_json(value: Any) -> None:
    json.dump(value, sys.stdout, indent=2, sort_keys=True)
    print()


def descriptor_by_tool_id(list_response: dict[str, Any], tool_id: str) -> dict[str, Any]:
    for tool in list_response.get("tools", []):
        if tool.get("toolId") == tool_id:
            return tool
    raise RuntimeError(f"tool_id {tool_id!r} not found in tools/list response")


def envelope_to_text(envelope: dict[str, Any]) -> str:
    if envelope.get("error"):
        error = envelope["error"]
        return f"{envelope.get('status')}: {error.get('code')} - {error.get('message')}"
    return json.dumps(envelope.get("output"), sort_keys=True)
