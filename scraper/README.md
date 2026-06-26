# Neurobro Scraper

A headless Playwright scraper that watches Hyperliquid charts, submits them to
Neurobro AI for trade-setup analysis, and — when the bot's `auto_scalp_enabled`
flag is ON — executes approved setups via the bot's internal HTTP API.

---

## Environment Variables

Copy `.env.example` and fill in the values before running locally.

| Variable | Required | Default | Description |
|---|---|---|---|
| `BOT_API_URL` | yes | — | Base URL of the bot's internal HTTP API (e.g. `http://localhost:8080`) |
| `BOT_API_TOKEN` | yes | — | Bearer token the scraper presents to the bot API (must match the bot's configured token) |
| `HYPERLIQUID_URL` | no | `https://app.hyperliquid.xyz` | Hyperliquid web app URL |
| `NEUROBRO_URL` | no | `https://app.neurobro.ai` | Neurobro web app URL |
| `NEUROBRO_STORAGE_STATE` | no | `./neurobro-session.json` | Path to the Playwright session file (Neurobro login cookies) |
| `POLL_INTERVAL_SECS` | no | `60` | Seconds between scan cycles |
| `COOLDOWN_SECS` | no | `300` | Per-coin cooldown after a skip or failed execution |
| `MAX_DEVIATION` | no | `0.004` | Maximum allowed fractional deviation between mark price and setup entry (slippage gate) |

---

## One-Time Session Bootstrap (Neurobro Login)

Neurobro requires an OTP login that cannot run headless. Run this once locally
to capture the session, then upload it to the PVC.

### Step 1 — Login locally

```bash
cd scraper
cp .env.example .env
# edit .env: set NEUROBRO_URL (leave NEUROBRO_STORAGE_STATE as ./neurobro-session.json)
npm install
npm run login
```

Follow the on-screen prompts (Playwright opens a visible browser). After you
complete the OTP flow, the script writes `neurobro-session.json` in the current
directory.

### Step 2 — Copy the session file onto the PVC

Find the running scraper pod (or any pod that mounts the `data` PVC) and copy
the file to `/data/`:

```bash
# Using the bot pod (always running, always mounts /data):
kubectl -n agentic-hyperliquid cp neurobro-session.json \
  $(kubectl -n agentic-hyperliquid get pod -l app=agentic-hyperliquid -o jsonpath='{.items[0].metadata.name}'):/data/neurobro-session.json
```

The scraper reads `NEUROBRO_STORAGE_STATE=/data/neurobro-session.json` from the
same PVC on startup.

### Session expiry

Neurobro sessions are long-lived but do expire. When the scraper logs a
`storageState` or authentication error, repeat the bootstrap above.

---

## Validating Selectors (Dry Run)

Before deploying, validate that all Playwright locators match the live pages:

```bash
npm run dry-run
```

This runs one full scan cycle — screenshot, Neurobro upload, response parse —
but skips the final `POST /execute` call. Check the console output for parse
errors or missing selectors and adjust `src/neurobro.ts` / `src/hyperliquid.ts`
as needed.

---

## How the 24/7 Loop Works

`npm start` enters `runForever`, which repeats this cycle every
`POLL_INTERVAL_SECS` seconds:

1. **Check flag** — calls `GET /watchlist` on the bot API. If `auto_scalp_enabled`
   is `false`, the cycle exits immediately ("auto_scalp disabled — idle").
2. **Free coins** — calls `GET /positions` and filters the watchlist: excludes
   coins already held, coins in per-coin cooldown, and enforces the
   `max_open_positions` cap from the watchlist response.
3. **For each eligible coin:**
   - Screenshots the Hyperliquid chart and reads the current mark price.
   - Uploads the screenshot to Neurobro and parses the returned HTML for a
     trade setup (entry, stop-loss, take-profit, direction, confidence score).
   - **Gates (fail-closed):** skips if no parseable setup, confidence < 7, or
     mark-price deviation from setup entry exceeds `MAX_DEVIATION`.
   - Calls `POST /execute` on the bot API with the approved setup.
   - Places the coin in cooldown after any skip or failed execution.
4. **Error isolation** — a crash in one coin's scan is caught and logged; the
   loop continues with the next coin and the next cycle.
5. **Graceful shutdown** — on `SIGTERM` / `SIGINT` (k8s pod stop), the Chromium
   browser is closed cleanly before the process exits.

---

## Enabling Live Trading

The scraper will only execute trades when the bot's `auto_scalp_enabled` flag is
`true`. Toggle it via the Telegram bot:

```
/set auto_scalp_enabled true
```

To pause trading without stopping the scraper pod:

```
/set auto_scalp_enabled false
```

The scraper polls the flag every cycle, so the change takes effect within one
`POLL_INTERVAL_SECS` window.

---

## Deployment (k8s)

See `k8s/scraper-deployment.yaml`. Prerequisites before applying:

1. **Bot Service** — the bot currently has no k8s Service. Create
   `k8s/bot-service.yaml` (ClusterIP, port 8080) so the scraper can reach
   `http://bot:8080` in-cluster.
2. **Session file** — complete the one-time bootstrap above before starting
   the scraper pod; it will fail on startup if the session file is missing.
3. **Image** — build and push `ghcr.io/bimapangestu28/agentic-hyperliquid-scraper`
   (the manifest pins `:latest`; update to a `:sha` tag in CI).

```bash
kubectl apply -f k8s/scraper-deployment.yaml
kubectl -n agentic-hyperliquid logs deploy/scraper -f
```
