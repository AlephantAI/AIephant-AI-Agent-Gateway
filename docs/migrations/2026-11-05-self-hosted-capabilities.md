# Self-hosted gateway capabilities

This branch adapts seven capabilities to the existing Alephant production runtime. It adds an authenticated local-key profile so the examples need no OpenAny backend, database, policy service, commercial billing service or paid upstream.

| Capability | Opt-in / interface | Main boundary |
| --- | --- | --- |
| Business failures in HTTP success | `dispatcher.business-error-checks`, OpenAPI `detect-business-errors` | Exact endpoint, bounded JSON probe, safe public errors |
| Workspace request quota | `global.identity-rate-limit` | Memory for one process, Redis shared across processes; Redis failures deny admission |
| MCP versions | Existing Streamable HTTP target | March/June/November 2025 tools lifecycle, actual negotiated headers and cache checks |
| Speech and transcription | `/v1/audio/speech`, `/v1/audio/transcriptions` | OpenAI-compatible protocol, raw audio / non-streaming transcription |
| Image edits | `/v1/images/edits` | Byte-preserving multipart, single/multi-image and mask, non-streaming |
| Header/image/box validation | `dispatcher.validation-rules`, OpenAPI `validation`/`request-contract` | Local rules, actual decoding and finite budgets, response failure retains upstream facts |
| Downstream heartbeats | `dispatcher.downstream-heartbeat` | Idle SSE, explicit early SSE / JSON whitespace, bounded forwarding and explicit post-commit errors |

See [the self-hosted profile](../../examples/self-hosted/README.md) for complete options, support limits and production smoke commands. Configuration absent preserves existing endpoint behavior. Existing database-backed authentication remains supported. Stable local key/workspace IDs require no database migration; credentials stay in environment variables. Runtime YAML changes require a restart.

Model policy retains Alephant's existing semantics: allow entries override matching blocks and unmatched models remain allowed. Media uploads retain the existing 1 MiB body bound. No cross-vendor binary conversion, duration-to-token billing estimate, generic JSON Schema engine, task/sampling MCP capability or streaming image-edit protocol is claimed.

The feature commits are ordered: local-auth baseline, business errors, quotas, MCP, audio, image edits, validation, heartbeats. Roll back dependent commits in reverse order, then remove the corresponding optional YAML. New local-auth deployments must retain their authentication baseline until replaced by a working database identity configuration.

Verification uses the real `internal` binary, generated temporary credentials, local HTTP upstreams, optional temporary Redis and a local reverse proxy. No production authentication bypass or paid supplier is involved. The external library suite is also run. `external` and `internal` are mutually exclusive build features.

Some pinned Git dependencies were fetched from their official locked-commit archives because direct Git fetches were unavailable. Local verification used `/private/tmp/alephant-cargo-sources.toml` to map those exact archives; this file and the machine proxy are not project dependencies. Cargo manifests retain the original Git sources and locked revisions. Normal builds use Cargo's ordinary source resolution.

The target repository's GPLv3 license is retained. Pure validation code/fixtures are attributed to the source package, whose manifest declares Apache-2.0; image decoder versions are pinned and their notices are recorded separately. Publishing, remote pushes and main-branch integration are separate release actions.
