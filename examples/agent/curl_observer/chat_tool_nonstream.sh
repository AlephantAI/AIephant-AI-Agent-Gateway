#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

curl -sS "${GATEWAY_URL}/v1/chat/completions" \
  "${curl_common_headers[@]}" \
  -H "Alephant-Step-Id: step_chat_tool_001" \
  -d '{
    "model": "'"${MODEL}"'",
    "messages": [
      {
        "role": "user",
        "content": "Call the get_weather tool for Hangzhou. Do not answer directly."
      }
    ],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "get_weather",
          "description": "Get weather by city",
          "parameters": {
            "type": "object",
            "properties": {
              "city": { "type": "string" }
            },
            "required": ["city"]
          }
        }
      }
    ],
    "tool_choice": {
      "type": "function",
      "function": { "name": "get_weather" }
    }
  }'

