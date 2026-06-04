#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

curl -N "${GATEWAY_URL}/v1/chat/completions" \
  "${curl_common_headers[@]}" \
  -H "Alephant-Step-Id: step_chat_tool_stream_001" \
  -d '{
    "model": "'"${MODEL}"'",
    "stream": true,
    "messages": [
      {
        "role": "user",
        "content": "Call the search_docs tool with query agent gateway observer."
      }
    ],
    "tools": [
      {
        "type": "function",
        "function": {
          "name": "search_docs",
          "description": "Search docs",
          "parameters": {
            "type": "object",
            "properties": {
              "query": { "type": "string" }
            },
            "required": ["query"]
          }
        }
      }
    ],
    "tool_choice": {
      "type": "function",
      "function": { "name": "search_docs" }
    }
  }'

