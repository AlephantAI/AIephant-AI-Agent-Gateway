#!/usr/bin/env python3
"""Sync model smoke cases from all_models_curl.sh into gateway-e2e-harness.

Run from repo root:
  python3 crates/gateway-e2e-harness/scripts/sync_all_models_cases.py
"""

from __future__ import annotations

import json
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SOURCE_SCRIPT = REPO_ROOT / "all_models_curl.sh"
CASES_DIR = Path(__file__).resolve().parents[1] / "cases"
FILE_PREFIX = "tc-or-"

MODEL_RE = re.compile(r'"model":"([^"]+)"')
SAFE_RE = re.compile(r"[^a-z0-9]+")


def slugify_model(model: str) -> str:
    lowered = model.lower()
    slug = SAFE_RE.sub("-", lowered).strip("-")
    return slug or "model"


def parse_models(script_text: str) -> list[str]:
    models: list[str] = []
    seen: set[str] = set()
    for line in script_text.splitlines():
        line = line.strip()
        if not line.startswith("curl "):
            continue
        m = MODEL_RE.search(line)
        if m is None:
            continue
        model = m.group(1)
        if model in seen:
            continue
        seen.add(model)
        models.append(model)
    return models


def build_case(case_index: int, model: str) -> dict:
    return {
        "caseId": f"OR-{case_index:03d}",
        "tier": "L2",
        "intent": f"OpenRouter model smoke synced from all_models_curl.sh: {model}",
        "profile": ["full"],
        "requiredEnv": [
            "ALEPHANT_CONTROL_PLANE_API_KEY",
            "ALL_MODELS_E2E_ENABLE",
        ],
        "curl": (
            "curl -X POST \"$GATEWAY_BASE/v1/chat/completions\" "
            "-H \"Authorization: Bearer $ALEPHANT_CONTROL_PLANE_API_KEY\" "
            "-H \"Content-Type: application/json\" "
            f"-d '{{\"model\":\"{model}\",\"messages\":[{{\"role\":\"user\",\"content\":\"Hello\"}}]}}'"
        ),
        "assertions": {"httpStatus": 200},
    }


def main() -> None:
    script_text = SOURCE_SCRIPT.read_text(encoding="utf-8")
    models = parse_models(script_text)
    if not models:
        raise SystemExit("no model entries parsed from all_models_curl.sh")

    CASES_DIR.mkdir(parents=True, exist_ok=True)

    for path in CASES_DIR.glob(f"{FILE_PREFIX}*.json"):
        path.unlink()

    for idx, model in enumerate(models, start=1):
        slug = slugify_model(model)
        case = build_case(idx, model)
        out = CASES_DIR / f"{FILE_PREFIX}{idx:03d}-{slug}.json"
        out.write_text(
            json.dumps(case, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )

    print(f"generated {len(models)} cases into {CASES_DIR}")


if __name__ == "__main__":
    main()
