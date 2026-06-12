#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/mcp_sse/common.sh
source "${SCRIPT_DIR}/common.sh"

STEP_ID="${STEP_ID:-step_mcp_sse_timeout}"
TOOL_CALL_ID="${TOOL_CALL_ID:-call_mcp_sse_timeout_$(random_suffix)}"

body="$(curl -sS "${GATEWAY_URL}/v1/agent/tools/call" \
  "${agent_tool_headers[@]}" \
  -H "Alephant-Step-Id: ${STEP_ID}" \
  -H "Alephant-Tool-Call-Id: ${TOOL_CALL_ID}" \
  -d '{
    "source": "curl",
    "agent_id": "'"${AGENT_ID}"'",
    "agent_name": "'"${AGENT_NAME}"'",
    "run_id": "'"${RUN_ID}"'",
    "step_id": "'"${STEP_ID}"'",
    "tool_call_id": "'"${TOOL_CALL_ID}"'",
    "tool_id": "docs.search",
    "timeout_ms": 500,
    "arguments": {"query": "timeout"}
  }')"
printf '%s\n' "${body}" | print_json

BODY="${body}" python3 - <<'PY'
import json
import os
import sys

body = json.loads(os.environ["BODY"])
if body.get("status") != "timeout" or body.get("error", {}).get("retryable") is not True:
    print("expected timeout with retryable=true", file=sys.stderr)
    sys.exit(2)
PY
