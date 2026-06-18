# CI/CD Deploy to k3s Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the bot into a Docker image, push to GHCR, and roll it out to the existing k3s cluster on push to `main` — as a singleton daemon with a persistent `trades.db` and secrets injected from a k8s Secret.

**Architecture:** Multi-stage Rust Dockerfile → image to `ghcr.io/bimapangestu28/agentic-hyperliquid`. GitHub Actions builds+pushes (`latest` + `:sha`) then SSHes to the VPS and `kubectl set image` the `:sha` into namespace `agentic-hyperliquid`. k8s manifests live in `k8s/`; the bot runs `replicas: 1` / `Recreate` (Telegram singleton), `envFrom` a Secret, PVC at `/data`. No Service/Ingress.

**Tech Stack:** Docker (buildx), GitHub Actions (`docker/build-push-action@v6`, GHCR via `GITHUB_TOKEN`), k3s/kubectl, SSH deploy.

## Global Constraints

- Image repo: `ghcr.io/bimapangestu28/agentic-hyperliquid`; tags `latest` + `:${{ github.sha }}`; build `linux/amd64` only.
- Namespace `agentic-hyperliquid`; Deployment `bot`, container `bot`; **`replicas: 1`, `strategy: Recreate`** (Telegram getUpdates singleton — never run two).
- Secrets/config via k8s Secret `agentic-hyperliquid-env` (`envFrom`); created manually on the server, NEVER in git/image. `.env` excluded by `.dockerignore`.
- State: PVC `data` (1Gi) mounted at `/data`; binary runs with `WORKDIR /data` so `trades.db` persists.
- **No server IP / hostnames in any committed file** — the deploy host comes from the `DEPLOY_SSH_HOST` GitHub secret only.
- Deploy job pins the immutable `:sha` (rollback = re-pin an older sha). Serialize with a `deploy-production` concurrency group.
- Infra-only change: add no Rust code; existing `cargo test` must still pass and `cargo build` stay warning-free.

---

### Task 1: Dockerfile + .dockerignore

**Files:**
- Create: `Dockerfile`
- Create: `.dockerignore`

**Interfaces:**
- Produces: an image whose entrypoint is the `agentic-hyperliquid` binary, runs with `WORKDIR /data`, and trusts public CAs. Consumed by Task 3 (build-push) and Task 2 (image ref).

- [ ] **Step 1: Create `.dockerignore`**
```
target/
.git/
.env
docs/
k8s/
.github/
```

- [ ] **Step 2: Create `Dockerfile`**
```dockerfile
# ---- builder ----
FROM rust:1-bookworm AS builder
WORKDIR /build
# Pre-build dependencies for layer caching: compile a stub against the manifests,
# then drop in the real sources. (Dependencies are determined by Cargo.toml, not
# by our source, so the stub compiles the full dep graph.)
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src
COPY src ./src
# Force a rebuild of our crate now that real sources are present.
RUN touch src/main.rs && cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /data
COPY --from=builder /build/target/release/agentic-hyperliquid /usr/local/bin/agentic-hyperliquid
ENTRYPOINT ["agentic-hyperliquid"]
```

- [ ] **Step 3: Build the image locally to verify it compiles & links**

Run: `docker build -t agentic-hyperliquid:test .`
Expected: completes successfully (the Rust build is heavy — several minutes — let it finish). The final image is the debian-slim runtime with the binary at `/usr/local/bin/agentic-hyperliquid`.
(If Docker is unavailable in the environment, instead run `cargo build --release` to confirm the binary builds, and note that the Docker build must be validated in CI. Do NOT skip silently.)

- [ ] **Step 4: Sanity-check the image entrypoint (no secrets → it should error out fast, proving it runs)**

Run: `docker run --rm agentic-hyperliquid:test || true`
Expected: the binary starts and exits with the config error (e.g. "TELEGRAM_BOT_TOKEN is required") — proving the binary executes in the runtime image. (Skip if Docker unavailable.)

- [ ] **Step 5: Commit**
```bash
git add Dockerfile .dockerignore
git commit -m "build: add multi-stage Dockerfile and .dockerignore"
```

---

### Task 2: k8s manifests

**Files:**
- Create: `k8s/namespace.yaml`
- Create: `k8s/pvc.yaml`
- Create: `k8s/deployment.yaml`

**Interfaces:**
- Consumes: the image `ghcr.io/bimapangestu28/agentic-hyperliquid` (Task 1), the Secret `agentic-hyperliquid-env` and pull secret `ghcr-pull` (created manually per Task 4 runbook).
- Produces: Deployment `bot` in namespace `agentic-hyperliquid` that Task 3's deploy job targets via `kubectl set image deploy/bot bot=...`.

- [ ] **Step 1: Create `k8s/namespace.yaml`**
```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: agentic-hyperliquid
```

- [ ] **Step 2: Create `k8s/pvc.yaml`**
```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: data
  namespace: agentic-hyperliquid
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 1Gi
```

- [ ] **Step 3: Create `k8s/deployment.yaml`**
```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: bot
  namespace: agentic-hyperliquid
  labels:
    app: agentic-hyperliquid
spec:
  replicas: 1
  strategy:
    type: Recreate          # singleton: never run two bots (Telegram getUpdates conflict)
  selector:
    matchLabels:
      app: agentic-hyperliquid
  template:
    metadata:
      labels:
        app: agentic-hyperliquid
    spec:
      imagePullSecrets:
        - name: ghcr-pull     # remove this line if the GHCR package is made public
      containers:
        - name: bot
          image: ghcr.io/bimapangestu28/agentic-hyperliquid:latest   # CI re-pins to :sha
          envFrom:
            - secretRef:
                name: agentic-hyperliquid-env
          env:
            - name: RUST_LOG
              value: "info,agentic_hyperliquid=debug"
          volumeMounts:
            - name: data
              mountPath: /data
          resources:
            requests:
              cpu: "50m"
              memory: "64Mi"
            limits:
              cpu: "500m"
              memory: "256Mi"
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: data
```

- [ ] **Step 4: Validate the manifests**

Run (if kubectl is available): `kubectl apply --dry-run=client -f k8s/namespace.yaml -f k8s/pvc.yaml -f k8s/deployment.yaml`
Expected: each prints `... (dry run)` with no errors.
If kubectl is NOT available, validate YAML parses:
`python3 -c "import yaml,glob,sys; [list(yaml.safe_load_all(open(f))) for f in glob.glob('k8s/*.yaml')]; print('yaml ok')"`
Expected: `yaml ok`.

- [ ] **Step 5: Commit**
```bash
git add k8s/namespace.yaml k8s/pvc.yaml k8s/deployment.yaml
git commit -m "deploy: add k8s namespace, pvc, and singleton deployment"
```

---

### Task 3: GitHub Actions workflow

**Files:**
- Create: `.github/workflows/deploy.yml`

**Interfaces:**
- Consumes: `Dockerfile` (Task 1), Deployment `bot` (Task 2), GitHub secrets `DEPLOY_SSH_KEY`/`DEPLOY_SSH_USER`/`DEPLOY_SSH_HOST` (set manually per Task 4).
- Produces: on push to `main`, the GHCR image (`latest` + `:sha`) and a rolled-out deployment pinned to `:sha`.

- [ ] **Step 1: Create `.github/workflows/deploy.yml`**
```yaml
name: Build and deploy

# Build the bot image on amd64, push to GHCR (built-in GITHUB_TOKEN has
# packages:write), then SSH to the VPS and pin the deployment to the new :sha.
on:
  push:
    branches: [main]
    paths:
      - "src/**"
      - "Cargo.toml"
      - "Cargo.lock"
      - "Dockerfile"
      - ".github/workflows/deploy.yml"
  workflow_dispatch:

jobs:
  build:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v6
        with:
          context: .
          platforms: linux/amd64
          push: true
          # GHCR paths must be lowercase.
          tags: |
            ghcr.io/bimapangestu28/agentic-hyperliquid:latest
            ghcr.io/bimapangestu28/agentic-hyperliquid:${{ github.sha }}
          cache-from: type=gha
          cache-to: type=gha,mode=max

  deploy:
    name: Deploy to k3s
    runs-on: ubuntu-latest
    needs: build
    # Serialize deploys so two quick merges can't interleave rollouts.
    concurrency:
      group: deploy-production
      cancel-in-progress: false
    steps:
      - name: Pin deployment to the new image and wait for rollout
        env:
          SSH_KEY: ${{ secrets.DEPLOY_SSH_KEY }}
          SSH_USER: ${{ secrets.DEPLOY_SSH_USER }}
          SSH_HOST: ${{ secrets.DEPLOY_SSH_HOST }}
        run: |
          install -m 700 -d ~/.ssh
          printf '%s\n' "$SSH_KEY" > ~/.ssh/deploy_key
          chmod 600 ~/.ssh/deploy_key
          # Pin the immutable :sha (not :latest) so what runs is exactly what this
          # workflow built; rollback is a one-line `set image` to an older sha.
          ssh -i ~/.ssh/deploy_key -o StrictHostKeyChecking=accept-new "$SSH_USER@$SSH_HOST" "
            set -euo pipefail
            kubectl -n agentic-hyperliquid set image deploy/bot bot=ghcr.io/bimapangestu28/agentic-hyperliquid:$GITHUB_SHA
            kubectl -n agentic-hyperliquid rollout status deploy/bot --timeout=300s
          "
```

- [ ] **Step 2: Validate the workflow YAML parses**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/deploy.yml')); print('workflow yaml ok')"`
Expected: `workflow yaml ok`. (If `actionlint` is installed, also run `actionlint .github/workflows/deploy.yml` and expect no errors.)

- [ ] **Step 3: Confirm no server IP/hostname leaked into the workflow**

Run: `grep -nE "([0-9]{1,3}\.){3}[0-9]{1,3}" .github/workflows/deploy.yml || echo "no ip ✓"`
Expected: `no ip ✓` (host comes only from the `DEPLOY_SSH_HOST` secret).

- [ ] **Step 4: Commit**
```bash
git add .github/workflows/deploy.yml
git commit -m "ci: build to GHCR and roll out to k3s on push to main"
```

---

### Task 4: Runbook + README + final scrub

**Files:**
- Create: `k8s/README.md`
- Modify: `README.md` (add a "Deploy (k3s)" section)

**Interfaces:**
- Consumes: everything above (manifests, workflow, image).
- Produces: the one-time setup runbook so a human can bootstrap the cluster; CI handles subsequent rollouts.

- [ ] **Step 1: Create `k8s/README.md`** (NO server IP — refer to the `DEPLOY_SSH_HOST` secret / "the VPS")
````markdown
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
````

- [ ] **Step 2: Add a "Deploy (k3s)" section to `README.md`** (append before or after Configuration):
```markdown
## Deploy (k3s)

CI/CD builds the bot to GHCR and rolls it out to the production k3s cluster on
every push to `main` (`.github/workflows/deploy.yml`). It runs as a singleton
daemon (no inbound traffic). See [`k8s/README.md`](k8s/README.md) for one-time
cluster setup (secrets, image pull, PVC).
```

- [ ] **Step 3: Final scrub — no IP anywhere in tracked files**

Run: `git grep -nIE "([0-9]{1,3}\.){3}[0-9]{1,3}" -- . ':!Cargo.lock' || echo "no ip in tracked files ✓"`
Expected: `no ip in tracked files ✓` (ignore any version-like matches in lockfiles; there should be none that look like the VPS IP).

- [ ] **Step 4: Confirm the Rust project still builds & tests cleanly (unchanged by this infra work)**

Run: `cargo build 2>&1 | grep -c warning` (expect `0`) and `cargo test 2>&1 | grep "test result:"` (expect all pass).

- [ ] **Step 5: Commit**
```bash
git add k8s/README.md README.md
git commit -m "docs: k3s deploy runbook and README section"
```

---

## Self-Review Notes

- **Spec coverage:** Dockerfile multi-stage + WORKDIR /data + ca-certs (Task 1); GHCR build+push latest/:sha amd64 (Task 3 build job); SSH kubectl set image + rollout, concurrency, :sha pin (Task 3 deploy job); namespace/PVC/singleton-Recreate/envFrom/imagePullSecret (Task 2); manual one-time secret+pull+apply runbook, rollback, rotation, no-IP rule (Task 4). All spec sections mapped.
- **No-IP rule:** enforced by Task 3 Step 3 and Task 4 Step 3 scrubs; the host lives only in `DEPLOY_SSH_HOST`.
- **Naming consistency:** namespace `agentic-hyperliquid`, Deployment `bot`, container `bot`, Secret `agentic-hyperliquid-env`, pull secret `ghcr-pull`, image `ghcr.io/bimapangestu28/agentic-hyperliquid` — identical across the Dockerfile, manifests, workflow, and runbook.
- **Placeholder scan:** the only angle-bracket token is `<GHCR_PAT_read_packages>` / `<older-sha>` in the runbook — those are genuine human-supplied values at setup/rollback time, not implementation placeholders.
- **Infra-only:** no Rust changes; Task 4 Step 4 re-confirms build/tests.
