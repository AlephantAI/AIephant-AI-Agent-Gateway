#!/usr/bin/env python3
"""Mock logs-collector HTTP sink for Agent Event E2E validation."""

from __future__ import annotations

import argparse
import json
import threading
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

if __package__:
    from .e2e_support import append_jsonl, read_jsonl
else:
    from e2e_support import append_jsonl, read_jsonl


def redact_header(key: str, value: str) -> str:
    if key.lower() != "authorization":
        return value

    scheme = value.split(None, 1)[0] if value.strip() else ""
    if scheme:
        return f"{scheme} [redacted]"
    return "[redacted]"


class AgentEventSinkServer:
    def __init__(self, host: str, port: int, output_path: Path) -> None:
        self.host = host
        self.output_path = output_path
        self._write_lock = threading.Lock()
        self._server = ThreadingHTTPServer((host, port), self._handler_class())
        self.port = int(self._server.server_address[1])
        self.url = f"http://{host}:{self.port}/v1/log/agent-event"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def _handler_class(self) -> type[BaseHTTPRequestHandler]:
        output_path = self.output_path
        write_lock = self._write_lock

        class Handler(BaseHTTPRequestHandler):
            server_version = "alephant-agent-event-sink/1.0"

            def do_POST(self) -> None:
                if self.path != "/v1/log/agent-event":
                    self._write_json(HTTPStatus.NOT_FOUND, {"error": "not found"})
                    return

                try:
                    length = int(self.headers.get("content-length", "0"))
                except ValueError:
                    self._write_json(
                        HTTPStatus.BAD_REQUEST,
                        {"error": "invalid content-length"},
                    )
                    return

                raw = self.rfile.read(length) if length else b"{}"
                try:
                    body = json.loads(raw or b"{}")
                except json.JSONDecodeError as exc:
                    self._write_json(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
                    return

                with write_lock:
                    append_jsonl(
                        output_path,
                        {
                            "headers": {
                                key: redact_header(key, value)
                                for key, value in self.headers.items()
                                if key.lower()
                                in {"authorization", "content-type", "user-agent"}
                            },
                            "body": body,
                        },
                    )
                self._write_json(HTTPStatus.OK, {"ok": True})

            def _write_json(self, status: int, body: dict[str, Any]) -> None:
                payload = json.dumps(body).encode("utf-8")
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, fmt: str, *args: object) -> None:
                print("[agent-event-sink] " + fmt % args)

        return Handler

    def start(self) -> "AgentEventSinkServer":
        self.output_path.parent.mkdir(parents=True, exist_ok=True)
        if self.output_path.exists():
            self.output_path.unlink()
        self._thread.start()
        return self

    def stop(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)

    def events(self) -> list[Any]:
        return read_jsonl(self.output_path)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=9877)
    parser.add_argument("--output", default="agent-events.jsonl")
    args = parser.parse_args()

    server = AgentEventSinkServer(
        host=args.host,
        port=args.port,
        output_path=Path(args.output),
    ).start()
    print(f"mock Agent Event sink listening on {server.url}")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.stop()


if __name__ == "__main__":
    main()
