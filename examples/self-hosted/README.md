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

OpenAPI tool targets can independently enable `detect-business-errors: true`. The tool envelope then reports a failure while metadata retains the original upstream HTTP status. Existing failure-charge settings continue to apply; the classifier does not introduce a balance or billing service. With the option absent, tool response behavior and snapshot serialization remain compatible.

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

`POST /v1/audio/speech` accepts JSON (`model`, `input`, `voice`) and forwards raw audio bytes and the upstream content type. `POST /v1/audio/transcriptions` accepts multipart (`model`, one nonempty `file`), preserving the filename, file bytes and optional fields such as `prompt`, `language` and `response_format`. JSON, text, SRT and VTT upstream responses are forwarded without chat conversion. This version supports raw audio speech and non-streaming transcription; speech SSE and transcription event streaming are rejected.

Supported wire protocols: OpenAI, named OpenAI-compatible providers and a configured custom OpenAI-compatible base URL. This describes protocol support, not a guarantee that every provider offers every media model. The key's bound provider selects the upstream; its matching prefix is removed from `model`. The existing virtual-key policy applies to the original model: `allowed-models` overrides matching blocks, `blocked-models` denies exact matches, and unmatched models remain allowed (it is not a restrictive allowlist).

The upload cap is **1 MiB including multipart framing**, matching the existing model-body bound; multipart permits up to 128 parts and 8 KiB of headers per part. Duplicate singleton fields, malformed forms and missing model/file fields are rejected before sending. Files and all non-model byte ranges remain intact. Media calls skip LLM/semantic caches and automatic retries/fallback, retaining authentication, workspace quotas, upstream credential sanitization and normal request logs. No duration-to-token estimate or new billing policy is applied; absent upstream usage remains absent/zero in existing token fields.

```sh
# Uses the installed openai Python SDK, generated local keys and a local mock; no paid API calls.
python3 scripts/self_hosted_smoke.py --audio-only
```

API references: [speech](https://developers.openai.com/api/reference/resources/audio/subresources/speech/methods/create), [transcriptions](https://developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create).
