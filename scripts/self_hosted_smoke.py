#!/usr/bin/env python3
"""Exercise the production binary with local credentials and a local upstream."""

import argparse
import contextlib
import http.server
import http.client
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
    parser.add_argument("--heartbeat-only", action="store_true", help="verify streaming keepalives through a local reverse proxy")
    parser.add_argument("--validation-only", action="store_true", help="verify header/image/box rules on the production media path")
    parser.add_argument("--image-only", action="store_true", help="verify multipart image edits with the OpenAI SDK")
    parser.add_argument("--audio-only", action="store_true", help="verify raw audio endpoints with the OpenAI SDK")
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
            raw = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            if self.path.endswith(("audio/transcriptions", "images/edits")):
                from email.parser import BytesParser
                from email.policy import default
                message = BytesParser(policy=default).parsebytes(("Content-Type: " + self.headers["Content-Type"] + "\r\n\r\n").encode() + raw)
                request = {part.get_param("name", header="content-disposition"): (part.get_filename(), part.get_payload(decode=True)) for part in message.iter_parts()}
                if self.path.endswith("images/edits"):
                    request["__parts"] = [(part.get_param("name", header="content-disposition"), part.get_filename(), part.get_payload(decode=True)) for part in message.iter_parts()]
            else:
                request = json.loads(raw)
            observed.append((self.path, self.headers.get("Authorization"), request, {k.lower():v for k,v in self.headers.items()}))
            if self.path.endswith("audio/speech") and request.get("input") == "delayed-audio":
                time.sleep(0.35)
            if self.path.endswith("audio/speech") and request.get("input") == "provider-failure":
                data = b'{"error":{"code":"provider_failure","message":"failed"}}'
                self.send_response(503)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            if self.path.endswith("audio/speech"):
                data = b"RIFF\x00\xfflocal-audio\r\n"
                self.send_response(200)
                self.send_header("Content-Type", "audio/wav")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            if self.path.endswith("audio/transcriptions"):
                data = b"hello transcript\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/plain")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            if self.path.endswith("chat/completions") and request.get("messages", [{}])[-1].get("content", "").startswith("delayed-"):
                time.sleep(0.8)
            if self.path.endswith("chat/completions") and request.get("stream"):
                if request["messages"][-1]["content"] == "delayed-stream-fail":
                    self.send_response(503)
                    self.send_header("Content-Type", "application/json")
                    data=b'{"error":{"code":"unavailable","message":"failed"}}'
                    self.send_header("Content-Length",str(len(data)))
                    self.end_headers();self.wfile.write(data);return
                self.send_response(200)
                self.send_header("Content-Type","text/event-stream")
                self.end_headers()
                event={"id":"chatcmpl-local","object":"chat.completion.chunk","created":1,"model":"gpt-4o","choices":[{"index":0,"delta":{"content":"local-ok"},"finish_reason":None}]}
                frame=b"data: "+json.dumps(event).encode()+b"\n\n"
                self.wfile.write(frame[:7]);self.wfile.flush();time.sleep(0.12)
                self.wfile.write(frame[7:]);self.wfile.flush();time.sleep(0.16)
                self.wfile.write(b"data: [DONE]\n\n");self.wfile.flush();return
            if self.path.endswith("images/edits"):
                body = {"created": 1, "data": [{"b64_json": "bG9jYWw="}], "box":{"l":0,"t":0,"r":1,"b":1}, "usage":{"input_tokens":17}}
                if request.get("prompt", (None,b""))[1] == b"bad-response":
                    body["box"]["r"] = -1
            elif self.path.endswith("chat/completions"):
                prompt = request["messages"][-1]["content"]
                if prompt in ("invalid-request","delayed-invalid-request","delayed-created-invalid-request"):
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
            self.send_response(201 if self.path.endswith("chat/completions") and request["messages"][-1]["content"]=="delayed-created-invalid-request" else 200)
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
        if args.heartbeat_only:
            profile=profile.replace("  business-error-checks:", "  downstream-heartbeat: {interval-ms: 50, early-sse: true, json-whitespace: true, max-frame-bytes: 65536}\n  business-error-checks:")
        if args.validation_only:
            image_rule={"kind":"image","select":{"pointer":"/multipart/image","each":"array_item"},"data_pointer":"/data_base64","encoding":"base64","formats":["png","jpeg"],"max_decoded_bytes":65536,"width":{"min":1,"max":4},"height":{"min":1,"max":4}}
            box_rule={"kind":"bounding_box","select":{"pointer":"/box"},"fields":{"left":"/l","top":"/t","right":"/r","bottom":"/b"},"min":0,"max":1}
            rules=[{"provider":"openai","path":"v1/audio/speech","validation":{"version":2,"request_checks":[{"kind":"header","name":"Experiment-Mode","required":True,"allowed_values":["true"]}]}},
                   {"provider":"openai","path":"v1/images/edits","validation":{"version":1,"request_checks":[image_rule],"response_checks":[box_rule]},"request-contract":{"version":1,"inputs":[],"mappings":[],"fixed_headers":{"X-Product":"alephant"}}}]
            profile=profile.replace("  business-error-checks:", "  validation-rules: " + json.dumps(rules) + "\n  business-error-checks:")
        if args.image_only:
            profile = profile.replace("allowed-models: [openai/gpt-4o]", "allowed-models: [openai/gpt-4o, openai/gpt-image-1]\n      blocked-models: [openai/denied]")
        if args.audio_only:
            profile = profile.replace("allowed-models: [openai/gpt-4o]", "allowed-models: [openai/gpt-4o, openai/tts-1, openai/whisper-1]\n      blocked-models: [openai/denied]")
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
                data = None if payload is None else payload if isinstance(payload, bytes) else json.dumps(payload).encode()
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
                if args.heartbeat_only:
                    class Proxy(http.server.BaseHTTPRequestHandler):
                        def do_POST(self):
                            connection=http.client.HTTPConnection("127.0.0.1",gateway_port,timeout=5)
                            data=self.rfile.read(int(self.headers["Content-Length"]))
                            connection.request("POST",self.path,data,dict(self.headers))
                            response=connection.getresponse()
                            self.send_response(response.status)
                            for k,v in response.getheaders():
                                if k.lower() not in ("transfer-encoding","connection","content-length"):
                                    self.send_header(k,v)
                            self.end_headers()
                            try:
                                while True:
                                    chunk=response.read(1)
                                    if not chunk: break
                                    self.wfile.write(chunk);self.wfile.flush()
                            except (BrokenPipeError,ConnectionResetError): pass
                            finally: connection.close()
                        def log_message(self,*_args): pass
                    proxy=http.server.ThreadingHTTPServer(("127.0.0.1",0),Proxy)
                    threading.Thread(target=proxy.serve_forever,daemon=True).start()
                    cleanup.callback(proxy.server_close);cleanup.callback(proxy.shutdown)
                    def delayed(prompt,stream=False,key=gateway_key):
                        connection=http.client.HTTPConnection("127.0.0.1",proxy.server_port,timeout=5)
                        data=json.dumps({"model":"openai/gpt-4o","messages":[{"role":"user","content":prompt}],"stream":stream})
                        start=time.monotonic()
                        connection.request("POST","/v1/chat/completions",data,{"Content-Type":"application/json","Authorization":"Bearer "+key})
                        response=connection.getresponse();first=response.read(1);elapsed=time.monotonic()-start
                        rest=response.read();connection.close()
                        return response.status,first+rest,first,elapsed
                    assert call("/v1/chat/completions",payload)[0]==200
                    status,body,first,elapsed=delayed("delayed-json")
                    assert first==b"\n" and elapsed<0.7,(first,elapsed)
                    assert json.loads(body)["choices"][0]["message"]["content"]=="local-ok"
                    status,body,first,elapsed=delayed("delayed-invalid-request")
                    assert status==200 and json.loads(body)["error"]["code"]=="invalid_request",(status,body)
                    assert json.loads(body)["upstream_status"]==200
                    assert json.loads(body)["gateway_status"]==400
                    status,body,first,elapsed=delayed("delayed-created-invalid-request")
                    assert status==200 and json.loads(body)["upstream_status"]==201,(status,body)
                    assert json.loads(body)["gateway_status"]==400
                    status,body,first,elapsed=delayed("delayed-stream",True)
                    assert first==b":" and elapsed<0.7,(first,elapsed)
                    assert body.count(b"data: [DONE]")==1,body
                    assert b"local-ok" in body and body.count(b": alephant heartbeat")>=2,body
                    for frame in body.split(b"\n\n"):
                        for line in frame.splitlines():
                            if line.startswith(b"data: ") and line!=b"data: [DONE]": json.loads(line[6:])
                    status,body,first,elapsed=delayed("delayed-stream-fail",True)
                    assert status==200 and b'"error"' in body,(status,body)
                    assert delayed("delayed-json",key="invalid")[0]==401
                    import openai
                    import httpx
                    with openai.OpenAI(base_url=f"http://127.0.0.1:{proxy.server_port}/v1",api_key=gateway_key,max_retries=0,http_client=httpx.Client(trust_env=False)) as client:
                        chunks=list(client.chat.completions.create(model="openai/gpt-4o",messages=[{"role":"user","content":"delayed-stream"}],stream=True))
                        assert "".join(c.choices[0].delta.content or "" for c in chunks)=="local-ok"
                        before=len(observed)
                        try:
                            list(client.chat.completions.create(model="openai/gpt-4o",messages=[{"role":"user","content":"delayed-stream-fail"}],stream=True))
                            raise AssertionError("post-commit stream error was presented as success")
                        except openai.APIError as error:
                            assert error.code=="upstream_request_failed",error
                        assert len(observed)==before+1,"failed streams must not be replayed"
                    before=time.monotonic()
                    status,body=call("/v1/audio/speech",{"model":"openai/tts-1","voice":"alloy","input":"delayed-audio"})
                    assert status==200 and body==b"RIFF\x00\xfflocal-audio\r\n"
                    assert time.monotonic()-before>=0.3
                    print("PASS: reverse-proxied early JSON/SSE and idle heartbeats, split frames, one DONE, post-commit errors, SDK parse, auth and binary isolation")
                    return
                if args.validation_only:
                    import struct
                    import zlib
                    speech={"model":"openai/tts-1","voice":"alloy","input":"hello"}
                    before=len(observed)
                    assert call("/v1/audio/speech",speech)[0]==400, "required header must be checked before upstream dispatch"
                    assert len(observed)==before
                    assert call("/v1/audio/speech",speech,extra_headers={"Experiment-Mode":"TRUE"})[0]==400
                    assert call("/v1/audio/speech",speech,extra_headers={"Experiment-Mode":"true"})[1]==b"RIFF\x00\xfflocal-audio\r\n"
                    def png(width):
                        def chunk(kind,data):
                            return struct.pack(">I",len(data))+kind+data+struct.pack(">I",zlib.crc32(kind+data)&0xffffffff)
                        return b"\x89PNG\r\n\x1a\n"+chunk(b"IHDR",struct.pack(">IIBBBBB",width,2,8,2,0,0,0))+chunk(b"IDAT",zlib.compress((b"\x00"+b"\x00"*(width*3))*2))+chunk(b"IEND",b"")
                    def image_form(image,prompt=b"edit"):
                        return b'--b\r\nContent-Disposition: form-data; name="model"\r\n\r\nopenai/gpt-image-1\r\n--b\r\nContent-Disposition: form-data; name="prompt"\r\n\r\n'+prompt+b'\r\n--b\r\nContent-Disposition: form-data; name="image"; filename="a.png"\r\nContent-Type: image/png\r\n\r\n'+image+b'\r\n--b--\r\n'
                    for image in [b"fake",png(5),png(2)[:-8]]:
                        before=len(observed)
                        status,body=call("/v1/images/edits",image_form(image),extra_headers={"Content-Type":"multipart/form-data; boundary=b"})
                        assert status in (400,413,415),(status,body)
                        assert len(observed)==before
                    status,body=call("/v1/images/edits",image_form(png(2)),extra_headers={"Content-Type":"multipart/form-data; boundary=b"})
                    assert status==200,(status,body)
                    assert observed[-1][3]["x-product"]=="alephant"
                    status,body=call("/v1/images/edits",image_form(png(2),b"bad-response"),extra_headers={"Content-Type":"Multipart/Form-Data; boundary=b","Alephant-Cache-Enabled":"true"})
                    assert status==502,(status,body)
                    assert json.loads(body)["error"]["code"]=="box_out_of_range"
                    assert call("/v1/audio/speech",{"model":"openai/tts-1","voice":"alloy","input":"private-log-input-sentinel"},extra_headers={"Experiment-Mode":"true","X-Business-Header":"private-log-header-sentinel","Alephant-Debug-Headers":"true","Alephant-Debug-Body":"true"})[0]==200
                    time.sleep(0.1)
                    assert "private-log-input-sentinel" not in logs.read_text(),"validation payload leaked into diagnostic logs"
                    assert "private-log-header-sentinel" not in logs.read_text(),"business header value leaked into diagnostic logs"
                    print("PASS: production required/strict headers, fixed safe header, actual PNG decode and bounds, successful response validation")
                    return
                if args.image_only:
                    import openai
                    import httpx
                    with openai.OpenAI(base_url=f"http://127.0.0.1:{gateway_port}/v1", api_key=gateway_key,
                                       max_retries=0, http_client=httpx.Client(trust_env=False)) as client:
                        for images in [[("a.png", b"PNG\x00\xffa", "image/png")],
                                       [("a.png", b"PNG\x00\xffa", "image/png"), ("照片.png", b"PNG\x01\xffb", "image/png")]]:
                            response = client.images.edit(model="openai/gpt-image-1", image=images, mask=("mask.png", b"MASK\x00\xff", "image/png"), prompt="edit locally", response_format="b64_json")
                            assert response.data[0].b64_json == "bG9jYWw="
                            parts=observed[-1][2]["__parts"]
                            assert [(f,b) for n,f,b in parts if n in ("image", "image[]")] == [(f,b) for f,b,_ in images], parts
                            assert observed[-1][2]["mask"] == ("mask.png", b"MASK\x00\xff")
                            assert observed[-1][2]["model"][1] == b"gpt-image-1"
                            assert observed[-1][1] == "Bearer " + upstream_key
                    def image_form(fields):
                        return b"".join(b'--b\r\nContent-Disposition: form-data; name="' + name.encode() + b'"' +
                            (b'; filename="image.png"' if name in ("image", "image[]", "mask") else b'') + b"\r\n\r\n" + value + b"\r\n" for name, value in fields) + b"--b--\r\n"
                    valid=[("model",b"openai/gpt-image-1"),("prompt",b"edit"),("image",b"PNG\x00\xff")]
                    for fields in [valid[:2], valid[:1]+[("prompt",b" ")]+valid[2:], valid+[("model",b"other")],
                                   valid+[("mask",b"a"),("mask",b"b")], valid+[("stream",b"true")]]:
                        before=len(observed)
                        status, body=call("/v1/images/edits",image_form(fields),extra_headers={"Content-Type":"multipart/form-data; boundary=b"})
                        assert status==400, (status,body)
                        assert len(observed)==before
                    assert call("/v1/images/edits",image_form([(n,b"openai/denied" if n=="model" else v) for n,v in valid]),extra_headers={"Content-Type":"multipart/form-data; boundary=b"})[0]==403
                    print("PASS: OpenAI SDK single/multiple image edits, mask and binary preservation")
                    return
                if args.audio_only:
                    import openai
                    import httpx
                    with openai.OpenAI(base_url=f"http://127.0.0.1:{gateway_port}/v1", api_key=gateway_key,
                                       max_retries=0, http_client=httpx.Client(trust_env=False)) as client:
                        response = client.audio.speech.create(model="openai/tts-1", voice="alloy", input="hello", response_format="wav")
                        assert response.read() == b"RIFF\x00\xfflocal-audio\r\n"
                        assert response.response.headers["content-type"] == "audio/wav"
                        assert observed[-1][2]["model"] == "tts-1"
                        transcript = client.audio.transcriptions.create(model="openai/whisper-1", file=("录音.wav", b"RIFF\x00\xff\r\ndata", "audio/wav"), response_format="text", prompt="hello")
                        assert transcript == "hello transcript\n", transcript
                        request = observed[-1][2]
                        assert request["file"] == ("录音.wav", b"RIFF\x00\xff\r\ndata"), request
                        assert request["model"][1] == b"whisper-1"
                        assert request["response_format"][1] == b"text"
                        assert observed[-1][1] == "Bearer " + upstream_key
                        before = len(observed)
                        try:
                            client.audio.speech.create(model="openai/denied", voice="alloy", input="hello")
                            raise AssertionError("denied audio model accepted")
                        except openai.PermissionDeniedError:
                            pass
                        assert len(observed) == before
                    def multipart(fields):
                        return b"".join(b'--b\r\nContent-Disposition: form-data; name="' + name.encode() + b'"' +
                            (b'; filename="sample.wav"' if name == "file" else b'') + b"\r\n\r\n" + value + b"\r\n" for name, value in fields) + b"--b--\r\n"
                    valid = [("model", b"openai/whisper-1"), ("file", b"RIFF\x00\xff")]
                    for fields in [valid + [("model", b"other")], valid[1:], valid[:1],
                                   valid + [("response_format", b"text"), ("response_format", b"json")]]:
                        before = len(observed)
                        status, body = call("/v1/audio/transcriptions", multipart(fields), extra_headers={"Content-Type":"multipart/form-data; boundary=b"})
                        assert status == 400, (status, body)
                        assert len(observed) == before
                    assert call("/v1/audio/transcriptions", b"broken", extra_headers={"Content-Type":"multipart/form-data; boundary=b"})[0] == 400
                    assert call("/v1/audio/transcriptions", b"x" * (1024*1024+1), extra_headers={"Content-Type":"multipart/form-data; boundary=b"})[0] == 413
                    assert call("/v1/audio/speech", {"model":"tts-1"}, extra_headers={"Content-Type":"text/plain"})[0] == 415
                    before=len(observed)
                    assert call("/v1/audio/speech", {"model":"openai/tts-1", "input":"provider-failure", "voice":"alloy"})[0] == 503
                    assert len(observed)==before+1, "media failures must not be replayed"
                    print("PASS: OpenAI SDK speech binary, transcription multipart, model rewrite, credential isolation and model policy")
                    return
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
