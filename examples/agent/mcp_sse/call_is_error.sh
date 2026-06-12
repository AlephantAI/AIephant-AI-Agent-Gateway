#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/mcp_sse/common.sh
source "${SCRIPT_DIR}/common.sh"

STEP_ID="${STEP_ID:-step_mcp_sse_is_error}"
TOOL_CALL_ID="${TOOL_CALL_ID:-call_mcp_sse_is_error_$(random_suffix)}"

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
    "arguments": {"query": "business-error"}
  }')"
printf '%s\n' "${body}" | print_json

BODY="${body}" python3 - <<'PY'
import json
import os
import sys

body = json.loads(os.environ["BODY"])
if body.get("status") != "completed" or body.get("output", {}).get("isError") is not True:
    print("expected completed MCP business error with output.isError=true", file=sys.stderr)
    sys.exit(2)
PY
