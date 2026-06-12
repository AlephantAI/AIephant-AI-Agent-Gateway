#!/usr/bin/env python3
"""Minimal traditional MCP SSE mock server for docs.search examples."""

from __future__ import annotations

import json
import os
import queue
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


HOST = os.getenv("MCP_SSE_HOST", "127.0.0.1")
PORT = int(os.getenv("MCP_SSE_PORT", "9118"))
PROTOCOL_VERSION = "2024-11-05"
EVENTS: "queue.Queue[dict[str, Any]]" = queue.Queue()


class Handler(BaseHTTPRequestHandler):
    server_version = "mcp-sse-mock/1.0"

    def log_message(self, fmt: str, *args: object) -> None:
        print("[mcp-sse-mock] " + fmt % args)

    def do_GET(self) -> None:
        if self.path != "/sse":
            self.send_error(HTTPStatus.NOT_FOUND)
            return

        self.send_response(HTTPStatus.OK)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        self.wfile.write(b"event: endpoint\ndata: /message\n\n")
        self.wfile.flush()

        while True:
            event = EVENTS.get()
            payload = json.dumps(event, separators=(",", ":")).encode()
            try:
                self.wfile.write(b"data: " + payload + b"\n\n")
                self.wfile.flush()
            except BrokenPipeError:
                return

    def do_POST(self) -> None:
        if self.path != "/message":
            self.send_error(HTTPStatus.NOT_FOUND)
            return

        try:
            body = self._read_json()
        except (json.JSONDecodeError, ValueError) as err:
            self._write_empty(HTTPStatus.BAD_REQUEST)
            print(f"[mcp-sse-mock] bad JSON-RPC request: {err}", flush=True)
            return

        method = body.get("method")
        request_id = body.get("id")

        if method == "initialize":
            EVENTS.put(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "alephant-mock-mcp-sse",
                            "version": "1",
                        },
                    },
                }
            )
            self._write_empty(HTTPStatus.ACCEPTED)
            return

        if method == "notifications/initialized":
            self._write_empty(HTTPStatus.ACCEPTED)
            return

        if method == "tools/call":
            params = body.get("params", {})
            arguments = params.get("arguments", {}) if isinstance(params, dict) else {}
            query = arguments.get("query", "")
            if query == "timeout":
                self._write_empty(HTTPStatus.ACCEPTED)
                return

            EVENTS.put(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": f"mock docs result for: {query}",
                            }
                        ],
                        "structuredContent": {"echo": arguments},
                        "isError": query == "business-error",
                    },
                }
            )
            self._write_empty(HTTPStatus.ACCEPTED)
            return

        EVENTS.put(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": "Method not found"},
            }
        )
        self._write_empty(HTTPStatus.ACCEPTED)

    def _read_json(self) -> dict[str, Any]:
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length) if length else b"{}"
        body = json.loads(raw or b"{}")
        if not isinstance(body, dict):
            raise ValueError("JSON-RPC body must be an object")
        return body

    def _write_empty(self, status: HTTPStatus) -> None:
        self.send_response(status)
        self.send_header("content-length", "0")
        self.end_headers()


def main() -> None:
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"MCP SSE mock listening on http://{HOST}:{PORT}/sse", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
