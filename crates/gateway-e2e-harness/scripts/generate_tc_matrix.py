#!/usr/bin/env python3
"""Generate tc-*.json harness cases aligned with docs/plans full coverage matrix.

Run from repo root:
  python3 crates/gateway-e2e-harness/scripts/generate_tc_matrix.py

Overwrites only files matching cases/tc-*.json (legacy l*.json unchanged).

Hand-maintained cases: do not add keys named `tc-sc-*.json` or `tc-pm-*.json` to the `files`
dict in this script (those prefixes are hand-authored only).
Existing `tc-sc-`/`tc-pm-` files on disk are not overwritten when this script runs unless an
entry with the same key is explicitly added to `files`.
"""

from __future__ import annotations

import json
from pathlib import Path

CASES_DIR = Path(__file__).resolve().parents[1] / "cases"

# Same curl shape as l4-openai-llm-kv-*.json (single $GATEWAY_BASE URL with slash).
KV_TWO_BUCKET_CURL = (
    "curl -X POST \"$GATEWAY_BASE/v1/chat/completions\" -H \"Authorization: Bearer legend\" "
    "-H \"Content-Type: application/json\" -H \"Alephant-Cache-Enabled: true\" "
    "-H \"Alephant-Cache-Bucket-Max-Size: 2\" "
    '-d \'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-two-buckets-$HARNESS_E2E_RUN_TOKEN"}],"max_tokens":4}\''
)

WIDE = {"httpStatus": {"in": [200, 400, 401, 403, 404, 422, 429, 500, 502, 503]}}
ERR_WIDE = {
    "httpStatus": {
        "in": [400, 401, 403, 404, 422, 429, 500, 502, 503],
    },
}

# Unified `/v1/chat/completions` JSON mapping → OpenAI-style `choices` (success).
OPENAI_CHAT_OK_JSON = {
    "json": {
        "pathExists": ["/choices", "/choices/0/message"],
        "arrayMinLength": {"/choices": 1},
    }
}
# See `ai-gateway` `ErrorResponse` (`/error`, `/error/message`).
API_ERROR_JSON = {"json": {"pathExists": ["/error", "/error/message"]}}


def merge_assert(*parts: dict) -> dict:
    """Shallow-merge assertion dicts; nested `json` and `headers` dicts merge one level deep."""
    out: dict = {}
    for p in parts:
        for k, v in p.items():
            if k in out and isinstance(out[k], dict) and isinstance(v, dict):
                merged = {**out[k], **v}
                out[k] = merged
            else:
                out[k] = v
    return out


def curl_d_json(inner: str) -> str:
    """Return a curl `-d '<json>'` fragment for `sh -c` + inject_curl_trailer.

    Use single-quoted JSON (inner double quotes). Avoid `\'` wrappers that
    break when combined with `-w '...'` in the harness.
    """
    if "'" in inner:
        raise ValueError("curl_d_json: JSON must not contain single quotes")
    return "-d '" + inner + "'"



def j(
    case_id: str,
    tier: str,
    intent: str,
    curl: str,
    assertions: dict,
    profile: list[str] | None = None,
) -> str:
    o: dict = {
        "caseId": case_id,
        "tier": tier,
        "intent": intent,
        "curl": curl,
        "assertions": assertions,
    }
    if profile is not None:
        o["profile"] = profile
    return json.dumps(o, ensure_ascii=False, indent=2) + "\n"


def main() -> None:
    CASES_DIR.mkdir(parents=True, exist_ok=True)
    files: dict[str, str] = {}

    gb = '"$GATEWAY_BASE"'
    vk = '"Authorization: Bearer legend"'
    ct = '-H "Content-Type: application/json"'

    # --- A CFG ---
    files["tc-cfg-001-smoke.json"] = j(
        "CFG-001",
        "L1",
        "Smoke: gateway accepts traffic after startup (same probe as CFG-007)",
        f"curl -X GET {gb}/health",
        {"httpStatus": 200},
        ["gate", "full"],
    )
    files["tc-cfg-004-alephant-smoke.json"] = j(
        "CFG-004",
        "L2",
        "End-to-end success; alephant.log_collector_url for logging is not assertable via HTTP alone",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"cfg-004"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-cfg-005-provider-base-smoke.json"] = j(
        "CFG-005",
        "L2",
        "Provider default base_url: smoke via successful OpenAI unified call",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"cfg-005"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-cfg-006-master-key-base-url.json"] = j(
        "CFG-006",
        "L2",
        "master_keys.base_url: configure via DB/env then run; wide status until verified",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"cfg-006"}],"max_tokens":4}'),
        WIDE,
        ["full"],
    )

    # --- B AUTH ---
    files["tc-auth-001-valid-bearer.json"] = j(
        "AUTH-001",
        "L2",
        "Valid Bearer + successful unified call",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-001"}],"max_tokens":6}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-auth-002-missing-bearer.json"] = j(
        "AUTH-002",
        "L1",
        "Missing Authorization on unified API (full only; gate uses ERR-001 as canonical missing-auth probe)",
        f"curl -X POST {gb}/v1/chat/completions {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-002"}],"max_tokens":4}'),
        merge_assert({"httpStatus": {"in": [400, 401, 403]}}, API_ERROR_JSON),
        ["full"],
    )
    files["tc-auth-003-auth-disabled.json"] = j(
        "AUTH-003",
        "L2",
        "Requires alephant.features=none (auth off). Isolated env only; skipped in gate/full by profile.",
        f"curl -X POST {gb}/v1/chat/completions {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-003"}],"max_tokens":4}'),
        WIDE,
        ["manual"],
    )
    files["tc-auth-004-vk-repeat.json"] = j(
        "AUTH-004",
        "L2",
        "Repeat VK request succeeds (internal hash cache)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-004"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-auth-005-no-provider-key.json"] = j(
        "AUTH-005",
        "L2",
        "Org missing provider key: use VK for org without keys; expect error",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-005"}],"max_tokens":4}'),
        WIDE,
        ["full"],
    )
    files["tc-auth-006-master-key-upstream.json"] = j(
        "AUTH-006",
        "L2",
        "Master key used upstream: verify via mock/logs; HTTP success when configured",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-006"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-auth-007-cold-provider-keys.json"] = j(
        "AUTH-007",
        "L2",
        "Cold path: first request after empty cache",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-007"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-auth-009-unified-no-router.json"] = j(
        "AUTH-009",
        "L2",
        "Unified API no router ownership check",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"auth-009"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    # --- C ROUTE ---
    files["tc-route-002-ai.json"] = j(
        "ROUTE-002",
        "L2",
        "/v1/chat/completions unified",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"route-002"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-route-003-v1.json"] = j(
        "ROUTE-003",
        "L2",
        "/v1/chat/completions alias",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"route-003"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-route-005-not-found.json"] = j(
        "ROUTE-005",
        "L1",
        "Unknown path",
        f"curl -X GET {gb}/definitely-not-a-route-xyz-12345/ping",
        merge_assert({"httpStatus": 404}, API_ERROR_JSON),
        ["gate", "full"],
    )
    files["tc-route-007-query.json"] = j(
        "ROUTE-007",
        "L2",
        "Query string on unified path",
        f"curl -X POST {gb}/v1/chat/completions?trace=route007 -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"route-007"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    # --- D UNI (extra rows; UNI-001..014 overlap with l2-* files) ---
    files["tc-uni-006-gemini-stream.json"] = j(
        "UNI-006",
        "L2",
        "Gemini stream",
        f"curl -N -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"gemini/gemini-2.0-flash","stream":true,"messages":[{"role":"user","content":"uni-006"}],"max_tokens":24}'),
        WIDE,
        ["full"],
    )
    files["tc-uni-008-ollama-stream.json"] = j(
        "UNI-008",
        "L2",
        "Ollama stream",
        f"curl -N -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"ollama/llama3.1:8b","stream":true,"messages":[{"role":"user","content":"uni-008"}],"max_tokens":24}'),
        WIDE,
        ["full"],
    )
    files["tc-uni-010-bedrock-stream.json"] = j(
        "UNI-010",
        "L2",
        "Bedrock stream",
        f"curl -N -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"bedrock/us.anthropic.claude-3-5-sonnet-20241022-v2:0","stream":true,"messages":[{"role":"user","content":"uni-010"}],"max_tokens":24}'),
        WIDE,
        ["full"],
    )
    files["tc-uni-011a-deepseek.json"] = j(
        "UNI-011",
        "L2",
        "Named provider deepseek",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"deepseek/deepseek-chat","messages":[{"role":"user","content":"uni-011a"}],"max_tokens":8}'),
        WIDE,
        ["full"],
    )
    files["tc-uni-011b-qwen.json"] = j(
        "UNI-011",
        "L2",
        "Named provider qwen (second matrix row)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"qwen/qwen-turbo","messages":[{"role":"user","content":"uni-011b"}],"max_tokens":8}'),
        WIDE,
        ["full"],
    )
    files["tc-uni-012-anthropic-tools.json"] = j(
        "UNI-012",
        "L2",
        "tool_calls on anthropic unified model",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"anthropic/claude-3-haiku","messages":[{"role":"user","content":"call get_time"}],"tools":[{"type":"function","function":{"name":"get_time","parameters":{"type":"object","properties":{}}}}],"max_tokens":64}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-uni-014-reasoning-effort.json"] = j(
        "UNI-014",
        "L2",
        "reasoning_effort field",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"uni-014"}],"reasoning_effort":"low","max_tokens":16}'),
        WIDE,
        ["full"],
    )

    # --- E PATH ---
    # --- F LCS ---
    files["tc-lcs-001-no-handler.json"] = j(
        "LCS-001",
        "L2",
        "No large-context handler",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-001"}],"max_tokens":8}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-lcs-002-truncate.json"] = j(
        "LCS-002",
        "L2",
        "truncate",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Token-Limit-Exception-Handler: truncate\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-002"}],"max_tokens":8}'),
        WIDE,
        ["full"],
    )
    files["tc-lcs-003-middle-out.json"] = j(
        "LCS-003",
        "L2",
        "middle-out",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Token-Limit-Exception-Handler: middle-out\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-003"}],"max_tokens":8}'),
        WIDE,
        ["full"],
    )
    files["tc-lcs-004-fallback.json"] = j(
        "LCS-004",
        "L2",
        "fallback + model override",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Token-Limit-Exception-Handler: fallback\" "
        + '-H "Alephant-Model-Override: openai/gpt-4o-mini" '
        + f"{ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-004"}],"max_tokens":8}'),
        WIDE,
        ["full"],
    )
    files["tc-lcs-005-fallback-no-candidate.json"] = j(
        "LCS-005",
        "L2",
        "fallback without second candidate",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Token-Limit-Exception-Handler: fallback\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-005"}],"max_tokens":8}'),
        WIDE,
        ["full"],
    )
    files["tc-lcs-007-alephant-only.json"] = j(
        "LCS-007",
        "L1",
        "Alephant-Session-Id without Alephant-Session-Id",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Session-Id: h1\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-007"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-lcs-008-session-path.json"] = j(
        "LCS-008",
        "L2",
        "Alephant session path without leading slash",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} "
        + '-H "Alephant-Session-Id: lcs008" -H "Alephant-Session-Path: a/b" '
        + f"{ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-008"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-lcs-010-prompt-unified.json"] = j(
        "LCS-010",
        "L2",
        "Prompt-ID on unified /ai (design boundary probe)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Prompt-ID: e2e-prompt-1\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"lcs-010"}],"max_tokens":4}'),
        WIDE,
        ["full"],
    )

    # --- G KV ---
    files["tc-kv-001-enabled.json"] = j(
        "KV-001",
        "L4",
        "Alephant-Cache-Enabled",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Enabled: true\" -H \"Alephant-Cache-Bucket-Max-Size: 2\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-001"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-kv-002-read-only.json"] = j(
        "KV-002",
        "L4",
        "Read only",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Read: true\" -H \"Alephant-Cache-Save: false\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-002"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-kv-003-save-only.json"] = j(
        "KV-003",
        "L4",
        "Save only",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Read: false\" -H \"Alephant-Cache-Save: true\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-003"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-kv-004-two-buckets-t1.json"] = j(
        "KV-004",
        "L4",
        "Two-slot bucket: request 1 (align with l4-openai-llm-kv-t1)",
        KV_TWO_BUCKET_CURL,
        {"httpStatus": 200, "headers": {"notContains": {"alephant-cache": "HIT"}}},
        ["full"],
    )
    files["tc-kv-004-two-buckets-t2.json"] = j(
        "KV-004",
        "L4",
        "Two-slot bucket: request 2",
        KV_TWO_BUCKET_CURL,
        {"httpStatus": 200, "headers": {"notContains": {"alephant-cache": "HIT"}}},
        ["full"],
    )
    files["tc-kv-004-two-buckets-t3.json"] = j(
        "KV-004",
        "L4",
        "Two-slot bucket: request 3 expect HIT",
        KV_TWO_BUCKET_CURL,
        {
            "httpStatus": 200,
            "headers": {
                "contains": {"alephant-cache": "HIT"},
                "exists": ["alephant-cache-bucket-idx"],
            },
        },
        ["full"],
    )
    files["tc-kv-005-invalid-bucket.json"] = j(
        "KV-005",
        "L2",
        "Invalid bucket size 0 -> 5xx",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Bucket-Max-Size: 0\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-005"}],"max_tokens":4}'),
        {"httpStatus": {"in": [500, 502, 503]}},
        ["full"],
    )
    files["tc-kv-006-ttl-note.json"] = j(
        "KV-006",
        "L4",
        "TTL: wait for expiry then MISS — use manual timing or CH; probe only",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Enabled: true\" -H \"Alephant-Cache-Control: max-age=2\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-006-ttl"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["manual"],
    )
    files["tc-kv-007-hit-headers.json"] = j(
        "KV-007",
        "L4",
        "Hit headers: use tc-kv-004-t3 or l4-* three-step",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Enabled: true\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-007"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-kv-008-seed.json"] = j(
        "KV-008",
        "L4",
        "Cache seed isolation",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Cache-Enabled: true\" -H \"Alephant-Cache-Seed: seed-a\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"kv-008"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )

    # --- H RL ---
    for n in range(1, 11):
        files[f"tc-rl-{n:03d}.json"] = j(
            f"RL-{n:03d}",
            "L2",
            f"RL-{n:03d}: rate-limit semantics need burst/load or integration tests; "
            f"single-request probe (manual profile skips default runs)",
            f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
            + curl_d_json(
                r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"rl-'
                + f"{n:03d}"
                + r'"}],"max_tokens":2}'
            ),
            {"httpStatus": {"in": [200, 429]}},
            ["manual"],
        )

    # --- J FB ---
    for n in range(1, 11):
        if n == 9:
            fb_intent = (
                "FB-009: invalid fallback_policy at startup — cargo test / "
                "config validation (manual)"
            )
        elif n == 10:
            fb_intent = (
                "FB-010: fallback decision observability emit — "
                "fallback_observability_logs tests (manual)"
            )
        else:
            fb_intent = (
                f"FB-{n:03d}: retry/health/meltdown — see ai-gateway/tests fallback_* (manual)"
            )
        files[f"tc-fb-{n:03d}.json"] = j(
            f"FB-{n:03d}",
            "L2",
            fb_intent,
            f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
            + curl_d_json(
                r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"fb-'
                + f"{n:03d}"
                + r'"}],"max_tokens":4}'
            ),
            WIDE,
            ["manual"],
        )

    # --- K OBS ---
    files["tc-obs-001-ch-smoke.json"] = j(
        "OBS-001",
        "L2",
        "Observability HTTP smoke: when enabled, request logging may emit downstream telemetry; "
        "this JSON does not include a ch block",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"obs-001"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )
    files["tc-obs-002-no-log-note.json"] = j(
        "OBS-002",
        "L2",
        "Observability off: requires features without observability (manual env)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"obs-002"}],"max_tokens":4}'),
        WIDE,
        ["manual"],
    )
    for n in [3, 4, 5]:
        files[f"tc-obs-{n:03d}.json"] = j(
            f"OBS-{n:03d}",
            "L2",
            f"OBS-{n:03d}: large body / cache log fields — see logger tests (manual)",
            f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
            + curl_d_json(
                r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"obs-'
                + f"{n:03d}"
                + r'"}],"max_tokens":4}'
            ),
            WIDE,
            ["manual"],
        )
    files["tc-obs-006-x-request-id.json"] = j(
        "OBS-006",
        "L1",
        "x-request-id present",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"obs-006"}],"max_tokens":4}'),
        merge_assert(
            {"httpStatus": 200, "headers": {"exists": ["x-request-id"]}},
            OPENAI_CHAT_OK_JSON,
        ),
        ["full"],
    )
    files["tc-obs-007-redaction-note.json"] = j(
        "OBS-007",
        "L2",
        "Log redaction: verify in log ingest (manual)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"obs-007"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["manual"],
    )
    files["tc-obs-008-sessionid-log-note.json"] = j(
        "OBS-008",
        "L2",
        "sessionId in logs when Alephant-Session-Id set",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Alephant-Session-Id: obs008\" {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"obs-008"}],"max_tokens":4}'),
        merge_assert({"httpStatus": 200}, OPENAI_CHAT_OK_JSON),
        ["full"],
    )

    # --- L ERR ---
    files["tc-err-001-unauthorized.json"] = j(
        "ERR-001",
        "L1",
        "Unauthorized error shape",
        f"curl -X POST {gb}/v1/chat/completions {ct} "
        + r"-d '{\"model\":\"openai/gpt-4o-mini\",\"messages\":[]}'",
        merge_assert({"httpStatus": {"in": [400, 401, 403]}}, API_ERROR_JSON),
        ["gate", "full"],
    )
    files["tc-err-002-bad-json.json"] = j(
        "ERR-002",
        "L1",
        "Malformed JSON body",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} -H \"Content-Type: application/json\" -d 'not-json{{'",
        merge_assert(ERR_WIDE, API_ERROR_JSON),
        ["full"],
    )
    files["tc-err-003-unsupported-model.json"] = j(
        "ERR-003",
        "L1",
        "L1: unknown provider/model → API error JSON (canonical; merged legacy l1-unified-unsupported-model-fake-key)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"unknown-provider/not-a-real-model","messages":[{"role":"user","content":"err-003"}],"max_tokens":4}'),
        merge_assert(ERR_WIDE, API_ERROR_JSON),
        ["gate", "full"],
    )
    files["tc-err-004-bad-endpoint.json"] = j(
        "ERR-004",
        "L1",
        "Unsupported route / method (GET on POST-only path); body often JSON error",
        f"curl -X GET {gb}/v1/chat/completions",
        {
            "httpStatus": {"in": [400, 404, 405, 422]},
            "body": {"contains": ['"error"']},
        },
        ["gate", "full"],
    )
    files["tc-err-005-compat-note.json"] = j(
        "ERR-005",
        "L2",
        "compat_mode=true behavior — run with compat gateway (manual)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"err-005"}],"max_tokens":4}'),
        WIDE,
        ["manual"],
    )
    files["tc-err-006-model-routing.json"] = j(
        "ERR-006",
        "L2",
        "compat_mode=false: model selects provider (default deployment)",
        f"curl -X POST {gb}/v1/chat/completions -H {vk} {ct} "
        + curl_d_json(r'{"model":"openai/gpt-4o-mini","messages":[{"role":"user","content":"err-006"}],"max_tokens":4}'),
        {"httpStatus": 200},
        ["full"],
    )
    files["tc-err-007-panic-note.json"] = j(
        "ERR-007",
        "L1",
        "CatchPanic: no single HTTP recipe (manual/fuzz)",
        f"curl -X GET {gb}/health",
        {"httpStatus": 200},
        ["manual"],
    )
    files["tc-err-008-shutdown-note.json"] = j(
        "ERR-008",
        "L1",
        "Graceful shutdown — integration (manual)",
        f"curl -X GET {gb}/health",
        {"httpStatus": 200},
        ["manual"],
    )

    # --- M DATA ---
    for n in range(1, 9):
        files[f"tc-data-{n:03d}.json"] = j(
            f"DATA-{n:03d}",
            "L2",
            f"DATA-{n:03d}: DB/Redis hot reload / pool — see ai-gateway/tests (manual)",
            f"curl -X GET {gb}/health",
            {"httpStatus": 200},
            ["manual"],
        )

    # --- Non-harness doc IDs (startup / cargo only) ---
    files["tc-cfg-002-config-invalid-note.json"] = j(
        "CFG-002",
        "L0",
        "CFG-002: invalid config at startup — cargo test / cli (manual)",
        f"curl -X GET {gb}/health",
        {"httpStatus": 200},
        ["manual"],
    )
    files["tc-cfg-003-feature-matrix-note.json"] = j(
        "CFG-003",
        "L0",
        "CFG-003: external/internal feature mutual exclusion — build-time (manual)",
        f"curl -X GET {gb}/health",
        {"httpStatus": 200},
        ["manual"],
    )

    # Hand-maintained file name prefixes: never add these keys to `files` in this script.
    _RESERVED_HANDWRITTEN_PREFIXES = ("tc-sc-", "tc-pm-")

    # Write
    for name, content in files.items():
        if not name.startswith("tc-") or not name.endswith(".json"):
            continue
        for prefix in _RESERVED_HANDWRITTEN_PREFIXES:
            if name.startswith(prefix):
                raise RuntimeError(
                    f"refuse to write generated content over hand-maintained "
                    f"prefix {prefix!r}: {name!r} — use a hand-written file or "
                    f"rename the generated case"
                )
        (CASES_DIR / name).write_text(content, encoding="utf-8")

    print(f"wrote {len(files)} files to {CASES_DIR}")


if __name__ == "__main__":
    main()
