"""Shared helpers for Agent Gateway framework examples.

The framework examples intentionally use only the Python standard library.
Each example sends native framework-shaped events to `/v1/agent/events`; the
gateway adapter maps those events into Alephant's standard taxonomy.
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


EVENT_VERSION = "2026-05-27"
DEFAULT_HTTP_USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36 "
    "AlephantAgentExample/1.0"
)


class AgentEventClient:
    def __init__(
        self,
        *,
        base_url: str,
        api_key: str | None,
        dry_run: bool = False,
        debug_headers: bool = False,
        debug_body: bool = False,
        http_user_agent: str = DEFAULT_HTTP_USER_AGENT,
        timeout_seconds: float = 20.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.dry_run = dry_run or not api_key
        self.debug_headers = debug_headers
        self.debug_body = debug_body
        self.http_user_agent = http_user_agent
        self.timeout_seconds = timeout_seconds

    @classmethod
    def from_env(cls) -> "AgentEventClient":
        load_dotenv()
        return cls(
            base_url=os.getenv("GATEWAY_BASE")
            or os.getenv("ALEPHANT_GATEWAY_URL", "http://127.0.0.1:8080"),
            api_key=os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY")
            or os.getenv("ALEPHANT_API_KEY"),
            dry_run=truthy(os.getenv("ALEPHANT_AGENT_DRY_RUN")),
            debug_headers=truthy(os.getenv("AI_GATEWAY_DEBUG_HEADERS")),
            debug_body=truthy(os.getenv("AI_GATEWAY_DEBUG_BODY")),
            http_user_agent=os.getenv("ALEPHANT_HTTP_USER_AGENT", DEFAULT_HTTP_USER_AGENT),
        )

    def emit_events(self, *, source: str, events: list[dict[str, Any]]) -> dict[str, Any]:
        return self._post_json("/v1/agent/events", {"source": source, "events": events})

    @staticmethod
    def response_allows_event(response: dict[str, Any], event_id: str) -> bool:
        for decision in response.get("decisions", []):
            if decision.get("eventId") == event_id:
                return bool(decision.get("allowed", True))
        return bool(response.get("dry_run"))

    def _post_json(self, path: str, payload: dict[str, Any]) -> dict[str, Any]:
        headers = self._request_headers()
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

    def _request_headers(self) -> dict[str, str]:
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": self.http_user_agent,
        }
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"
        if self.debug_headers:
            headers["alephant-debug-headers"] = "true"
        if self.debug_body:
            headers["alephant-debug-body"] = "true"
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


def base_event(
    event_type: str,
    *,
    agent_id: str,
    run_id: str,
    agent_name: str | None = None,
    metadata: dict[str, Any] | None = None,
    **raw_fields: Any,
) -> dict[str, Any]:
    event = {
        "version": EVENT_VERSION,
        "event_id": f"evt_{uuid.uuid4().hex}",
        "type": event_type,
        "agent_id": agent_id,
        "run_id": run_id,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "metadata": metadata or {},
    }
    if agent_name:
        event["agent_name"] = agent_name
    event.update({key: value for key, value in raw_fields.items() if value is not None})
    return event


def default_agent_name(value: str) -> str:
    return os.getenv("ALEPHANT_AGENT_NAME") or value


def default_run_id(prefix: str) -> str:
    return os.getenv("ALEPHANT_RUN_ID") or f"{prefix}_{uuid.uuid4().hex}"


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
