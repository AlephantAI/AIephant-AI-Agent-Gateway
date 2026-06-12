#!/usr/bin/env python3
"""Minimal MCP Streamable HTTP mock server for docs.search examples."""

from __future__ import annotations

import json
import os
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


HOST = os.getenv("MCP_STREAMABLE_MOCK_HOST", "127.0.0.1")
PORT = int(os.getenv("MCP_STREAMABLE_MOCK_PORT", "8766"))
SESSION_ID = "example-session-1"
RECORD_FILE = os.getenv("MCP_STREAMABLE_MOCK_RECORD_FILE", "")
RESPONSE_MODE = os.getenv("MCP_STREAMABLE_MOCK_RESPONSE_MODE", "success")


def append_record(record: dict[str, Any]) -> None:
    if not RECORD_FILE:
        return
    path = Path(RECORD_FILE)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")


class Handler(BaseHTTPRequestHandler):
    server_version = "mcp-streamable-mock/1.0"

    def _read_json(self) -> dict[str, Any]:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        body = json.loads(raw or b"{}")
        if not isinstance(body, dict):
            raise ValueError("JSON-RPC body must be an object")
        return body

    def _write_json(
        self,
        status: int,
        body: dict[str, Any],
        *,
        session_id: bool = False,
    ) -> None:
        payload = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        if session_id:
            self.send_header("mcp-session-id", SESSION_ID)
        self.end_headers()
        self.wfile.write(payload)

    def _write_empty(self, status: int) -> None:
        self.send_response(status)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_POST(self) -> None:
        if self.path != "/mcp":
            self._write_json(HTTPStatus.NOT_FOUND, {"error": "not found"})
            return

        try:
            body = self._read_json()
        except (json.JSONDecodeError, ValueError) as err:
            self._write_json(
                HTTPStatus.BAD_REQUEST,
                {
                    "jsonrpc": "2.0",
                    "id": None,
                    "error": {"code": -32700, "message": str(err)},
                },
            )
            return

        append_record(
            {
                "time": time.time(),
                "path": self.path,
                "headers": {
                    "accept": self.headers.get("accept"),
                    "content-type": self.headers.get("content-type"),
                    "mcp-session-id": self.headers.get("mcp-session-id"),
                    "mcp-protocol-version": self.headers.get("mcp-protocol-version"),
                },
                "body": body,
            }
        )

        method = body.get("method")
        request_id = body.get("id")

        if method == "initialize":
            self._write_json(
                HTTPStatus.OK,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "alephant-example-mcp",
                            "version": "1.0.0",
                        },
                    },
                },
                session_id=True,
            )
            return

        if method == "notifications/initialized":
            self._write_empty(HTTPStatus.ACCEPTED)
            return

        if method == "tools/call":
            params = body.get("params", {})
            args = params.get("arguments", {}) if isinstance(params, dict) else {}
            is_error = RESPONSE_MODE == "business_error"
            text = (
                "mock streamable business error"
                if is_error
                else "mock streamable result"
            )
            self._write_json(
                HTTPStatus.OK,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": text,
                            }
                        ],
                        "structuredContent": {"echo": args, "mode": RESPONSE_MODE},
                        "isError": is_error,
                    },
                },
            )
            return

        self._write_json(
            HTTPStatus.BAD_REQUEST,
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": "Method not found"},
            },
        )

    def log_message(self, fmt: str, *args: object) -> None:
        print("[mcp-streamable-mock] " + fmt % args)


def main() -> None:
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"mock MCP Streamable HTTP server listening on http://{HOST}:{PORT}/mcp")
    server.serve_forever()


if __name__ == "__main__":
    main()
