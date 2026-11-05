#!/usr/bin/env python3
"""Exercise the production binary with local credentials and a local upstream."""

import argparse
import contextlib
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


def unused_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def stop_process(process):
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/ai-gateway"))
    parser.add_argument("--rate-limit-only", action="store_true", help="verify shared workspace quotas")
    parser.add_argument("--redis", action="store_true", help="test two gateway processes with temporary Redis")
    args = parser.parse_args()
    if args.redis and not args.rate_limit_only:
        parser.error("--redis requires --rate-limit-only")
    binary = args.binary.resolve()
    root = Path(__file__).resolve().parents[1]
    gateway_key = secrets.token_urlsafe(32)
    upstream_key = secrets.token_urlsafe(32)
    second_key = secrets.token_urlsafe(32)
    other_workspace_key = secrets.token_urlsafe(32)
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

    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream) as upstream, contextlib.ExitStack() as server_cleanup:
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        server_cleanup.callback(upstream.shutdown)
        gateway_port = unused_port()
        origin = "http://127.0.0.1:" + str(upstream.server_port)
        profile = (root / "examples/self-hosted/config.yaml").read_text()
        profile = profile.replace("port: 18081", "port: " + str(gateway_port))
        profile = profile.replace("https://api.openai.com/v1", origin + "/v1")
        profile = profile.replace("https://example.invalid/search", origin + "/search")
        profile = profile.replace("https-only: true", "https-only: false")
        profile = profile.replace("block-loopback: true", "block-loopback: false")
        profile = profile.replace("level: warn", "level: info,ai_gateway::dispatcher=debug")
        if args.rate_limit_only:
            profile += "\nglobal:\n  identity-rate-limit:\n    enabled: true\n    requests: 3\n    window-seconds: 60\n    backend: memory\n    scopes: [models, tools]\n"
            if args.redis:
                profile = profile.replace("backend: memory", "backend: redis")
            additional_keys = """    - id: 00000000-0000-0000-0000-000000000003
      workspace-id: 00000000-0000-0000-0000-000000000002
      key-env: ALEPHANT_SECOND_LOCAL_API_KEY
      provider: openai
      allowed-models: [openai/gpt-4o]
    - id: 00000000-0000-0000-0000-000000000004
      workspace-id: 00000000-0000-0000-0000-000000000005
      key-env: ALEPHANT_OTHER_LOCAL_API_KEY
      provider: openai
      allowed-models: [openai/gpt-4o]
"""
            profile = profile.replace("providers:\n", additional_keys + "providers:\n", 1)
        env = {k: v for k, v in os.environ.items() if not k.startswith("AI_GATEWAY__")
               and k not in ("POSTGRES_DATABASE_URL", "REDIS_URL", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY")}
        env.update(ALEPHANT_LOCAL_API_KEY=gateway_key, OPENAI_API_KEY=upstream_key,
                   RUST_LOG="info,ai_gateway::dispatcher=debug",
                   NO_PROXY="127.0.0.1,localhost,::1", no_proxy="127.0.0.1,localhost,::1",
                   ALEPHANT_SECOND_LOCAL_API_KEY=second_key, ALEPHANT_OTHER_LOCAL_API_KEY=other_workspace_key)
        with tempfile.TemporaryDirectory(prefix="alephant-self-hosted-") as directory, contextlib.ExitStack() as cleanup:
            config = Path(directory) / "config.yaml"
            config.write_text(profile)
            logs = Path(directory) / "gateway.log"
            secondary_logs = Path(directory) / "gateway-2.log"
            secondary_port = None
            redis_process = None
            if args.redis:
                redis_port = unused_port()
                redis_process = subprocess.Popen([
                    "redis-server", "--bind", "127.0.0.1", "--port", str(redis_port),
                    "--save", "", "--appendonly", "no",
                ], stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
                cleanup.callback(stop_process, redis_process)
                deadline = time.monotonic() + 10
                while True:
                    if redis_process.poll() is not None:
                        raise AssertionError("temporary Redis exited during startup")
                    try:
                        with socket.create_connection(("127.0.0.1", redis_port), timeout=0.1):
                            break
                    except OSError:
                        if time.monotonic() >= deadline:
                            raise AssertionError("temporary Redis did not start")
                        time.sleep(0.01)
                env["REDIS_URL"] = "redis://127.0.0.1:" + str(redis_port)
            with logs.open("w") as output:
                process = subprocess.Popen([str(binary), "-c", str(config)], cwd=directory,
                                           env=env, stdout=output, stderr=subprocess.STDOUT)
            cleanup.callback(stop_process, process)
            secondary_process = None
            if args.redis:
                secondary_port = unused_port()
                secondary_config = Path(directory) / "config-2.yaml"
                secondary_config.write_text(profile.replace("port: " + str(gateway_port), "port: " + str(secondary_port)))
                with secondary_logs.open("w") as output:
                    secondary_process = subprocess.Popen([str(binary), "-c", str(secondary_config)], cwd=directory,
                                                         env=env, stdout=output, stderr=subprocess.STDOUT)
                cleanup.callback(stop_process, secondary_process)
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            last_headers = {}

            def call(path, payload=None, key=gateway_key, extra_headers=None, port=None):
                data = None if payload is None else json.dumps(payload).encode()
                headers = {"Content-Type": "application/json"}
                if key is not None:
                    headers["Authorization"] = "Bearer " + key
                headers.update(extra_headers or {})
                request = urllib.request.Request(
                    "http://127.0.0.1:" + str(port or gateway_port) + path, data=data, headers=headers)
                try:
                    with opener.open(request, timeout=5) as response:
                        last_headers.clear()
                        last_headers.update((k.lower(), v) for k, v in response.headers.items())
                        return response.status, response.read()
                except urllib.error.HTTPError as error:
                    last_headers.clear()
                    last_headers.update((k.lower(), v) for k, v in error.headers.items())
                    return error.code, error.read()

            def wait_for_gateway(child, port):
                deadline = time.monotonic() + 20
                while True:
                    if child.poll() is not None:
                        raise AssertionError("production gateway exited during startup")
                    try:
                        if call("/health", port=port)[0] == 200:
                            break
                    except (OSError, urllib.error.URLError):
                        pass
                    if time.monotonic() >= deadline:
                        raise AssertionError("production gateway did not start")
                    time.sleep(0.1)

            try:
                wait_for_gateway(process, gateway_port)
                if secondary_process is not None:
                    wait_for_gateway(secondary_process, secondary_port)
                payload = {"model": "openai/gpt-4o", "messages": [{"role": "user", "content": "hello"}]}
                if args.rate_limit_only:
                    assert call("/v1/agent/tools/list", {})[0] == 200
                    assert call("/v1/agent/tools/list", {}, key=second_key, port=secondary_port)[0] == 200
                    assert call("/v1/chat/completions", payload)[0] == 200
                    before = len(observed)
                    status, body = call("/v1/chat/completions", payload, key=second_key,
                                        extra_headers={"X-Workspace-Id": "00000000-0000-0000-0000-000000000005"}, port=secondary_port)
                    assert status == 429, ("shared workspace quota", status, body)
                    assert int(last_headers["retry-after"]) >= 1
                    assert last_headers.get("cache-control") == "no-store"
                    assert len(observed) == before, "rate limited requests must not reach a provider"
                    assert call("/v1/agent/tools/list", {})[0] == 429
                    assert call("/v1/chat/completions", payload, key=other_workspace_key, port=secondary_port)[0] == 200
                    if redis_process is not None:
                        stop_process(redis_process)
                        assert call("/v1/agent/tools/list", {}, key=other_workspace_key, port=secondary_port)[0] == 503, "Redis failure must deny admission"
                    backend = "two processes sharing Redis" if args.redis else "memory"
                    print("PASS: workspace quota shared across keys, models and tools (" + backend + "); forged scope ignored; other workspace isolated")
                    return
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
                if secondary_logs.exists():
                    print(secondary_logs.read_text()[-5000:])
                raise


if __name__ == "__main__":
    main()
