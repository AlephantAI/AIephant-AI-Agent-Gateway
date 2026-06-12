#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/openapi/common.sh
source "${SCRIPT_DIR}/common.sh"

curl -sS "${AI_GATEWAY_BASE_URL%/}/v1/agent/tools/list" \
  -H "Authorization: Bearer ${API_KEY}" \
  -H "Content-Type: application/json" \
  -H "Alephant-Agent-Id: ${ALEPHANT_AGENT_ID}" \
  -H "Alephant-Agent-Name: ${ALEPHANT_AGENT_NAME}" \
  -H "Alephant-Run-Id: ${ALEPHANT_RUN_ID}" \
  -H "Alephant-Step-Id: step_list_tools" \
  -H "alephant-debug-body: ${ALEPHANT_DEBUG_BODY:-true}" \
  -d "{
    \"source\":\"curl-openapi\",
    \"agent_id\":\"${ALEPHANT_AGENT_ID}\",
    \"agent_name\":\"${ALEPHANT_AGENT_NAME}\",
    \"run_id\":\"${ALEPHANT_RUN_ID}\",
    \"capabilities\":{\"schema_dialect\":\"openai_function\"}
  }"
echo
