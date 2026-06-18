#!/usr/bin/env bash
# One-time (idempotent) bootstrap of the agentic-hyperliquid deployment on k3s.
#
# Run ON A HOST WHOSE kubectl POINTS AT THE TARGET CLUSTER (the server, or your
# machine with the cluster kubeconfig), FROM THE REPO ROOT. Requires ./k8s/*.yaml
# and a filled ./.env (the production config — never commit it; scp it here).
#
# Optional: set GHCR_PAT to a GitHub PAT with `read:packages` to create the image
# pull secret. Skip it if the GHCR package is public (then also delete the
# `imagePullSecrets` line from k8s/deployment.yaml).
#
#   ./scripts/bootstrap-k8s.sh
#   GHCR_PAT=ghp_xxx ./scripts/bootstrap-k8s.sh
set -euo pipefail

NS=agentic-hyperliquid
ENV_FILE="${ENV_FILE:-.env}"

command -v kubectl >/dev/null || { echo "error: kubectl not found" >&2; exit 1; }
[ -f "$ENV_FILE" ] || { echo "error: $ENV_FILE not found (the production .env)" >&2; exit 1; }
[ -d k8s ] || { echo "error: run from the repo root (k8s/ not found)" >&2; exit 1; }

echo ">> namespace"
kubectl apply -f k8s/namespace.yaml

echo ">> secret agentic-hyperliquid-env (from $ENV_FILE)"
kubectl -n "$NS" create secret generic agentic-hyperliquid-env \
  --from-env-file="$ENV_FILE" --dry-run=client -o yaml | kubectl apply -f -

if [ -n "${GHCR_PAT:-}" ]; then
  echo ">> image pull secret ghcr-pull"
  kubectl -n "$NS" create secret docker-registry ghcr-pull \
    --docker-server=ghcr.io --docker-username=BimaPangestu28 --docker-password="$GHCR_PAT" \
    --dry-run=client -o yaml | kubectl apply -f -
else
  echo ">> GHCR_PAT not set — skipping pull secret."
  echo "   (Make the GHCR package public, or re-run with GHCR_PAT=<read:packages PAT>.)"
fi

echo ">> pvc + deployment"
kubectl apply -f k8s/pvc.yaml -f k8s/deployment.yaml

echo ">> waiting for rollout"
kubectl -n "$NS" rollout status deploy/bot --timeout=300s
echo ">> done. Logs: kubectl -n $NS logs deploy/bot -f"
