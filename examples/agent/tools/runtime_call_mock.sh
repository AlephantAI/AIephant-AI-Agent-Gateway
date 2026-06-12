#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
ENV_FILE="${ENV_FILE:-${REPO_ROOT}/.env}"

if [[ -f "${ENV_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a
fi

random_suffix() {
  if command -v uuidgen >/dev/null 2>&1; then
    uuidgen | tr '[:upper:]' '[:lower:]' | tr -d '-' | cut -c1-12
  else
    date +%s%N | sha256sum | cut -c1-12
  fi
}

print_json() {
  if command -v jq >/dev/null 2>&1; then
    jq .
  else
    cat
  fi
}

GATEWAY_URL="${GATEWAY_URL:-${ALEPHANT_GATEWAY_URL:-${AI_GATEWAY_BASE_URL:-http://127.0.0.1:3000}}}"
API_KEY="${ALEPHANT_API_KEY:-${API_KEY:-${AI_GATEWAY_API_KEY:-}}}"
AGENT_ID="${AGENT_ID:-agent-tools-$(random_suffix)}"
AGENT_NAME="${AGENT_NAME:-Agent Tools Runtime Example}"
RUN_ID="${RUN_ID:-run_tools_$(random_suffix)}"
STEP_ID="${STEP_ID:-step_tool_1}"
TOOL_ID="${TOOL_ID:-support.echo}"
TOOL_CALL_ID="${TOOL_CALL_ID:-call_$(random_suffix)}"

if [[ -z "${API_KEY}" ]]; then
  echo "error: set ALEPHANT_API_KEY, API_KEY, or AI_GATEWAY_API_KEY" >&2
  exit 1
fi

curl -sS "${GATEWAY_URL}/v1/agent/tools/call" \
  -H "Authorization: Bearer ${API_KEY}" \
  -H "Content-Type: application/json" \
  -H "Alephant-Agent-Id: ${AGENT_ID}" \
  -H "Alephant-Agent-Name: ${AGENT_NAME}" \
  -H "Alephant-Run-Id: ${RUN_ID}" \
  -H "Alephant-Step-Id: ${STEP_ID}" \
  -H "Alephant-Tool-Call-Id: ${TOOL_CALL_ID}" \
  -d "{
    \"source\":\"curl\",
    \"agent_id\":\"${AGENT_ID}\",
    \"agent_name\":\"${AGENT_NAME}\",
    \"run_id\":\"${RUN_ID}\",
    \"step_id\":\"${STEP_ID}\",
    \"tool_call_id\":\"${TOOL_CALL_ID}\",
    \"tool_execution_id\":\"client_supplied_should_be_ignored\",
    \"tool_id\":\"${TOOL_ID}\",
    \"snapshot_revision\":0,
    \"arguments\":{\"message\":\"hello from agent tools runtime\"},
    \"idempotency_key\":\"${RUN_ID}:${STEP_ID}:${TOOL_CALL_ID}\"
  }" | print_json
