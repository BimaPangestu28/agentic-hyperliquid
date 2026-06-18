# CI/CD Deploy to k3s — Design Spec

**Date:** 2026-06-18
**Status:** Approved for planning
**Project:** agentic-hyperliquid

## Overview

Add CI/CD that builds the bot into a Docker image, pushes it to GHCR, and
deploys it to the existing k3s cluster on the production VPS (host configured via
the `DEPLOY_SSH_HOST` GitHub secret — not stored in the repo),
mirroring the portfolio-tracker pattern (GitHub Actions → GHCR → SSH
`kubectl set image`). The bot is an **outbound-only daemon** (Telegram
long-poll + Hyperliquid/DeepSeek/OpenAI calls), so unlike portfolio-tracker it
needs **no Service, Ingress, domain, or TLS** — just a singleton Deployment, a
Secret for its `.env`, and a PVC for `trades.db`.

## Goals

- One Docker image built on `linux/amd64`, pushed to
  `ghcr.io/bimapangestu28/agentic-hyperliquid` tagged `latest` + `:<sha>`.
- Push to `main` auto-builds and rolls out the new `:<sha>` to k3s.
- Run as a **singleton** (replicas 1, `Recreate`) — two instances would conflict
  on Telegram `getUpdates` and double-execute trades.
- Persist `trades.db` across restarts/redeploys (risk-cap daily sum + `/stats`
  attribution depend on it).
- Keep all secrets out of git and out of the image; inject via a k8s Secret.
- Pin to the immutable `:<sha>` tag so rollback is a one-line `set image`.

## Non-Goals (YAGNI)

- No Service / Ingress / domain / TLS (the bot serves no inbound traffic).
- No multi-replica / HPA (must be a singleton).
- No docker-compose path (k3s only, per decision).
- No analytics integration (separate sub-project, brainstormed next).
- No automated first-time cluster bootstrap (namespace/secret/PVC are applied
  manually once; CI only does subsequent rollouts).

## Decisions (from brainstorming)

| Topic | Decision |
|---|---|
| Target | k3s on the existing production VPS (host in `DEPLOY_SSH_HOST` secret), mirror portfolio-tracker |
| Pipeline | GitHub Actions → build amd64 → GHCR (`latest` + `:sha`) → SSH `kubectl set image` |
| Topology | Singleton Deployment, `replicas: 1`, `strategy: Recreate`, no Service/Ingress |
| Secrets | k8s Secret `agentic-hyperliquid-env` from `.env` (manual, off-git) |
| State | PVC `data` (1Gi) mounted at `/data`; `trades.db` lives there (`WORKDIR /data`) |
| Image auth | Private GHCR → `imagePullSecret ghcr-pull` (or make the package public) |
| Rollback | `kubectl -n agentic-hyperliquid set image deploy/bot bot=...:<old-sha>` |

## Components

### Dockerfile (multi-stage)
- **Builder:** `rust:1-slim` (or matching toolchain) → `cargo build --release`.
- **Runtime:** `debian:bookworm-slim` + `ca-certificates` (reqwest uses rustls;
  rusqlite uses the `bundled` SQLite, so no system libsqlite needed). Copy the
  release binary to `/usr/local/bin/agentic-hyperliquid`. `WORKDIR /data` so the
  relative `trades.db` is written under the mounted PVC. `ENTRYPOINT
  ["agentic-hyperliquid"]`.
- `.dockerignore`: `target/`, `.git/`, `.env`, `docs/`.

### GitHub Actions — `.github/workflows/deploy.yml`
- Trigger: `push` to `main` on paths `src/**`, `Cargo.toml`, `Cargo.lock`,
  `Dockerfile`, `.github/workflows/deploy.yml`; plus `workflow_dispatch`.
- Job `build` (`permissions: contents:read, packages:write`):
  checkout → buildx → `docker/login-action` to ghcr.io with
  `${{ github.actor }}` / `${{ secrets.GITHUB_TOKEN }}` → `docker/build-push-action@v6`
  `platforms: linux/amd64`, `push: true`, tags
  `ghcr.io/bimapangestu28/agentic-hyperliquid:latest` and `:${{ github.sha }}`,
  gha cache.
- Job `deploy` (`needs: build`, `concurrency: group deploy-production`,
  `cancel-in-progress: false`): SSH with `secrets.DEPLOY_SSH_KEY/USER/HOST`
  (StrictHostKeyChecking=accept-new) → `kubectl -n agentic-hyperliquid set image
  deploy/bot bot=ghcr.io/bimapangestu28/agentic-hyperliquid:${GITHUB_SHA}` →
  `kubectl -n agentic-hyperliquid rollout status deploy/bot --timeout=180s`.

### k8s manifests — `k8s/`
- `namespace.yaml`: namespace `agentic-hyperliquid`.
- `pvc.yaml`: PVC `data`, `ReadWriteOnce`, 1Gi (k3s default `local-path`).
- `deployment.yaml`: Deployment `bot`, `replicas: 1`, `strategy.type: Recreate`,
  container `bot` image `ghcr.io/bimapangestu28/agentic-hyperliquid:latest`
  (CI repins to `:sha`), `envFrom: [{ secretRef: { name: agentic-hyperliquid-env }}]`,
  `volumeMounts: [{ name: data, mountPath: /data }]`, `imagePullSecrets:
  [{ name: ghcr-pull }]`, modest resource requests/limits, `RUST_LOG` env set to
  `info,agentic_hyperliquid=debug`.
- `k8s/README.md`: the one-time setup runbook (below).

## Data flow (deploy)

```
push to main → Actions build job → image to GHCR (:latest + :sha)
            → Actions deploy job → SSH VPS → kubectl set image deploy/bot :sha
            → kubectl rollout status (Recreate: old pod terminates, new starts)
bot pod: envFrom Secret → connects to Telegram (long-poll) etc.; writes /data/trades.db (PVC)
```

## One-time setup (manual; documented in `k8s/README.md`)

1. **GitHub repo secrets** (Settings → Secrets → Actions): `DEPLOY_SSH_KEY`,
   `DEPLOY_SSH_USER`, `DEPLOY_SSH_HOST` — same values as portfolio-tracker (same VPS).
2. **On the server** (kubectl context for the cluster):
   - `kubectl apply -f k8s/namespace.yaml`
   - `kubectl -n agentic-hyperliquid create secret generic agentic-hyperliquid-env --from-env-file=.env`
     (the production `.env`; start with `HYPERLIQUID_NETWORK=testnet`).
   - Image pull: either make the GHCR package public, or
     `kubectl -n agentic-hyperliquid create secret docker-registry ghcr-pull --docker-server=ghcr.io --docker-username=BimaPangestu28 --docker-password=<PAT read:packages>`.
   - `kubectl apply -f k8s/pvc.yaml -f k8s/deployment.yaml`
3. Subsequent pushes to `main` roll out automatically.

## Error handling / operational notes

- **Singleton invariant:** `replicas: 1` + `Recreate` guarantees only one bot
  polls Telegram at a time (RollingUpdate would briefly run two → getUpdates
  conflict + double execution).
- **Rollback:** re-pin a prior `:sha` (immutable tags) and `rollout status`.
- **Secrets never in git/image:** image carries only the compiled binary; all
  config is the k8s Secret. `.dockerignore` excludes `.env`.
- **State durability:** PVC keeps `trades.db`; a redeploy reuses it. (k3s
  `local-path` PVC is node-local — fine for a single-node VPS.)
- **Secret rotation:** update the k8s Secret + `kubectl rollout restart
  deploy/bot` (documented).
- A failed rollout (`rollout status` timeout) fails the Actions job, leaving the
  previous ReplicaSet running.

## Testing / verification

- Build the image locally (`docker build .`) and run it with a test `.env` to
  confirm it starts and connects (manual).
- After first deploy: `kubectl -n agentic-hyperliquid rollout status deploy/bot`
  succeeds; `kubectl -n agentic-hyperliquid logs deploy/bot` shows the bot
  connecting to Telegram.
- Trigger the workflow (push or `workflow_dispatch`); confirm the image appears
  in GHCR with both tags and the deploy job repins + rolls out.
- No Rust unit tests are added (infra-only change); existing `cargo test` must
  still pass and the build must stay warning-free.

## Files added/changed

```
Dockerfile
.dockerignore
.github/workflows/deploy.yml
k8s/namespace.yaml
k8s/pvc.yaml
k8s/deployment.yaml
k8s/README.md
README.md            (add a "Deploy (k3s)" section pointing to k8s/README.md)
```
