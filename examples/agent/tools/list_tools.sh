#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/tools/common.sh
source "${SCRIPT_DIR}/common.sh"

curl -sS "${GATEWAY_URL}/v1/agent/tools/list" \
  "${agent_tool_headers[@]}" \
  -d "{
    \"source\":\"curl\",
    \"agent_id\":\"${AGENT_ID}\",
    \"agent_name\":\"${AGENT_NAME}\",
    \"run_id\":\"${RUN_ID}\",
    \"capabilities\":{\"schema_dialect\":\"openai_function\"}
  }"
echo
