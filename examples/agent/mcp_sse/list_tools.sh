#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=examples/agent/mcp_sse/common.sh
source "${SCRIPT_DIR}/common.sh"

curl -sS "${GATEWAY_URL}/v1/agent/tools/list" \
  "${agent_tool_headers[@]}" \
  -d '{
    "agent_id": "'"${AGENT_ID}"'",
    "adapter": "langgraph"
  }' | print_json
