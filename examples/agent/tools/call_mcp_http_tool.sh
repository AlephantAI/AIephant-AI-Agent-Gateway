#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/tools/common.sh
source "${SCRIPT_DIR}/common.sh"

# Demo target config (gateway side):
# tool-id: docs.search
# name: docs.search
# kind: mcp-http
# url: http://127.0.0.1:9876/mcp
# method: POST
# local egress policy for this demo must allow loopback/http:
#   agent.tools.egress-policy.https-only=false
#   agent.tools.egress-policy.block-loopback=false

print_json() {
  if command -v jq >/dev/null 2>&1; then
    jq .
  else
    cat
  fi
}

build_payload() {
  if command -v jq >/dev/null 2>&1; then
    jq -nc \
      --arg source "curl" \
      --arg agent_id "${AGENT_ID}" \
      --arg agent_name "${AGENT_NAME}" \
      --arg run_id "${RUN_ID}" \
      --arg step_id "${STEP_ID}" \
      --arg tool_call_id "${TOOL_CALL_ID}" \
      --arg tool_id "${TOOL_ID}" \
      --arg query "${QUERY}" \
      --arg idempotency_key "${RUN_ID}:${STEP_ID}:${TOOL_CALL_ID}" \
      '{source:$source,agent_id:$agent_id,agent_name:$agent_name,run_id:$run_id,step_id:$step_id,tool_call_id:$tool_call_id,tool_id:$tool_id,snapshot_revision:0,arguments:{query:$query},idempotency_key:$idempotency_key}'
    return
  fi

  python3 - <<PY
import json
import os

payload = {
    "source": "curl",
    "agent_id": os.environ["AGENT_ID"],
    "agent_name": os.environ["AGENT_NAME"],
    "run_id": os.environ["RUN_ID"],
    "step_id": os.environ["STEP_ID"],
    "tool_call_id": os.environ["TOOL_CALL_ID"],
    "tool_id": os.environ["TOOL_ID"],
    "snapshot_revision": 0,
    "arguments": {"query": os.environ["QUERY"]},
    "idempotency_key": f"{os.environ['RUN_ID']}:{os.environ['STEP_ID']}:{os.environ['TOOL_CALL_ID']}",
}
print(json.dumps(payload))
PY
}

TOOL_ID="${TOOL_ID:-docs.search}"
STEP_ID="${STEP_ID:-step_mcp_1}"
TOOL_CALL_ID="${TOOL_CALL_ID:-call_$(random_suffix)}"
QUERY="${QUERY:-refund policy}"

export AGENT_ID AGENT_NAME RUN_ID STEP_ID TOOL_CALL_ID TOOL_ID QUERY

curl -sS "${GATEWAY_URL}/v1/agent/tools/call" \
  "${agent_tool_headers[@]}" \
  -H "Alephant-Step-Id: ${STEP_ID}" \
  -H "Alephant-Tool-Call-Id: ${TOOL_CALL_ID}" \
  -d "$(build_payload)" | print_json
