"""Local mock Support API for OpenAPI Agent Tool demos."""

from __future__ import annotations

import json
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse


HOST = os.getenv("SUPPORT_API_HOST", "127.0.0.1")
PORT = int(os.getenv("SUPPORT_API_PORT", "9108"))


TICKETS = {
    "T-1001": {
        "ticket_id": "T-1001",
        "customer": "Ada Lovelace",
        "priority": "high",
        "status": "open",
        "subject": "Refund request after duplicate billing",
        "summary": "Customer reports two charges for the same invoice.",
    },
    "T-2002": {
        "ticket_id": "T-2002",
        "customer": "Grace Hopper",
        "priority": "medium",
        "status": "waiting_on_customer",
        "subject": "Cannot access analytics dashboard",
        "summary": "User needs help resetting SSO access.",
    },
}


class SupportApiHandler(BaseHTTPRequestHandler):
    server_version = "AlephantSupportMock/1.0"

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path.startswith("/v1/tickets/"):
            ticket_id = path.rsplit("/", 1)[-1]
            if ticket_id == "server-error":
                self._json(503, {"error": "support_api_unavailable"})
                return
            if ticket_id == "not-found" or ticket_id not in TICKETS:
                self._json(404, {"error": "ticket_not_found", "ticket_id": ticket_id})
                return
            self._json(200, TICKETS[ticket_id])
            return

        if path == "/v1/slow":
            time.sleep(float(os.getenv("SUPPORT_API_SLOW_SECONDS", "2.5")))
            self._json(200, {"ok": True, "delayed": True})
            return

        self._json(404, {"error": "not_found", "path": path})

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        body = self._read_json()
        if path == "/v1/refund-reviews":
            amount_cents = int(body.get("amount_cents", 0) or 0)
            self._json(
                200,
                {
                    "review_id": "rr_mock_001",
                    "decision": "needs_approval" if amount_cents >= 50000 else "approved",
                    "amount_cents": amount_cents,
                    "reason": "large_refund_review" if amount_cents >= 50000 else "standard_refund",
                },
            )
            return

        self._json(404, {"error": "not_found", "path": path})

    def log_message(self, fmt: str, *args: object) -> None:
        print(f"[support-api] {self.address_string()} {fmt % args}")

    def _read_json(self) -> dict[str, object]:
        length = int(self.headers.get("content-length", "0") or "0")
        if length <= 0:
            return {}
        raw = self.rfile.read(length)
        try:
            data = json.loads(raw.decode("utf-8"))
        except json.JSONDecodeError:
            return {}
        return data if isinstance(data, dict) else {}

    def _json(self, status: int, payload: dict[str, object]) -> None:
        body = json.dumps(payload, sort_keys=True).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main() -> None:
    server = ThreadingHTTPServer((HOST, PORT), SupportApiHandler)
    print(f"Support API mock listening on http://{HOST}:{PORT}")
    print("Try: curl http://127.0.0.1:9108/v1/tickets/T-1001")
    server.serve_forever()


if __name__ == "__main__":
    main()
