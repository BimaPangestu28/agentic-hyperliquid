#!/usr/bin/env bash
# Quick read-only check of how much collateral the bot will see for sizing.
# Queries the public Hyperliquid info API for the account in .env and prints
# the perp accountValue, the spot USDC total, and which one the bot uses
# (depending on HYPERLIQUID_UNIFIED_ACCOUNT). No private key is read or sent.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${1:-$SCRIPT_DIR/../.env}"

if [ ! -f "$ENV_FILE" ]; then
  echo "error: env file not found at $ENV_FILE" >&2
  exit 1
fi

read_var() { grep -E "^$1=" "$ENV_FILE" | head -1 | cut -d= -f2- | tr -d ' \t\r'; }

ADDRESS="$(read_var HYPERLIQUID_ACCOUNT_ADDRESS)"
NETWORK="$(read_var HYPERLIQUID_NETWORK)"
UNIFIED="$(read_var HYPERLIQUID_UNIFIED_ACCOUNT)"

if [ -z "$ADDRESS" ]; then
  echo "error: HYPERLIQUID_ACCOUNT_ADDRESS is empty in $ENV_FILE" >&2
  exit 1
fi

if [ "$NETWORK" = "mainnet" ]; then
  BASE="https://api.hyperliquid.xyz"
else
  BASE="https://api.hyperliquid-testnet.xyz"
fi

echo "Network: ${NETWORK:-testnet}   Account: $ADDRESS   Unified: ${UNIFIED:-false}"
echo "API: $BASE"
echo

PERP_JSON="$(curl -s -X POST "$BASE/info" -H 'Content-Type: application/json' \
  -d "{\"type\":\"clearinghouseState\",\"user\":\"$ADDRESS\"}")"
SPOT_JSON="$(curl -s -X POST "$BASE/info" -H 'Content-Type: application/json' \
  -d "{\"type\":\"spotClearinghouseState\",\"user\":\"$ADDRESS\"}")"

PERP_JSON="$PERP_JSON" SPOT_JSON="$SPOT_JSON" UNIFIED="$UNIFIED" python3 - <<'PY'
import json, os

perp = json.loads(os.environ["PERP_JSON"] or "{}")
spot = json.loads(os.environ["SPOT_JSON"] or "{}")
unified = os.environ.get("UNIFIED", "").lower() in ("1", "true", "yes")

perp_value = float(perp.get("marginSummary", {}).get("accountValue", 0) or 0)
usdc = 0.0
for bal in spot.get("balances", []):
    if bal.get("coin", "").upper() == "USDC":
        usdc = float(bal.get("total", 0) or 0)
        break

print(f"Perp accountValue : ${perp_value:,.2f}")
print(f"Spot USDC total   : ${usdc:,.2f}")
used = usdc if unified else perp_value
source = "spot USDC (unified mode)" if unified else "perp accountValue"
print(f"\nBot will size against: ${used:,.2f}  (from {source})")
if used <= 0:
    print("WARNING: equity is 0 — trades will be rejected (BelowMinSize). Fund/transfer collateral first.")
PY
