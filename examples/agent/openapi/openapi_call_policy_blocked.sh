#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/openapi/common.sh
source "${SCRIPT_DIR}/common.sh"

TOOL_ID="${TOOL_ID:-support.review_refund}"
STEP_ID="${STEP_ID:-step_openapi_refund_review}"
TOOL_CALL_ID="${TOOL_CALL_ID:-call_openapi_$(random_suffix)}"
EXPECT_POLICY_BLOCKED="${EXPECT_POLICY_BLOCKED:-true}"

response="$(curl -sS "${AI_GATEWAY_BASE_URL%/}/v1/agent/tools/call" \
  -H "Authorization: Bearer ${API_KEY}" \
  -H "Content-Type: application/json" \
  -H "Alephant-Agent-Id: ${ALEPHANT_AGENT_ID}" \
  -H "Alephant-Agent-Name: ${ALEPHANT_AGENT_NAME}" \
  -H "Alephant-Run-Id: ${ALEPHANT_RUN_ID}" \
  -H "Alephant-Step-Id: ${STEP_ID}" \
  -H "Alephant-Tool-Call-Id: ${TOOL_CALL_ID}" \
  -H "alephant-debug-body: ${ALEPHANT_DEBUG_BODY:-true}" \
  -d "{
    \"source\":\"curl-openapi\",
    \"agent_id\":\"${ALEPHANT_AGENT_ID}\",
    \"agent_name\":\"${ALEPHANT_AGENT_NAME}\",
    \"run_id\":\"${ALEPHANT_RUN_ID}\",
    \"step_id\":\"${STEP_ID}\",
    \"tool_call_id\":\"${TOOL_CALL_ID}\",
    \"tool_id\":\"${TOOL_ID}\",
    \"arguments\":{\"ticket_id\":\"T-1001\",\"amount_cents\":75000,\"reason\":\"large refund demo\"},
    \"idempotency_key\":\"${ALEPHANT_RUN_ID}:${STEP_ID}:${TOOL_CALL_ID}\"
  }")"
printf '%s\n' "${response}"

if [[ "${EXPECT_POLICY_BLOCKED}" == "true" ]]; then
  RESPONSE_JSON="${response}" python3 - <<'PY'
import json
import os
import sys

payload = json.loads(os.environ["RESPONSE_JSON"])
status = payload.get("status")
error_code = (payload.get("error") or {}).get("code")
if status != "blocked" or error_code != "openapi_policy_blocked":
    print(
        "expected policy blocked envelope; set EXPECT_POLICY_BLOCKED=false "
        "if your policy service intentionally allows this demo input",
        file=sys.stderr,
    )
    sys.exit(2)
PY
fi
