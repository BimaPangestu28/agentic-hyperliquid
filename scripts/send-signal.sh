#!/usr/bin/env bash
# Pipe the macOS clipboard (or stdin) to the bot's local /ingest endpoint.
# Reads INGEST_PORT/INGEST_TOKEN from .env. Bind a hotkey to this script.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${ENV_FILE:-$SCRIPT_DIR/../.env}"

read_var() { grep -E "^$1=" "$ENV_FILE" | head -1 | cut -d= -f2- | tr -d ' \t\r'; }

PORT="$(read_var INGEST_PORT)"
TOKEN="$(read_var INGEST_TOKEN)"

: "${PORT:?INGEST_PORT not set in .env}"
: "${TOKEN:?INGEST_TOKEN not set in .env}"

# Use clipboard if no stdin piped.
if [ -t 0 ]; then
    BODY="$(pbpaste)"
else
    BODY="$(cat)"
fi

code=$(curl -s -o /tmp/ingest-resp -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/ingest" \
    -H "X-Ingest-Token: $TOKEN" \
    --data-binary "$BODY")

echo "ingest -> HTTP $code: $(cat /tmp/ingest-resp)"
