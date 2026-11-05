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
