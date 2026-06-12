#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/tools/common.sh
source "${SCRIPT_DIR}/common.sh"

STEP_ID="${STEP_ID:-step_tool_1}"
TOOL_ID="${TOOL_ID:-support.echo}"
TOOL_CALL_ID="${TOOL_CALL_ID:-call_$(random_suffix)}"

curl -sS "${GATEWAY_URL}/v1/agent/tools/call" \
  "${agent_tool_headers[@]}" \
  -H "Alephant-Step-Id: ${STEP_ID}" \
  -H "Alephant-Tool-Call-Id: ${TOOL_CALL_ID}" \
  -d "{
    \"source\":\"curl\",
    \"agent_id\":\"${AGENT_ID}\",
    \"agent_name\":\"${AGENT_NAME}\",
    \"run_id\":\"${RUN_ID}\",
    \"step_id\":\"${STEP_ID}\",
    \"tool_call_id\":\"${TOOL_CALL_ID}\",
    \"tool_id\":\"${TOOL_ID}\",
    \"arguments\":{\"text\":\"hello from agent tools\"},
    \"idempotency_key\":\"${RUN_ID}:${STEP_ID}:${TOOL_CALL_ID}\"
  }"
echo
