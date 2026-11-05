#!/usr/bin/env python3
"""Exercise the production binary with local credentials and a local upstream."""

import argparse
import http.server
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ai-gateway"))
    args = parser.parse_args()
    binary = args.binary.resolve()
    root = Path(__file__).resolve().parents[1]
    gateway_key = secrets.token_urlsafe(32)
    upstream_key = secrets.token_urlsafe(32)
    observed = []

    class Upstream(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            observed.append((self.path, self.headers.get("Authorization"), request))
            if self.path.endswith("chat/completions"):
                prompt = request["messages"][-1]["content"]
                if prompt == "invalid-request":
                    body = {"success": False, "error": {"code": "invalid_request", "message": "private user input"}}
                elif prompt == "rate-limit":
                    body = {"success": False, "error": {"code": "rate_limit_exceeded", "message": "private provider credential"}}
                else:
                    body = {
                    "id": "chatcmpl-local", "object": "chat.completion", "created": 1,
                    "model": "gpt-4o", "choices": [{"index": 0,
                        "message": {"role": "assistant", "content": "local-ok"},
                        "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                    }
            else:
                body = ({"success": False, "error": {"code": "invalid_request", "message": "private-value"}}
                        if request.get("query") == "invalid-request" else {"result": "document-found"})
            data = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *_args):
            pass

    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream) as upstream:
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            gateway_port = sock.getsockname()[1]
        origin = "http://127.0.0.1:" + str(upstream.server_port)
        profile = (root / "examples/self-hosted/config.yaml").read_text()
        profile = profile.replace("port: 18081", "port: " + str(gateway_port))
        profile = profile.replace("https://api.openai.com/v1", origin + "/v1")
        profile = profile.replace("https://example.invalid/search", origin + "/search")
        profile = profile.replace("https-only: true", "https-only: false")
        profile = profile.replace("block-loopback: true", "block-loopback: false")
        profile = profile.replace("level: warn", "level: info,ai_gateway::dispatcher=debug")
        env = {k: v for k, v in os.environ.items() if not k.startswith("AI_GATEWAY__")
               and k not in ("POSTGRES_DATABASE_URL", "REDIS_URL", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY")}
        env.update(ALEPHANT_LOCAL_API_KEY=gateway_key, OPENAI_API_KEY=upstream_key,
                   RUST_LOG="info,ai_gateway::dispatcher=debug",
                   NO_PROXY="127.0.0.1,localhost,::1", no_proxy="127.0.0.1,localhost,::1")
        with tempfile.TemporaryDirectory(prefix="alephant-self-hosted-") as directory:
            config = Path(directory) / "config.yaml"
            config.write_text(profile)
            logs = Path(directory) / "gateway.log"
            with logs.open("w") as output:
                process = subprocess.Popen([str(binary), "-c", str(config)], cwd=directory,
                                           env=env, stdout=output, stderr=subprocess.STDOUT)
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def call(path, payload=None, key=gateway_key):
                data = None if payload is None else json.dumps(payload).encode()
                headers = {"Content-Type": "application/json"}
                if key is not None:
                    headers["Authorization"] = "Bearer " + key
                request = urllib.request.Request(
                    "http://127.0.0.1:" + str(gateway_port) + path, data=data, headers=headers)
                try:
                    with opener.open(request, timeout=5) as response:
                        return response.status, response.read()
                except urllib.error.HTTPError as error:
                    return error.code, error.read()

            try:
                deadline = time.monotonic() + 20
                while True:
                    if process.poll() is not None:
                        raise AssertionError("production gateway exited during startup")
                    try:
                        if call("/health")[0] == 200:
                            break
                    except (OSError, urllib.error.URLError):
                        pass
                    if time.monotonic() >= deadline:
                        raise AssertionError("production gateway did not start")
                    time.sleep(0.1)
                payload = {"model": "openai/gpt-4o", "messages": [{"role": "user", "content": "hello"}]}
                status, body = call("/v1/chat/completions", payload, key=None)
                assert status == 401, ("missing credentials", status, body)
                status, body = call("/v1/chat/completions", payload, key="invalid-key")
                assert status == 401, ("invalid credentials", status, body)
                status, body = call("/v1/chat/completions", payload)
                assert status == 200, ("authenticated chat", status, body)
                assert json.loads(body)["choices"][0]["message"]["content"] == "local-ok"
                model_requests = [item for item in observed if item[0].endswith("chat/completions")]
                assert len(model_requests) == 1, "rejected requests must not reach the upstream"
                assert model_requests[0][1] == "Bearer " + upstream_key, "gateway key must not reach provider"
                assert model_requests[0][2]["model"] == "gpt-4o", "provider prefix must be removed"
                for prompt, expected in (("invalid-request", 400), ("rate-limit", 429)):
                    bad = {"model": "openai/gpt-4o", "messages": [{"role": "user", "content": prompt}]}
                    before = len(observed)
                    status, body = call("/v1/chat/completions", bad)
                    assert status == expected, ("business failure classification", prompt, status, body)
                    assert len(observed) == before + 1, "client errors and rate limits must not be replayed"
                    assert b"private" not in body, "provider error details must not echo sensitive values"
                status, body = call("/v1/agent/tools/list", {})
                assert status == 200, ("static tool catalog", status, body)
                assert any(tool["toolId"] == "docs.search" for tool in json.loads(body)["tools"])
                status, body = call("/v1/agent/tools/call", {"tool_id": "docs.search", "arguments": {"query": "hello"}})
                assert status == 200, ("static tool call", status, body)
                result = json.loads(body)
                assert result["executed"] and result["output"]["result"] == "document-found", result
                before = len(observed)
                status, body = call("/v1/agent/tools/call", {"tool_id": "docs.search", "arguments": {"query": "invalid-request"}})
                result = json.loads(body)
                assert result["status"] == "failed" and result["error"]["code"] == "invalid_request", result
                assert len(observed) == before + 1, "a failed tool call must not be replayed"
                assert b"private-value" not in body, "tool errors must not echo provider values"
                assert call("/healthz/ready")[0] == 200, "local configuration must open readiness without a database listener"
                print("PASS: production startup, readiness, authentication, model/tool calls and business failures; no database or commercial services")
            except Exception:
                print("Observed local upstream paths:", [item[0] for item in observed])
                print(logs.read_text()[-10000:])
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                upstream.shutdown()


if __name__ == "__main__":
    main()
