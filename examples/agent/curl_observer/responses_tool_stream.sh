#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

curl -N "${GATEWAY_URL}/v1/responses" \
  "${curl_common_headers[@]}" \
  -H "Alephant-Step-Id: step_responses_tool_stream_001" \
  -d '{
    "model": "'"${MODEL}"'",
    "stream": true,
    "input": "Use the lookup_customer tool for customer C-42. Do not answer directly.",
    "tools": [
      {
        "type": "function",
        "name": "lookup_customer",
        "description": "Lookup a customer",
        "parameters": {
          "type": "object",
          "properties": {
            "customer_id": { "type": "string" }
          },
          "required": ["customer_id"]
        }
      }
    ],
    "tool_choice": {
      "type": "function",
      "name": "lookup_customer"
    }
  }'

