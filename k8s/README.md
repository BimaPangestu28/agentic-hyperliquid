# Deploy (k3s)

The bot runs as a singleton Deployment in namespace `agentic-hyperliquid` on the
production k3s cluster. CI (`.github/workflows/deploy.yml`) builds the image to
GHCR and pins the new `:sha` on every push to `main`. First-time cluster setup is
manual (below). The bot is outbound-only — no Service/Ingress/domain.

## One-time setup

### 1. GitHub repo secrets (Settings → Secrets and variables → Actions)
Same values as the portfolio-tracker repo (same VPS):
- `DEPLOY_SSH_KEY` — private key authorized on the VPS
- `DEPLOY_SSH_USER` — ssh user
- `DEPLOY_SSH_HOST` — VPS host/IP (kept only here, never committed)

### 2. On the server (a shell with kubectl pointed at the cluster)
```bash
kubectl apply -f k8s/namespace.yaml

# Secrets: build the production .env (start with HYPERLIQUID_NETWORK=testnet),
# then load it as a k8s Secret. The .env never enters git or the image.
kubectl -n agentic-hyperliquid create secret generic agentic-hyperliquid-env \
  --from-env-file=.env

# Image pull. Option A — private package: create a pull secret with a PAT
# (scope: read:packages):
kubectl -n agentic-hyperliquid create secret docker-registry ghcr-pull \
  --docker-server=ghcr.io \
  --docker-username=BimaPangestu28 \
  --docker-password='<GHCR_PAT_read_packages>'
# Option B — make the GHCR package public (the image holds only the binary, no
# secrets) and delete the `imagePullSecrets` line from k8s/deployment.yaml.

kubectl apply -f k8s/pvc.yaml -f k8s/deployment.yaml
```

### 3. Done
Push to `main` → CI builds, pushes to GHCR, and rolls out the new `:sha`.

## Operations
- **Logs:** `kubectl -n agentic-hyperliquid logs deploy/bot -f`
- **Rollback:** `kubectl -n agentic-hyperliquid set image deploy/bot bot=ghcr.io/bimapangestu28/agentic-hyperliquid:<older-sha> && kubectl -n agentic-hyperliquid rollout status deploy/bot`
- **Rotate secrets:** update the Secret, then `kubectl -n agentic-hyperliquid rollout restart deploy/bot`
  ```bash
  kubectl -n agentic-hyperliquid delete secret agentic-hyperliquid-env
  kubectl -n agentic-hyperliquid create secret generic agentic-hyperliquid-env --from-env-file=.env
  kubectl -n agentic-hyperliquid rollout restart deploy/bot
  ```
- **Go live (mainnet):** edit the `.env`, recreate the Secret, `rollout restart`.

## Notes
- `replicas: 1` + `strategy: Recreate` is mandatory: two bots would conflict on
  Telegram `getUpdates` and double-execute trades.
- `trades.db` persists on the `data` PVC (risk-cap daily sum + `/stats`).
