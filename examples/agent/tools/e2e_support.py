#!/usr/bin/env python3
"""Shared helpers for Agent Tools E2E examples."""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Callable, Iterable


class E2EAssertionError(AssertionError):
    """Raised when the E2E runner cannot prove the expected gateway behavior."""


def repo_root() -> Path:
    return Path(__file__).resolve().parents[3]


def load_dotenv(path: Path | None = None) -> None:
    dotenv = path or repo_root() / ".env"
    if not dotenv.is_file():
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
        if key and key not in os.environ:
            os.environ[key] = clean_env_value(value)


def clean_env_value(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value


def random_id(prefix: str) -> str:
    return f"{prefix}_{uuid.uuid4().hex}"


def env_gateway_url() -> str:
    return (
        os.getenv("GATEWAY_URL")
        or os.getenv("ALEPHANT_GATEWAY_URL")
        or "http://127.0.0.1:8080"
    ).rstrip("/")


def env_api_key() -> str:
    api_key = (
        os.getenv("ALEPHANT_API_KEY")
        or os.getenv("API_KEY")
        or os.getenv("ALEPHANT_CONTROL_OPENROUTER_API_KEY")
        or os.getenv("OPENAI_API_KEY")
        or ""
    )
    if not api_key:
        raise E2EAssertionError(
            "set ALEPHANT_API_KEY, API_KEY, ALEPHANT_CONTROL_OPENROUTER_API_KEY, "
            "or OPENAI_API_KEY before running the E2E loop"
        )
    return api_key


def json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2)


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json_dumps(value) + "\n", encoding="utf-8")


def append_jsonl(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(value, ensure_ascii=False, sort_keys=True) + "\n")


def read_jsonl(path: Path) -> list[Any]:
    if not path.is_file():
        return []
    rows: list[Any] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def post_json(
    *,
    base_url: str,
    path: str,
    payload: dict[str, Any],
    api_key: str,
    timeout_seconds: float = 20.0,
    extra_headers: dict[str, str] | None = None,
) -> dict[str, Any]:
    data = json.dumps(payload).encode("utf-8")
    headers = {
        "Accept": "application/json",
        "Content-Type": "application/json",
        "Authorization": f"Bearer {api_key}",
    }
    if extra_headers:
        headers.update(extra_headers)

    url = f"{base_url.rstrip('/')}{path}"
    request = urllib.request.Request(
        url,
        data=data,
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
            raw = response.read()
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")
        raise E2EAssertionError(
            f"{url} failed: HTTP status={exc.code} body={detail}"
        ) from exc
    except urllib.error.URLError as exc:
        raise E2EAssertionError(f"{url} failed: {exc}") from exc
    except TimeoutError as exc:
        raise E2EAssertionError(f"{url} timed out: {exc}") from exc

    if not raw:
        return {}
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        preview = raw.decode("utf-8", errors="replace")[:500]
        raise E2EAssertionError(
            f"{url} returned invalid JSON: {exc}; raw body preview={preview!r}"
        ) from exc


def get_first(mapping: dict[str, Any], names: Iterable[str], default: Any = None) -> Any:
    for name in names:
        if name in mapping:
            return mapping[name]
    return default


def parse_metadata(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if isinstance(value, str) and value.strip():
        parsed = json.loads(value)
        if isinstance(parsed, dict):
            return parsed
    return {}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise E2EAssertionError(message)


def wait_until(
    description: str,
    timeout_seconds: float,
    callback: Callable[[], Any],
) -> Any:
    deadline = time.time() + timeout_seconds
    last_value: Any = None
    while time.time() < deadline:
        last_value = callback()
        if last_value:
            return last_value
        time.sleep(0.2)
    raise E2EAssertionError(f"timed out waiting for {description}; last={last_value!r}")


def self_test() -> None:
    require(random_id("run").startswith("run_"), "random_id prefix failed")
    require(
        get_first(
            {"toolExecutionId": "x"},
            ["tool_execution_id", "toolExecutionId"],
        )
        == "x",
        "alias lookup failed",
    )
    require(
        parse_metadata('{"billing":{"costType":"tool"}}')["billing"]["costType"]
        == "tool",
        "metadata parse failed",
    )
    print("e2e_support self-test passed")


if __name__ == "__main__":
    self_test()
