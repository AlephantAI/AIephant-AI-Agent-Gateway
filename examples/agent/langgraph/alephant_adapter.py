"""Small Alephant Agent Gateway adapter for LangGraph examples.

The adapter intentionally uses only the Python standard library so the example
scripts can compile without installing extra dependencies. LangGraph itself is
only imported by the runnable examples.
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


DEFAULT_MODEL = "openai/gpt-4o-mini"
DEFAULT_HTTP_USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36 "
    "AlephantAgentExample/1.0"
)


@dataclass(frozen=True)
class AgentStepContext:
    step_id: str
    step_kind: str
    graph_node: str | None = None
    parent_step_id: str | None = None
    tool_call_id: str | None = None
    handoff_id: str | None = None
    attempt: int | None = None
    input_hash: str | None = None


class AlephantAgentAdapter:
    def __init__(
        self,
        *,
        base_url: str,
        api_key: str | None,
        agent_id: str,
        run_id: str,
        agent_name: str | None = None,
        model: str = DEFAULT_MODEL,
        dry_run: bool = False,
        debug_headers: bool = False,
        debug_body: bool = False,
        http_user_agent: str = DEFAULT_HTTP_USER_AGENT,
        timeout_seconds: float = 20.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.agent_id = agent_id
        self.run_id = run_id
        self.agent_name = agent_name
        self.model = model
        self.dry_run = dry_run or not api_key
        self.debug_headers = debug_headers
        self.debug_body = debug_body
        self.http_user_agent = http_user_agent
        self.timeout_seconds = timeout_seconds

    @classmethod
    def from_env(
        cls,
        *,
        agent_id: str,
        run_id: str | None = None,
        default_agent_name: str | None = None,
    ) -> "AlephantAgentAdapter":
        load_dotenv()
        return cls(
            base_url=os.getenv("GATEWAY_BASE")
            or os.getenv("ALEPHANT_GATEWAY_URL", "http://127.0.0.1:8080"),
            api_key=os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY")
            or os.getenv("ALEPHANT_API_KEY"),
            agent_id=os.getenv("ALEPHANT_AGENT_ID") or agent_id,
            run_id=run_id or f"run_{uuid.uuid4().hex}",
            agent_name=os.getenv("ALEPHANT_AGENT_NAME") or default_agent_name,
            model=os.getenv("ALEPHANT_MODEL", DEFAULT_MODEL),
            dry_run=os.getenv("ALEPHANT_AGENT_DRY_RUN", "").lower() in {"1", "true", "yes"},
            debug_headers=truthy(os.getenv("AI_GATEWAY_DEBUG_HEADERS")),
            debug_body=truthy(os.getenv("AI_GATEWAY_DEBUG_BODY")),
            http_user_agent=os.getenv("ALEPHANT_HTTP_USER_AGENT", DEFAULT_HTTP_USER_AGENT),
        )

    def agent_headers(self, context: AgentStepContext) -> dict[str, str]:
        headers = {
            "Alephant-Agent-Id": self.agent_id,
            "Alephant-Run-Id": self.run_id,
            "Alephant-Step-Id": context.step_id,
            "Alephant-Step-Kind": context.step_kind,
            "Alephant-Step-Source": "runtime",
        }
        if self.agent_name:
            headers["Alephant-Agent-Name"] = self.agent_name
        optional = {
            "Alephant-Parent-Step-Id": context.parent_step_id,
            "Alephant-Tool-Call-Id": context.tool_call_id,
            "Alephant-Handoff-Id": context.handoff_id,
            "Alephant-Graph-Node": context.graph_node,
            "Alephant-Step-Attempt": context.attempt,
            "Alephant-Step-Input-Hash": context.input_hash,
        }
        headers.update({key: str(value) for key, value in optional.items() if value is not None})
        return headers

    def emit(
        self,
        event_type: str,
        *,
        context: AgentStepContext | None = None,
        step_id: str | None = None,
        step_kind: str | None = None,
        event_phase: str | None = None,
        policy_stage: str | None = None,
        policy_mode: str | None = None,
        event_source_trust: str | None = None,
        metadata: dict[str, Any] | None = None,
        extra_headers: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        event = self.build_event(
            event_type,
            context=context,
            step_id=step_id,
            step_kind=step_kind,
            event_phase=event_phase,
            policy_stage=policy_stage,
            policy_mode=policy_mode,
            event_source_trust=event_source_trust,
            metadata=metadata,
        )
        return self.emit_batch([event], extra_headers=extra_headers)

    def build_event(
        self,
        event_type: str,
        *,
        context: AgentStepContext | None = None,
        step_id: str | None = None,
        step_kind: str | None = None,
        event_phase: str | None = None,
        policy_stage: str | None = None,
        policy_mode: str | None = None,
        event_source_trust: str | None = None,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        event = {
            "version": "2026-05-27",
            "event_id": f"evt_{uuid.uuid4().hex}",
            "type": event_type,
            "agent_id": self.agent_id,
            "run_id": self.run_id,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "metadata": metadata or {},
        }
        if self.agent_name:
            event["agent_name"] = self.agent_name
        if context is not None:
            event.update(
                {
                    "step_id": context.step_id,
                    "parent_step_id": context.parent_step_id,
                    "tool_call_id": context.tool_call_id,
                    "handoff_id": context.handoff_id,
                    "graph_node": context.graph_node,
                    "step_kind": context.step_kind,
                    "step_source": "runtime",
                    "step_confidence": "high",
                    "attempt": context.attempt,
                    "input_hash": context.input_hash,
                }
            )
        elif step_id is not None or step_kind is not None:
            if step_id is not None:
                event["step_id"] = step_id
            if step_kind is not None:
                event["step_kind"] = step_kind
            event["step_source"] = "runtime"
            event["step_confidence"] = "high"
        optional = {
            "event_phase": event_phase,
            "policy_stage": policy_stage,
            "policy_mode": policy_mode,
            "event_source_trust": event_source_trust,
        }
        event.update({key: value for key, value in optional.items() if value is not None})
        return event

    def emit_batch(
        self,
        events: list[dict[str, Any]],
        *,
        extra_headers: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        if self.agent_name:
            for event in events:
                event.setdefault("agent_name", self.agent_name)
        return self._post_json("/v1/agent/events", {"events": events}, extra_headers=extra_headers)

    @staticmethod
    def response_allows_event(response: dict[str, Any], event_id: str) -> bool:
        for decision in response.get("decisions", []):
            if decision.get("eventId") == event_id:
                return bool(decision.get("allowed", True))
        return bool(response.get("dry_run"))

    def chat_completion(self, *, messages: list[dict[str, str]], context: AgentStepContext) -> str:
        payload = {
            "model": self.model,
            "messages": messages,
            "temperature": 0,
        }
        if self.dry_run:
            self._post_json(
                "/v1/chat/completions",
                payload,
                extra_headers=self.agent_headers(context),
            )
            return "dry-run response"

        response = self._post_json(
            "/v1/chat/completions",
            payload,
            extra_headers=self.agent_headers(context),
        )
        choices = response.get("choices") or []
        if not choices:
            return ""
        message = choices[0].get("message") or {}
        return str(message.get("content") or "")

    def _post_json(
        self,
        path: str,
        payload: dict[str, Any],
        *,
        extra_headers: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        headers = self._request_headers(extra_headers)
        debug_headers = self._debug_enabled("headers", headers)
        debug_body = self._debug_enabled("body", headers)

        if self.dry_run:
            if debug_headers:
                print_json(f"dry_run.request_headers {path}", redact_headers(headers))
            if debug_body:
                print_json(f"dry_run.request_body {path}", payload)
            if debug_headers:
                print_json(f"dry_run.response_headers {path}", {"content-type": "application/json"})
            if debug_body:
                print_json(f"dry_run.response_body {path}", {"dry_run": True})
            if not debug_headers and not debug_body:
                print_json(f"dry_run.post {path}", payload)
            return {"dry_run": True}

        data = json.dumps(payload).encode("utf-8")
        if debug_headers:
            print_json(f"request_headers {path}", redact_headers(headers))
        if debug_body:
            print_json(f"request_body {path}", payload)
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=data,
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout_seconds) as response:
                body = response.read()
                if debug_headers:
                    print_json(f"response_headers {path}", dict(response.headers.items()))
                if debug_body:
                    print_text(f"response_body {path}", body.decode("utf-8", errors="replace"))
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")
            if debug_headers:
                print_json(f"response_headers {path}", dict(exc.headers.items()))
            if debug_body:
                print_text(f"response_body {path}", detail)
            raise RuntimeError(f"Alephant request failed: {exc.code} {detail}") from exc
        if not body:
            return {}
        return json.loads(body)

    def _request_headers(self, extra_headers: dict[str, str] | None = None) -> dict[str, str]:
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": self.http_user_agent,
        }
        if self.debug_headers:
            headers["alephant-debug-headers"] = "true"
        if self.debug_body:
            headers["alephant-debug-body"] = "true"
        if extra_headers:
            headers.update(extra_headers)
        return headers

    def _debug_enabled(self, target: str, headers: dict[str, str]) -> bool:
        if target == "headers" and self.debug_headers:
            return True
        if target == "body" and self.debug_body:
            return True

        header_name = f"alephant-debug-{target}"
        for key, value in headers.items():
            if key.lower() == header_name:
                return truthy(value)
        return False


def truthy(value: object) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on"}


def load_dotenv() -> None:
    dotenv = find_dotenv()
    if dotenv is None:
        return

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
        if not key or key in os.environ:
            continue
        os.environ[key] = clean_env_value(value)


def find_dotenv() -> Path | None:
    for start in (Path.cwd(), Path(__file__).resolve()):
        for directory in [start, *start.parents]:
            dotenv = directory / ".env"
            if dotenv.is_file():
                return dotenv
    return None


def clean_env_value(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value


def redact_headers(headers: dict[str, str]) -> dict[str, str]:
    redacted = {}
    for key, value in headers.items():
        if key.lower() in {"authorization", "x-api-key", "api-key"}:
            redacted[key] = "[redacted]"
        else:
            redacted[key] = value
    return redacted


def print_json(label: str, value: Any) -> None:
    print(f"\n[{label}]")
    print(json.dumps(value, indent=2, sort_keys=True, default=str))


def print_text(label: str, value: str) -> None:
    print(f"\n[{label}]")
    print(value)


def sleep_briefly() -> None:
    time.sleep(float(os.getenv("ALEPHANT_AGENT_STEP_DELAY_SECONDS", "0.1")))
