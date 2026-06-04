#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/common.sh"

curl -N "${GATEWAY_URL}/v1/responses" \
  "${curl_common_headers[@]}" \
  -H "Alephant-Step-Id: step_responses_text_stream_001" \
  -d '{
    "model": "'"${MODEL}"'",
    "stream": true,
    "input": "Reply with one short sentence: agent gateway observer test."
  }'

