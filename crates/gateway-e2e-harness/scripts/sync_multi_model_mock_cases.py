#!/usr/bin/env python3
"""Generate per-provider multi_model_service mock cases (VK legend_<code>).

Source of truth: starter model per provider in
  docs/sql/migrations/20260414_seed_provider_catalog_starter_models.sql
(provider_model_seed VALUES).

Run from repo root:
  python3 crates/gateway-e2e-harness/scripts/sync_multi_model_mock_cases.py

Deletes existing ``tc-mm-p-*.json`` in cases/ then regenerates (same pattern as
``sync_all_models_cases.py``).
"""

from __future__ import annotations

import json
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
MIGRATION = (
    REPO_ROOT
    / "docs"
    / "sql"
    / "migrations"
    / "20260414_seed_provider_catalog_starter_models.sql"
)
CASES_DIR = Path(__file__).resolve().parents[1] / "cases"
FILE_PREFIX = "tc-mm-p-"

# DB provider row is ``amazon``; unified ModelId uses ``bedrock/...`` prefix.
def unified_chat_model(provider_code: str, model_id: str) -> str:
    if provider_code == "amazon":
        return f"bedrock/{model_id}"
    return f"{provider_code}/{model_id}"


def parse_provider_model_seed(sql_text: str) -> list[tuple[str, str]]:
    """Return (provider_code, model_id) in migration file order."""
    start = sql_text.find("WITH provider_model_seed")
    if start < 0:
        raise SystemExit("provider_model_seed CTE not found in migration SQL")
    sub = sql_text[start:]
    # Stop before INSERT INTO provider_models
    end = sub.find("INSERT INTO provider_models")
    if end > 0:
        sub = sub[:end]
    pat = re.compile(r"\(\s*'([^']+)'\s*,\s*'([^']+)'\s*,")
    rows: list[tuple[str, str]] = []
    for m in pat.finditer(sub):
        rows.append((m.group(1), m.group(2)))
    if not rows:
        raise SystemExit("no (provider_code, model_id) rows parsed from migration")
    return rows


def build_case(seq: int, provider_code: str, model_id: str) -> dict:
    model = unified_chat_model(provider_code, model_id)
    vk = f"legend_{provider_code}"
    prompt = f"mm-p-{provider_code}"[:80]
    # Mock OpenAI-compat echoes: ``Mock response from {provider}/{model}.``
    # Anthropic upstream still uses ``anthropic`` in the mock body string.
    # Bedrock mock: ``Mock response from amazon/{model_id}.``
    if provider_code == "amazon":
        body_needle = "Mock response from amazon"
    elif provider_code == "anthropic":
        body_needle = "Mock response from anthropic"
    else:
        body_needle = f"Mock response from {provider_code}/"

    body_json = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 24,
        },
        separators=(",", ":"),
    )
    curl = (
        "curl -X POST \"$GATEWAY_BASE/v1/chat/completions\" "
        f"-H \"Authorization: Bearer {vk}\" "
        "-H \"Content-Type: application/json\" "
        f"-d '{body_json}'"
    )

    return {
        "caseId": f"MM-P-{seq:03d}",
        "tier": "L2",
        "intent": (
            f"multi_model_service per-provider smoke: VK {vk}, unified model "
            f"{model} (from provider_model_seed)"
        ),
        "profile": ["gate", "full"],
        "requiredEnv": ["MULTI_MODEL_MOCK_E2E_ENABLE"],
        "curl": curl,
        "assertions": {
            "httpStatus": 200,
            "json": {"pathExists": ["/choices/0/message/content"]},
            "body": {"contains": [body_needle]},
        },
    }


def main() -> None:
    sql_text = MIGRATION.read_text(encoding="utf-8")
    rows = parse_provider_model_seed(sql_text)
    CASES_DIR.mkdir(parents=True, exist_ok=True)

    for path in CASES_DIR.glob(f"{FILE_PREFIX}*.json"):
        path.unlink()

    for idx, (code, model_id) in enumerate(rows, start=1):
        case = build_case(idx, code, model_id)
        slug = re.sub(r"[^a-z0-9-]+", "-", code.lower()).strip("-") or "provider"
        out = CASES_DIR / f"{FILE_PREFIX}{idx:03d}-{slug}.json"
        out.write_text(
            json.dumps(case, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )

    print(f"generated {len(rows)} cases from {MIGRATION} into {CASES_DIR}")


if __name__ == "__main__":
    main()
