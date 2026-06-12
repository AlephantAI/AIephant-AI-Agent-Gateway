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

AI_GATEWAY_BASE_URL="${AI_GATEWAY_BASE_URL:-${GATEWAY_URL:-${ALEPHANT_GATEWAY_URL:-http://127.0.0.1:3000}}}"
API_KEY="${API_KEY:-${ALEPHANT_API_KEY:-${ALEPHANT_CONTROL_OPENROUTER_API_KEY:-${OPENAI_API_KEY:-}}}}"
ALEPHANT_AGENT_ID="${ALEPHANT_AGENT_ID:-openapi-demo-agent}"
ALEPHANT_AGENT_NAME="${ALEPHANT_AGENT_NAME:-OpenAPI Demo Agent}"
ALEPHANT_RUN_ID="${ALEPHANT_RUN_ID:-run_openapi_$(random_suffix)}"

if [[ -z "${API_KEY:-}" ]]; then
  echo "error: set API_KEY, ALEPHANT_API_KEY, ALEPHANT_CONTROL_OPENROUTER_API_KEY, or OPENAI_API_KEY first" >&2
  exit 1
fi
