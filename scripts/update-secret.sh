#!/usr/bin/env bash
# Update the agentic-hyperliquid env Secret from .env and restart the bot so it
# picks up the new values. Use this after rotating keys / editing .env.
#
# Run ON A HOST WHOSE kubectl POINTS AT THE TARGET CLUSTER, FROM THE REPO ROOT,
# with the current ./.env present (scp it here; never commit it).
#
#   ./scripts/update-secret.sh
set -euo pipefail

NS=agentic-hyperliquid
ENV_FILE="${ENV_FILE:-.env}"

command -v kubectl >/dev/null || { echo "error: kubectl not found" >&2; exit 1; }
[ -f "$ENV_FILE" ] || { echo "error: $ENV_FILE not found" >&2; exit 1; }

echo ">> updating secret agentic-hyperliquid-env (from $ENV_FILE)"
kubectl -n "$NS" create secret generic agentic-hyperliquid-env \
  --from-env-file="$ENV_FILE" --dry-run=client -o yaml | kubectl apply -f -

echo ">> restarting bot to pick up the new secret"
kubectl -n "$NS" rollout restart deploy/bot
kubectl -n "$NS" rollout status deploy/bot --timeout=300s
echo ">> done."
