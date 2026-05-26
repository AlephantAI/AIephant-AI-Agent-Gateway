<h1 align="center">
  <img src="docs/images/readme/alephant-logo.png" alt="Alephant logo" width="42" />
  Alephant AI Gateway
</h1>

<p align="center">
  <strong>Open source, OpenAI-compatible AI Gateway for 50+ providers, 320+ models, and custom model backends.</strong><br />
  Route traffic, adapt provider APIs, cache responses, enforce policy, and observe every request from one developer-friendly integration point.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-GPL%20v3-blue.svg?style=flat-square" /></a>
  <img alt="Edition" src="https://img.shields.io/badge/build-external%20%7C%20internal-black?style=flat-square" />
  <img alt="Version" src="https://img.shields.io/badge/version-0.2.0--beta.30-orange?style=flat-square" />
  <img alt="Providers" src="https://img.shields.io/badge/providers-50%2B-teal?style=flat-square" />
  <img alt="Models" src="https://img.shields.io/badge/models-320%2B-0052FF?style=flat-square" />
  <img alt="Rust edition" src="https://img.shields.io/badge/rust-edition%202024-dea584?style=flat-square&logo=rust&logoColor=white" />
</p>

<p align="center">
  <a href="https://x.com/alephantai" rel="noopener noreferrer" target="_blank"><img alt="Follow X" src="https://img.shields.io/badge/Follow%20X-000000?style=flat-square&logo=x&logoColor=white" /></a>
  <a href="https://discord.gg/tRQghcXhaH" rel="noopener noreferrer" target="_blank"><img alt="Discord" src="https://img.shields.io/badge/Discord-5865F2?style=flat-square&logo=discord&logoColor=white" /></a>
  <a href="https://t.me/alephantai" rel="noopener noreferrer" target="_blank"><img alt="Telegram" src="https://img.shields.io/badge/Telegram-26A5E4?style=flat-square&logo=telegram&logoColor=white" /></a>
</p>

<p align="center">
  <img alt="Hosted SaaS" src="https://img.shields.io/badge/hosted%20SaaS-ready-00C853?style=flat-square" />
  <img alt="Self-hostable" src="https://img.shields.io/badge/self--hostable-yes-00C853?style=flat-square" />
  <img alt="BYO keys" src="https://img.shields.io/badge/BYO%20keys-ready-00C853?style=flat-square" />
  <img alt="Agent clients" src="https://img.shields.io/badge/agent%20clients-supported-00C853?style=flat-square" />
</p>

<p align="center">
  <img src="docs/images/readme/ai-gateway-cover.png" alt="Alephant AI Gateway cover" width="900" />
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> ·
  <a href="https://alephant.io/">Website</a> ·
  <a href="#features">Features</a> ·
  <a href="#ide-integration">IDE</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="#screenshots">Screenshots</a> ·
  <a href="#comparison">Comparison</a> ·
  <a href="#community">Community</a> ·
  <a href="https://developers.alephant.io/">Docs</a>
</p>

<p align="center">
  <a href="https://alephant.io/"><b>Get started -></b></a> ·
  <a href="README.zh-CN.md">Simplified Chinese</a>
</p>

## What is Alephant AI Gateway

Alephant AI Gateway is an OpenAI-compatible control layer for production AI applications, available as hosted SaaS or as a self-hosted gateway. It gives developers one stable API surface while the gateway handles provider-specific adaptation, model routing, policy enforcement, layered caching, retries, fallback, usage metadata, request logging, and audit trails.

Instead of wiring every application directly to every provider, teams connect once and route across 50+ providers, 320+ models, and custom model backends. Start with Alephant Cloud for a managed workspace, or self-host the gateway when you need private infrastructure, BYO keys, and direct operational control.

```typescript
import OpenAI from "openai"

const openai = new OpenAI({
  baseURL: "https://ai.alephant.io/v1",
  defaultHeaders: {
    Authorization: `Bearer ${process.env.ALEPHANT_API_KEY}`,
    "Alephant-Session-Id": "session-xxx", // optional
  }
})
```

## Project status

Alephant AI Gateway is currently in beta (`0.2.0-beta.30`). Alephant Cloud is the hosted SaaS path, and this repository provides the gateway runtime for self-hosted and platform-connected deployments. Public APIs, configuration fields, and internal build modes may evolve before a stable `1.0` release.

---

## Why this exists

AI applications are moving from single-model prototypes to production systems that call many providers, agents, tools, and custom model backends. Without a gateway, every team ends up rebuilding the same operational layer: provider adapters, routing rules, key management, usage metadata, retries, caching, and request logs.

Alephant AI Gateway centralizes that layer behind one OpenAI-compatible API. It gives developers a stable integration surface while platform teams get policy before provider access, cache before repeated calls, fallback before outages, and audit trails before production incidents.

The goal is simple: make AI traffic observable, governable, and reliable without slowing developers down. [Learn more ->](https://alephant.io/)

<a id="features"></a>

## Features

| Capability | What Alephant AI Gateway provides |
| --- | --- |
| One API surface | OpenAI-compatible `/v1/*` and `/ai/*` routes for chat, responses, embeddings, images, and provider-style model names |
| Provider and model coverage | 50+ providers, 320+ models, local runtimes, OpenRouter-style catalogs, and custom/private backends |
| Provider adaptation | Request, tool, streaming, error, usage, finish-reason, and response normalization across provider APIs |
| Routing and resilience | Direct provider paths, policy routers, retries, fallback, health checks, provider 429 handling, and fail-open cache paths |
| Agent client compatibility | OpenAI-compatible formats for Cursor, Codex, opencode, and Antigravity workflows |
| IDE integration | Cursor-ready with architecture rules, workflow guides, implementation skills, and task management; opencode, Codex, and Claude Code adapters in progress |
| Policy and key control | Virtual keys, master key resolution, model policy, workspace provider allowlists, and concurrency controls |
| Caching | Gateway-side LLM KV cache and semantic cache to avoid repeated upstream calls |
| Observability | Request logs, traces, metrics, usage metadata, optional body archival, and downstream log delivery |
| Live operations | Route, virtual key, and provider key refresh from database changes without restarting the gateway |
| Deployment | Hosted SaaS through Alephant Cloud, or self-hosted Rust gateway with PostgreSQL, Redis, Qdrant, and S3-compatible integrations |

## Developer surface

| Surface | Purpose |
| --- | --- |
| `/v1/*` | Drop-in OpenAI-compatible API for existing SDKs and agent clients |
| `/router/{id}/*` | Policy-driven routing through a configured router |
| `/{provider}/*` | Direct provider passthrough when you want explicit upstream control |
| `model=provider/model_id` | Select a provider and model without changing application code |
| Custom backends | Put private models or self-hosted runtimes behind the same gateway contract |

<h2 id="architecture">Architecture & request lifecycle</h2>

<p align="center">
  <img src="docs/images/readme/ai-gateway-architecture.png" alt="Architecture & request lifecycle" width="900" />
</p>

Every request passes through the same gateway lifecycle: global middleware, routing, provider mapping, dispatch, cache, fallback, and async logging. The entry path depends on how much control you want:

| Path | Use it for |
| --- | --- |
| `/v1/*` | Unified OpenAI-style access with `model=provider/model_id` |
| `/router/{id}/*` | Policy-driven routing through a configured router |
| `/{provider}/*` | Direct provider passthrough when you want an explicit upstream |

## Multi-provider adaptation

Use one OpenAI-style request shape across 50+ providers and 320+ models, including OpenAI-compatible APIs, Anthropic Messages, Gemini, Bedrock, Ollama, OpenRouter-style catalogs, and custom backends. The client selects a runtime with `model=provider/model_id`; Alephant resolves the provider, applies the right adapter, maps provider-specific fields, and returns a normalized OpenAI-style response.

Instead of listing every model in the README, this section focuses on the contract: one request format in, one consistent response out. The provider and model catalog can evolve independently without forcing application code changes.

<p align="center">
  <img src="docs/images/readme/ai-gateway-multi-provider.png" alt="Multi-provider adaptation" width="900" />
</p>

<blockquote>
  <table>
    <tr>
      <td><strong>Mainstream models</strong></td>
      <td>GPT-4o · GPT-4.1 · o3 · Claude 3.5/3.7 Sonnet · Claude Opus · Gemini 1.5/2.0 · Llama 3/4 · Mistral Large · Command R+</td>
    </tr>
    <tr>
      <td><strong>Provider ecosystem</strong></td>
      <td>OpenAI · Anthropic · Google Gemini · AWS Bedrock · Azure OpenAI · OpenRouter · Together AI · Fireworks · Groq · Cohere · Mistral · Perplexity · DeepSeek · xAI · Ollama</td>
    </tr>
    <tr>
      <td><strong>Agent client compatibility</strong></td>
      <td>Cursor · Codex · opencode · Antigravity</td>
    </tr>
  </table>
</blockquote>

<a id="ide-integration"></a>

## IDE integration

Alephant AI Gateway ships repository-level tooling for AI-assisted development inside supported IDEs.

| IDE / Agent Client | Status | What's included |
| --- | --- | --- |
| Cursor | Ready | Project architecture & code-convention rules, development & API workflow guides, gated-module-implementation skill (Skill), file-based task management (Task Magic) — see the `.cursor` directory; also configure the gateway in Agent Settings → Models |
| opencode | In progress | Adapter and configuration under development |
| Codex | In progress | Adapter and configuration under development |
| Claude Code | In progress | Adapter and configuration under development |

<a id="quickstart"></a>

## Quickstart

### Use Alephant Cloud (hosted SaaS)

Keep your existing OpenAI SDK and change only the base URL plus authorization header. Your app keeps using familiar OpenAI-style calls while Alephant Cloud gives you the managed workspace, hosted gateway endpoint, provider resolution, routing, caching, logging, and fallback.

Set your gateway key:

```bash
export ALEPHANT_API_KEY="vk-..."
```

Smoke-test with `curl`:

```bash
curl https://ai.alephant.io/v1/chat/completions \
  -H "Authorization: Bearer $ALEPHANT_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "openai/gpt-4o",
    "messages": [
      { "role": "user", "content": "Explain Alephant AI Gateway in one sentence." }
    ]
  }'
```

Or use the OpenAI SDK:

```typescript
import OpenAI from "openai"

const openai = new OpenAI({
  baseURL: "https://ai.alephant.io/v1",
  defaultHeaders: {
    Authorization: `Bearer ${process.env.ALEPHANT_API_KEY}`,
    "Alephant-Session-Id": "demo-session", // optional: group requests into a trace/session
  }
})

const response = await openai.chat.completions.create({
  model: "openai/gpt-4o",
  messages: [
    { role: "user", content: "Explain Alephant AI Gateway in one sentence." }
  ]
})

console.log(response.choices[0]?.message?.content)
```

[Get started ->](https://alephant.io/)

## Self-host from source

Alephant AI Gateway can run as an independent self-hosted Rust service. You can point your own applications at the local gateway, connect it to your own PostgreSQL/Redis/Qdrant/S3-compatible infrastructure, and control provider keys, router configuration, cache behavior, and logging destinations from your deployment.

Self-hosting is useful when you need the gateway inside your own network, want full control over upstream provider credentials, or need to test provider adaptation and routing behavior before connecting to Alephant Cloud.

### Prerequisites

| Dependency | Required | Used for |
| --- | --- | --- |
| Rust toolchain | Yes | Build and run the gateway service |
| PostgreSQL | Yes | Router, key, workspace, and runtime configuration |
| Redis | Recommended | Shared runtime state, concurrency controls, and cache-related paths |
| Qdrant | Optional | Semantic cache |
| S3-compatible storage | Optional | Large request/response body archival |

Build `ai-gateway` with exactly one of `--features external` or `--features internal`.

### Build

```bash
cargo build -p ai-gateway --features external
```

Use `external` for the public/open deployment mode, or `internal` when running with the internal KV/backend assumptions used by your environment. Only enable one of these feature sets at a time.

### Run locally

```bash
cargo run -p ai-gateway --features external -- -c ./ai-gateway/config/local.yaml
```

The config file controls database connections, provider settings, cache services, observability, and runtime behavior. For local development, start with `ai-gateway/config/local.yaml` and adjust it to match your services.

### Configuration

The gateway reads a YAML config file and supports environment overrides for sensitive values. Keep secrets such as provider keys, S3 credentials, and Redis URLs out of committed YAML whenever possible.

Useful starting points:

| File | Purpose |
| --- | --- |
| `ai-gateway/config/local.yaml` | Local development defaults |
| `ai-gateway/config/local-cloud.yaml` | Local cloud-style integration |
| `ai-gateway/config/alephant-cloud.yaml` | Alephant platform-connected deployment shape |

Environment overrides follow the `AI_GATEWAY__...` pattern used by the config loader, for example `AI_GATEWAY__S3__ACCESS_KEY`, `AI_GATEWAY__S3__SECRET_KEY`, and `AI_GATEWAY__REQUEST_LOG__LOG_QUEUE_REDIS_URL`.

### Verify

Keep the local gateway process running. The smoke harness targets the default local gateway URL, `http://localhost:8080`.

```bash
cargo run -p test
```

You can also point an OpenAI-compatible SDK at your self-hosted gateway:

```typescript
import OpenAI from "openai"

const openai = new OpenAI({
  baseURL: "http://localhost:8080/v1",
  defaultHeaders: {
    Authorization: `Bearer ${process.env.ALEPHANT_VIRTUAL_KEY}`,
  }
})
```

### Integration tests

```bash
cargo test -p ai-gateway --tests --features "external integration"
```

## Security & privacy

Alephant AI Gateway is designed for both managed SaaS usage and self-hosted deployments where teams need control over provider credentials, request metadata, and deployment boundaries.

| Area | Gateway behavior |
| --- | --- |
| BYO provider keys | Provider credentials can stay under your control through gateway configuration and key resolution |
| Virtual key isolation | Application-facing keys can be separated from upstream provider keys |
| Optional body archival | Request/response body storage is configurable rather than mandatory |
| SaaS or self-host | Use Alephant Cloud for managed operations, or run the gateway inside your own infrastructure |
| Policy gates | Model policy, provider allowlists, and concurrency controls can be enforced before upstream dispatch |

## Runtime internals

| Capability | Why it matters |
| --- | --- |
| DB listener-driven hot reload | Route and key changes can be picked up without restarting the gateway |
| S3-compatible body storage | Request and response bodies can be archived outside the hot request path when enabled |
| Downstream request-log delivery | Structured gateway logs can be pushed to Alephant or another downstream system |
| Content-filter integration | Optional gRPC filter path with fail-open reconnect behavior |
| Workspace concurrency guard | Redis-backed controls help protect shared upstream capacity |
| Provider 429 monitoring | Provider rate-limit signals can feed discovery and routing decisions |

## Screenshots

Explore the Alephant workspace experience around the gateway: usage overview, request logs, sessions, cache visibility, insights, and governance controls.

| Overview | Request logs |
| --- | --- |
| ![Alephant AI Gateway overview dashboard](docs/images/readme/screenshots/overview.png)<br /><sub>Workspace-level usage, request volume, latency, tokens, and cache health.</sub> | ![Alephant AI Gateway request logs](docs/images/readme/screenshots/requests.png)<br /><sub>Request-level inspection for status, model, source, tokens, cost, and upstream outcome.</sub> |

| Sessions | Cache |
| --- | --- |
| ![Alephant AI Gateway sessions](docs/images/readme/screenshots/sessions.png)<br /><sub>Trace agent and application journeys across steps, duration, spend, and status.</sub> | ![Alephant AI Gateway cache dashboard](docs/images/readme/screenshots/cache.png)<br /><sub>Monitor cache hits, savings, repeated prompts, and frequently reused responses.</sub> |

| Insights | Governance |
| --- | --- |
| ![Alephant AI insights dashboard](docs/images/readme/screenshots/insights.png)<br /><sub>Surface reliability, spend, and efficiency signals from gateway traffic.</sub> | ![Alephant AI governance controls](docs/images/readme/screenshots/governance.png)<br /><sub>Configure usage limits, budget controls, rate limits, and policy rules.</sub> |

<a id="comparison"></a>

## Comparison

Portkey, Alephant, and LiteLLM are excellent projects, but they start from different centers of gravity. Alephant is built for teams shipping agentic AI products: a hosted SaaS workspace plus a self-hosted gateway path for agent development, cost control, provider routing, governance, and operational visibility.

| Project | Best known for | Best fit |
| --- | --- | --- |
| Portkey | Enterprise AI gateway controls, guardrails, and managed policy workflows | Teams that want a managed AI control plane |
| Helicone | LLM observability, request analytics, sessions, and cost visibility | Teams whose primary need is tracing and analytics |
| LiteLLM | Broad Python proxy/SDK ecosystem for many providers | Teams that want maximum provider breadth through a Python stack |
| Alephant AI Gateway | Agent development infrastructure, cost control, governance, provider routing, and SaaS + self-host deployment | Teams building production agents that need cost guardrails, request traceability, BYO keys, and multi-provider control |

| Capability | Portkey | Alephant | LiteLLM | Alephant AI Gateway |
| --- | --- | --- | --- | --- |
| OpenAI-compatible API | Yes | Yes | Yes | Yes |
| SaaS + self-host | Enterprise/self-host options | Hosted and self-host options | Self-hosted proxy | Yes: Alephant Cloud plus self-hosted Rust gateway |
| Provider/model coverage | Broad | Broad logging/proxy coverage | Very broad | 50+ providers, 320+ models, custom backends |
| Agent coding clients | No dedicated compatibility layer | No dedicated compatibility layer | No dedicated compatibility layer | Cursor, Codex, opencode, Antigravity workflows |
| Agent cost control | Guardrails and policy controls | Cost analytics and request visibility | Budgets and spend controls | Agent/session-aware usage visibility, cache savings, budget controls, and governance workflows |
| Provider adaptation | Gateway policies and routing | Proxy plus observability pipeline | Strong provider abstraction | Explicit mappers for requests, streaming, errors, usage, and responses |
| Routing and resilience | Routing, retries, fallbacks | Gateway controls plus observability | Router, fallback, budgets | Direct paths, policy routers, fallback, health checks, provider 429 handling |
| BYO key control | Key vault / enterprise controls | BYO keys with proxy controls | Virtual keys and self-hosted keys | BYO provider keys, master-key resolution, workspace allowlists |
| Cache | Gateway caching | Cache tracking/integrations | Cache integrations | LLM KV cache plus semantic cache |
| Observability | Logs and policy events | Core strength | Callback/logging integrations | Logs, traces, metrics, usage metadata, optional body archival |
| Governance path | Strong enterprise guardrails | Workspace controls around observability | Teams, budgets, rate limits | Agent/session governance, model policy, provider allowlists, concurrency controls, and workspace-level controls |

Alephant's differentiator is the combination: hosted SaaS, self-hosted Rust gateway, agent-first developer compatibility, cost-control workflows, BYO-key governance, explicit provider adaptation, and workspace-level AI FinOps.

## Repository structure

```text
alephant-ai-gateway/
├── ai-gateway/                 # Gateway service crate
├── crates/                     # Shared libraries and harnesses
├── docs/                       # In-repo notes; curated docs at https://api.alephant.io/
├── scripts/                    # CI and local automation
├── infrastructure/             # Deployment and observability infra
├── test/                       # Integration and runtime test helpers
├── AGENTS.md                   # Agent collaboration conventions
├── CLAUDE.md                   # Command and architecture reference
└── CHANGELOG.md                # Project changelog
```

<a id="community"></a>

## Community

- Website: [alephant.io](https://alephant.io/)
- Docs: [developers.alephant.io](https://developers.alephant.io/)
- Discord: [discord.gg/tRQghcXhaH](https://discord.gg/tRQghcXhaH)
- Telegram: [t.me/alephantai](https://t.me/alephantai)
- X: [x.com/alephantai](https://x.com/alephantai)

## Contributing

Contributions are welcome through issues and pull requests.

Helpful contribution areas:

- Provider adapter correctness and API mapping.
- Routing, fallback, and resilience behavior.
- Observability and diagnostics quality.
- Test harness coverage and documentation clarity.

For substantial changes, include reproducible validation steps and feature-flag context (`external` or `internal`).

## License

Licensed under the [GPL License 3.0](LICENSE).
Upstream license continuity is preserved where applicable.

## Star History

<a href="https://www.star-history.com/?repos=AlephantAI%2FAIephant-AI-Gateway&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=AlephantAI/AIephant-AI-Gateway&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=AlephantAI/AIephant-AI-Gateway&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=AlephantAI/AIephant-AI-Gateway&type=date&legend=top-left" />
 </picture>
</a>
