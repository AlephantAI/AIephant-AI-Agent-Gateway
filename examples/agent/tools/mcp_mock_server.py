#!/usr/bin/env python3
"""Minimal MCP JSON-RPC mock server for docs.search examples."""

from __future__ import annotations

import http.server
import json
import os
from http import HTTPStatus
from typing import Any


HOST = "127.0.0.1"
PORT = int(os.getenv("MCP_MOCK_PORT", "9876"))


class MCPRequestHandler(http.server.BaseHTTPRequestHandler):
    """Handle MCP HTTP requests on POST /mcp."""

    server_version = "mcp-mock-server/1.0"

    def _coerce_request_id(self, request_id: Any) -> str | int | float | None:
        if request_id is None:
            return None
        if isinstance(request_id, bool):
            return None
        if isinstance(request_id, (str, int, float)):
            return request_id
        return None

    def _json_rpc_error(
        self, request_id: Any, code: int, message: str, *, status: int = HTTPStatus.OK
    ) -> tuple[dict[str, Any], int]:
        return {
            "jsonrpc": "2.0",
            "id": self._coerce_request_id(request_id),
            "error": {
                "code": code,
                "message": message,
            },
        }, status

    def _write_response(self, status: int, body: dict[str, Any]) -> None:
        response = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def _log(self, msg: str) -> None:
        print(f"[{self.client_address[0]}:{self.client_address[1]}] {msg}")

    def do_POST(self) -> None:
        if self.path != "/mcp":
            self._write_response(HTTPStatus.NOT_FOUND, {"error": "not found"})
            return

        raw_content_length = self.headers.get("Content-Length")
        if raw_content_length is None:
            response, status = self._json_rpc_error(
                None, -32600, "Invalid Request"
            )
            self._write_response(HTTPStatus.BAD_REQUEST, response)
            return

        try:
            content_length = int(raw_content_length)
        except ValueError:
            self._log(f"invalid content-length header: {raw_content_length!r}")
            response, status = self._json_rpc_error(
                None, -32600, "Invalid Request"
            )
            self._write_response(HTTPStatus.BAD_REQUEST, response)
            return

        if content_length < 0:
            self._log(f"negative content-length header: {raw_content_length!r}")
            response, status = self._json_rpc_error(
                None, -32600, "Invalid Request"
            )
            self._write_response(HTTPStatus.BAD_REQUEST, response)
            return

        try:
            raw_body = (
                self.rfile.read(content_length).decode("utf-8")
                if content_length
                else ""
            )
        except UnicodeDecodeError as err:
            self._log(f"invalid utf-8 body: {err}")
            response, status = self._json_rpc_error(
                None, -32700, f"Parse error: {err}"
            )
            self._write_response(HTTPStatus.BAD_REQUEST, response)
            return

        try:
            body = json.loads(raw_body) if raw_body else {}
        except json.JSONDecodeError as err:
            self._log("invalid JSON body")
            response, status = self._json_rpc_error(
                None, -32700, f"Parse error: {err}"
            )
            self._write_response(HTTPStatus.BAD_REQUEST, response)
            return

        if not isinstance(body, dict):
            self._log(f"invalid request body type={type(body).__name__!r}")
            response, status = self._json_rpc_error(None, -32600, "Invalid Request")
            self._write_response(status, response)
            return

        req_id = body.get("id")
        method = body.get("method")
        params = body.get("params", {})

        if not isinstance(method, str):
            self._log(f"invalid request method type={type(method).__name__!r} id={req_id!r}")
            response, status = self._json_rpc_error(req_id, -32600, "Invalid Request")
            self._write_response(status, response)
            return

        self._log(f"mcp request method={method!r} id={req_id!r}")

        if method == "initialize":
            if not isinstance(params, dict) and params is not None:
                response, status = self._json_rpc_error(req_id, -32602, "Invalid params")
                self._write_response(status, response)
                return
            result = {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "mock-mcp-server", "version": "1.0.0"},
                },
            }
            self._write_response(HTTPStatus.OK, result)
            self._log(f"initialize handled id={req_id!r}")
            return

        if method == "tools/call":
            if not isinstance(params, dict):
                response, status = self._json_rpc_error(req_id, -32602, "Invalid params")
                self._write_response(status, response)
                return

            name = params.get("name", "")
            arguments = params.get("arguments", {})
            if not isinstance(name, str):
                response, status = self._json_rpc_error(req_id, -32602, "Invalid params")
                self._write_response(status, response)
                return
            if not isinstance(arguments, dict):
                response, status = self._json_rpc_error(req_id, -32602, "Invalid params")
                self._write_response(status, response)
                return

            self._log(f"tool call tool={name!r} args={arguments!r}")
            result = {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [
                        {"type": "text", "text": f"mock mcp result for {name}"}
                    ],
                    "structuredContent": {
                        "tool": name,
                        "arguments": arguments,
                    },
                    "isError": False,
                },
            }
            self._write_response(HTTPStatus.OK, result)
            self._log(f"tools/call handled id={req_id!r}")
            return

        self._log(f"unknown method {method!r} id={req_id!r}")
        response, status = self._json_rpc_error(req_id, -32601, "Method not found")
        self._write_response(status, response)

    def do_GET(self) -> None:
        self.send_response(HTTPStatus.METHOD_NOT_ALLOWED)
        self.end_headers()

    def log_message(self, format: str, *args: object) -> None:  # pragma: no cover
        # Keep logs concise and delegated to _log.
        return


def main() -> None:
    with http.server.ThreadingHTTPServer((HOST, PORT), MCPRequestHandler) as server:
        print(f"mcp mock server listening on http://{HOST}:{PORT}/mcp")
        server.serve_forever()


if __name__ == "__main__":
    main()
