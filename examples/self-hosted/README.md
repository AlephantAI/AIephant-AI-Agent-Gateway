# Authenticated self-hosting

Run the production gateway with local identities, provider environment credentials and a static tool catalog. This profile does not need PostgreSQL, OpenAny, a policy service, a billing service or a log collector. The `internal` build uses a disabled KV cache when TiKV is not configured.

```sh
export ALEPHANT_LOCAL_API_KEY="$(openssl rand -hex 32)"
export OPENAI_API_KEY="your-provider-key"
cargo run -p ai-gateway --features internal -- -c examples/self-hosted/config.yaml
```

Use `ALEPHANT_LOCAL_API_KEY` as the Bearer credential when calling the gateway. The provider receives `OPENAI_API_KEY`, never the gateway credential. Keep the generated gateway key private.

Each entry in `local-auth.keys` has a unique `id`, a stable `workspace-id`, a `provider` binding and a `key-env` environment variable name. Credentials must be unique and contain at least 32 non-whitespace ASCII bytes. Missing credentials fail startup. `allowed-models` and `blocked-models` use the existing virtual-key policy. Requests cannot choose their own workspace using a header.

Local authentication requires `compat-mode: true` and `alephant.features: auth`. Existing database authentication remains available when local authentication is absent. Local provider credentials are shared process environment values; separate workspace-specific upstream keys continue to use the database-backed deployment.

The example `docs.search` URL is a placeholder: replace it with your own HTTPS API. Tools use the existing static catalog and execution pipeline. High-risk tools retain the existing requirement for an available audit sink; the example is low risk. Runtime YAML changes and credential rotation require restarting the process.

Verify the real binary with local mock upstreams and generated temporary credentials:

```sh
cargo build -p ai-gateway --features internal
python3 scripts/self_hosted_smoke.py
```

The smoke check covers startup, readiness, rejected credentials, authenticated model calls, upstream credential isolation, tool discovery and tool execution. It does not call a paid provider or use a test-only authentication hook.

## Explicit business failure checks

`dispatcher.business-error-checks` enables checks for an exact provider and endpoint, for example `provider: openai` and `path: v1/chat/completions`. Checks are disabled when no matching entry exists. Query strings do not change the endpoint match.

For successful JSON responses up to 64 KiB, the gateway recognizes explicit failure signals such as a nonempty `error`, `success: false`, or a failed `status`. It maps parameter failures to 400, rate limits to 429, unavailable models to 404, and provider failures to 502. Safe provider error codes are retained, while messages use bounded generic text rather than echoing provider input. `x-alephant-upstream-status` records the original HTTP status. Modified responses discard stale entity headers.

Larger responses, non-JSON responses and SSE retain their existing handling. Do not enable the rule for an endpoint where these failure fields are ordinary business data. A checked failure cannot be saved through the successful-response cache path.

OpenAPI tool targets can independently enable `detect-business-errors: true`. The tool envelope then reports a failure while metadata retains the original upstream HTTP status. Business classification preserves the existing cost/billing decision for the actual upstream 2xx invocation. Failure-charge settings continue to apply to actual HTTP failures; classification itself does not manufacture a refund or introduce a balance service. With the option absent, tool response behavior and snapshot serialization remain compatible.

## Workspace request quota

Add under `global` (disabled by default):

```yaml
identity-rate-limit:
  enabled: true
  requests: 100
  window-seconds: 60
  backend: memory
  scopes: [models, tools]
```

All admitted model and tool requests share one fixed-window budget per authenticated workspace, including different keys in that workspace. Rejected requests return 429 with `Retry-After` and do not reach the upstream. Failed admitted requests consume quota; internal retries do not consume it again. Fixed windows permit bursts around a window boundary. Health and agent event ingestion are excluded.

`memory` is for a single process and resets on restart. For multiple gateways use `backend: redis`, the same `REDIS_URL`, stable workspace IDs and the same `redis-key-prefix` (default `gw:identityrl:`). Gateway restarts retain the Redis window. Redis failures return 503; there is no fallback to independent local quotas. Missing Redis TTLs are repaired without clearing existing counts.

```sh
python3 scripts/self_hosted_smoke.py --rate-limit-only
# Requires redis-server; starts and cleans up its own temporary Redis and two gateways.
python3 scripts/self_hosted_smoke.py --rate-limit-only --redis
```

## MCP versions

Streamable HTTP tool targets accept negotiated `2025-03-26`, `2025-06-18` and `2025-11-25` versions. The client keeps its existing `2025-06-18` initial offer for deployment compatibility; a server may select another supported version. The selected version is used for initialized notifications, calls and cached sessions. Unknown versions, expired sessions and invalid session IDs are rejected before cache reuse. Cache identities retain workspace, key, target revision and authentication revision isolation.

Coverage concerns the tools client lifecycle and JSON/SSE result transport, including RPC errors, `isError`, interrupted streams and timeouts. It does not advertise sampling, elicitation, tasks or subscriptions. An uncertain tool result is not replayed; the existing explicit cached-session 404 reinitialization remains in place.

Protocol references: [March lifecycle](https://modelcontextprotocol.io/specification/2025-03-26/basic/lifecycle), [November lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle), [Streamable HTTP](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).

## Audio endpoints

`POST /v1/audio/speech` accepts JSON (`model`, `input`, `voice`) and forwards raw audio bytes and the upstream content type. `POST /v1/audio/transcriptions` accepts multipart (`model`, one nonempty `file`), preserving UTF-8/escaped filenames, file bytes and optional fields such as `prompt`, `language` and `response_format`. JSON, text, SRT and VTT upstream responses are forwarded without chat conversion. This version supports raw audio speech and non-streaming transcription; speech SSE and transcription event streaming are rejected.

Supported wire protocols: OpenAI, named OpenAI-compatible providers and a configured custom OpenAI-compatible base URL. This describes protocol support, not a guarantee that every provider offers every media model. The key's bound provider selects the upstream; its matching prefix is removed from `model`. The existing virtual-key policy applies to the original model: `allowed-models` overrides matching blocks, `blocked-models` denies exact matches, and unmatched models remain allowed (it is not a restrictive allowlist).

The upload cap is **1 MiB including multipart framing**, matching the existing model-body bound; multipart permits up to 128 parts and 8 KiB of headers per part. Repeated disposition/boundary parameters, duplicate singleton fields, malformed forms and missing model/file fields are rejected before sending. Files and all non-model byte ranges remain intact. Media calls skip LLM/semantic caches and automatic retries/fallback, retaining authentication, workspace quotas, upstream credential sanitization and normal request logs. No duration-to-token estimate or new billing policy is applied; absent upstream usage remains absent/zero in existing token fields.

```sh
# Uses the installed openai Python SDK, generated local keys and a local mock; no paid API calls.
python3 scripts/self_hosted_smoke.py --audio-only
```

API references: [speech](https://developers.openai.com/api/reference/resources/audio/subresources/speech/methods/create), [transcriptions](https://developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create).

## Image edits

`POST /v1/images/edits` uses the same authenticated raw media pipeline. Send multipart with a unique `model`, nonempty `prompt`, 1–16 nonempty `image`/`image[]` file parts, and an optional single `mask` file. Other upstream parameters pass through; the model's matching provider prefix is the only changed byte range. The same 1 MiB total upload, 128-part and 8 KiB part-header limits apply. Duplicate singleton fields are rejected. Streaming edits are rejected; non-streaming upstream JSON (including base64/URL and real usage fields) passes through unchanged.

OpenAI and configured OpenAI-compatible protocols are supported; the upstream still decides accepted model, image format, dimensions and parameter combinations unless explicit validation rules are configured. Original `images/generations` handling remains separate. There are no media retries or cache writes.

```sh
python3 scripts/self_hosted_smoke.py --image-only
```

[OpenAI image edit reference](https://developers.openai.com/api/reference/resources/images/methods/edit).

## Optional payload validation

`dispatcher.validation-rules` selects an exact provider and mapped `v1/...` path (without query). Each entry accepts `validation` and/or `request-contract` JSON/YAML objects. Static OpenAPI tool targets accept the same two fields; runtime snapshots preserve them. Invalid rules fail startup. No rule means no decoder, extra body buffering or media admission.

```yaml
dispatcher:
  validation-rules:
    - provider: openai
      path: v1/audio/speech
      validation:
        version: 2
        request_checks:
          - kind: header
            name: Experiment-Mode
            required: true
            allowed_values: ["true", "false"]
            min_length: 4
            max_length: 5
```

Headers reject duplicates and compare exact UTF-8 wire values. To compile strict boolean inputs and fixed business headers, use `request-contract: {version: 1, inputs: [...], mappings: [...], fixed_headers: {...}}`. A header input declares `location: header`, `name`, `type: string|boolean`, boolean `required`, `nullable: false`, optional `enum_values`, `min_length`/`max_length`, and an identity `header:Name` mapping. Boolean wire values are exactly `true`/`false`. Fixed/dynamic conflicts fail configuration. Credential, host, hop, cookie, forwarded and Alephant control headers cannot be declared or replaced. Existing credential sanitization still runs before authentication is applied. Only the header portion of this contract is executed; tool body schemas remain owned by the existing tool schema validator.

Media rules use JSON pointers and `select.each: single|object_value|array_item`, with optional discriminator `where`, `value_pointer`, `required` and `min_items`. `kind: image` validates selected base64 fields with `data_pointer`, `encoding: base64`, `formats: [png, jpeg]`, `max_decoded_bytes`, and `width`/`height: {min, max}`. Optional format declarations, forbidden fields and reference constraints are supported. PNG/JPEG are fully decoded; fake headers, corrupt data, oversize dimensions and disallowed animation fail. Multipart uploads are projected for validation only as `/multipart/<field-name>/<index>/{filename,data_base64}`; the outgoing upload stays intact. For example select `/multipart/image` with `each: array_item`, then use `/data_base64`.

`kind: bounding_box` declares coordinate field pointers, finite `min`/`max`, corners or `origin_size`, and optional confidence bounds. Response rules can declare a `request_selector` with matching collection kind to require the same request/response keys or indices. Checks never rewrite payloads. Response checks require non-streaming results; only successful upstream responses are validated. Request errors return 400/413/415; unavailable admission returns 503; an invalid successful model/media response returns 502. Tools report a failed envelope with the original upstream status and safe error code. Completed upstream cost/billing and available usage facts are preserved; validation does not manufacture refunds.

At most four media validations hold admission across request, upstream call and response. Each phase shares a 64 MiB decoded-image budget, with pixel/frame and reference-work limits. Blocking decoder jobs retain their admission on caller cancellation. Header-only rules do not occupy media admission or parse bodies. Validation's JSON response buffer is capped at 32 MiB; request and Tools limits remain their existing smaller bounds. Response validation disables successful-response caches for that request. Errors contain codes without header values, images, asset content or decoder diagnostics. Configured validation paths suppress raw request/response debug logging and business-header values from the outermost ingress layer onward, even when debug controls are requested. Stored log bodies contain a redaction marker and whitelisted real usage facts; original sizes, status, token counts and request/workspace IDs remain. Header-derived custom telemetry and body-derived observation are omitted on these paths. Unconfigured paths keep existing logging behavior.

```sh
python3 scripts/self_hosted_smoke.py --validation-only
```

The pure validators and nine image/box regression scenarios are adapted from `openmodels-gateway` (source baseline `42e793a`, Apache-2.0 package metadata). PNG `0.18.1`, zune-core `0.5.3` and zune-jpeg `0.5.15` are pinned; the `image` crate is only a fixture encoder in tests. No source database-contract loader, commercial rules or billing engine is included.

## Optional downstream heartbeats

Disabled when `dispatcher.downstream-heartbeat` is absent. A configured policy sends idle SSE comments on selected Unified API endpoints:

```yaml
dispatcher:
  downstream-heartbeat:
    interval-ms: 15000
    paths: [chat/completions, responses]
    early-sse: false
    json-whitespace: false
    max-frame-bytes: 262144
```

`early-sse: true` permits SSE comments before the upstream response is ready. `json-whitespace: true` permits newline whitespace while a non-streaming JSON result is pending. Both are separate opt-ins. Intervals must be 10–60000 ms, unfinished frames 1 KiB–4 MiB. Only configured JSON model endpoints can use these modes; audio, multipart uploads, tools and other binary endpoints cannot be selected. Authentication, workspace admission and model policy run before the early response wrapper.

Before any early heartbeat, the original HTTP status is retained. After it commits HTTP 200, later errors are carried by an explicit JSON `error` object or an SSE error frame followed by one `[DONE]`, with `gateway_status` for the classified result and `upstream_status` only when trusted transport metadata is available. These statuses can differ, for example actual upstream 201 with a business failure classified as 400. JSON clients must check this error envelope when opting into whitespace mode; an HTTP 200 alone is no longer a success assertion. The configured business-error classifier supplies safe error categories/codes. No successful completion is synthesized after an interrupted SSE stream without a terminal event.

Heartbeats are inserted after response mapping and the usage/TTFT logging tap. They do not count as a model token, usage or cached content; `downstream_heartbeat_total` counts them separately. Idle comments are emitted between complete SSE frames, including when upstream frame bytes arrive in separate transport chunks. `Cache-Control: no-store, no-transform` and `X-Accel-Buffering: no` request streaming from proxies; configure the actual proxy accordingly.

Active heartbeat requests disable retries, reconnects, regional/cross-provider fallback and response caches, even before their first business output. Their upstream SSE transport polls on demand, keeps one transport chunk and a bounded unfinished frame, and uses a four-chunk log queue with backpressure. The response/log body budget is 32 MiB; exceeding it produces a terminal failure rather than accumulating indefinitely. The existing dispatcher total timeout remains in force. Closing the client body drops its pending operation and I/O. These limits and error semantics apply only to the opt-in path.

```sh
# Uses a local flushing reverse proxy, split SSE frames, delayed/failing mock upstream and OpenAI SDK.
python3 scripts/self_hosted_smoke.py --heartbeat-only
```
