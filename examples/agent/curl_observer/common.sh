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

GATEWAY_URL="${GATEWAY_URL:-${ALEPHANT_GATEWAY_URL:-http://127.0.0.1:8080}}"
MODEL="${MODEL:-gpt-4.1-mini}"
RUN_ID="${RUN_ID:-run_curl_observer_001}"
AGENT_ID="${AGENT_ID:-curl-observer-agent-$(random_suffix)}"
AGENT_NAME="${AGENT_NAME:-Curl Observer Agent}"
DEBUG_BODY="${DEBUG_BODY:-true}"
API_KEY="${API_KEY:-${ALEPHANT_API_KEY:-${ALEPHANT_CONTROL_OPENROUTER_API_KEY:-${OPENAI_API_KEY:-}}}}"

if [[ -z "${API_KEY:-}" ]]; then
  echo "error: set API_KEY, ALEPHANT_API_KEY, ALEPHANT_CONTROL_OPENROUTER_API_KEY, or OPENAI_API_KEY first" >&2
  exit 1
fi

curl_common_headers=(
  -H "Authorization: Bearer ${API_KEY}"
  -H "Content-Type: application/json"
  -H "Alephant-Agent-Id: ${AGENT_ID}"
  -H "Alephant-Agent-Name: ${AGENT_NAME}"
  -H "Alephant-Run-Id: ${RUN_ID}"
  -H "Alephant-Step-Kind: llm_call"
)

if [[ "${DEBUG_BODY}" == "true" ]]; then
  curl_common_headers+=(-H "alephant-debug-body: true")
fi
