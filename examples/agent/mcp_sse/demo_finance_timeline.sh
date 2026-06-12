#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export RUN_ID="${RUN_ID:-run_mcp_sse_demo_$(date +%s)}"
export ALEPHANT_RUN_ID="${RUN_ID}"

bash "${SCRIPT_DIR}/list_tools.sh"
bash "${SCRIPT_DIR}/call_success.sh"
bash "${SCRIPT_DIR}/call_is_error.sh"
bash "${SCRIPT_DIR}/call_timeout.sh"
bash "${SCRIPT_DIR}/call_policy_blocked.sh"
bash "${SCRIPT_DIR}/call_egress_blocked.sh"

echo "Demo run id: ${RUN_ID}"
echo "Expected downstream timeline: requested + terminal events for success, business error, timeout, policy blocked, and egress blocked."
