#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

curl -sS "${GATEWAY_URL}/v1/responses" \
  "${curl_common_headers[@]}" \
  -H "Alephant-Step-Id: step_responses_tool_001" \
  -d '{
    "model": "'"${MODEL}"'",
    "input": "Use the lookup_ticket tool for ticket T-1001. Do not answer directly.",
    "tools": [
      {
        "type": "function",
        "name": "lookup_ticket",
        "description": "Lookup a support ticket",
        "parameters": {
          "type": "object",
          "properties": {
            "ticket_id": { "type": "string" }
          },
          "required": ["ticket_id"]
        }
      }
    ],
    "tool_choice": {
      "type": "function",
      "name": "lookup_ticket"
    }
  }'

