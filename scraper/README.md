# Neurobro Scraper

A Playwright service that watches Hyperliquid charts, submits them to Neurobro AI for
trade-setup analysis, and — when the bot's `auto_scalp_enabled` flag is ON — executes
approved setups via the bot's internal HTTP API.

> **It is NOT headless.** Neurobro sits behind Cloudflare Turnstile, which flags both
> Playwright's bundled "Chrome for Testing" and headless mode. The scraper therefore
> runs **real Google Chrome (`channel: chrome`), headful, with a persistent profile**
> (so the Cloudflare `cf_clearance` cookie + Neurobro auth survive across runs). On a
> server it runs headful under **Xvfb** (a virtual display).

---

## Environment Variables

Copy `.env.example` and fill it in before running locally.

| Variable | Required | Default | Description |
|---|---|---|---|
| `BOT_API_URL` | yes | — | Base URL of the bot's HTTP API (local: `http://127.0.0.1:8088`; in-cluster: `http://bot:8088`) |
| `BOT_API_TOKEN` | yes | — | Bearer token; must equal the bot's `PORTFOLIO_API_TOKEN` |
| `NEUROBRO_USER_DATA_DIR` | no | `./neurobro-profile` | Persistent Chrome profile dir (Cloudflare + Neurobro session live here) |
| `HEADLESS` | no | `false` | Keep `false`; headless is blocked by Cloudflare. `true` only behind Xvfb if you must |
| `BROWSER_CHANNEL` | no | `chrome` | Real installed Chrome; do not use the bundled Chromium |
| `HYPERLIQUID_URL` | no | `https://app.hyperliquid.xyz` | Hyperliquid web app |
| `NEUROBRO_URL` | no | `https://app.neurobro.ai` | Neurobro web app |
| `POLL_INTERVAL_SECS` | no | `60` | Seconds between scan cycles |
| `COOLDOWN_SECS` | no | `300` | Per-coin cooldown after a skip or failed execution |
| `MAX_DEVIATION` | no | `0.004` | Max fractional mark-vs-entry deviation (slippage gate) |
| `TELEGRAM_BOT_TOKEN` | no | — | Reuse the bot's token to send "session expired" alerts |
| `TELEGRAM_CHAT_ID` | no | — | Your chat id; both Telegram vars must be set or alerts are only logged |
| `NEUROBRO_STORAGE_STATE` | no | `./neurobro-session.json` | Legacy storageState snapshot (the profile dir is the source of truth) |

Requires Node ≥ 20. First run: `npm install` then `npx playwright install chrome`.

---

## One-Time Session Bootstrap (Neurobro Login)

Neurobro login is OTP + a Cloudflare check — it can't run unattended. Do it once with a
visible browser; the persistent profile then carries the session into the loop.

```bash
cd scraper
cp .env.example .env          # set BOT_API_URL + BOT_API_TOKEN
npm install
npx playwright install chrome # real Chrome (once)
npm run login                 # opens real Chrome
```

In the opened window: solve the Cloudflare "Verify you are human" check, log in with
your OTP, then press Enter in the terminal. The session is saved to `./neurobro-profile`.

### Deploying the session to the server

Copy the **whole profile directory** onto the shared PVC (it holds the `cf_clearance`
cookie + auth):

```bash
kubectl -n agentic-hyperliquid cp ./neurobro-profile \
  $(kubectl -n agentic-hyperliquid get pod -l app=agentic-hyperliquid -o jsonpath='{.items[0].metadata.name}'):/data/neurobro-profile
```

### Session expiry

The Cloudflare cookie expires (≈30 min–hours). When it does, the scraper detects the
wall, sends a Telegram alert ("session expired — re-run `npm run login`"), and pauses
scanning until you refresh the profile. For 24/7 use, run headful under Xvfb and expect
to re-login periodically.

---

## Running

```bash
npm run dry-run   # one cycle; never calls /execute (selector + parse validation)
npm run once      # one cycle; DOES execute if a setup passes all gates, then exits (cron / first live test)
npm start         # 24/7 loop (runForever)
```

`dry-run` is the safe way to validate the live selectors after any Neurobro UI change —
it logs the parsed setup (or the skip reason) and `[dry-run] would execute` without
trading.

---

## How a Cycle Works

1. **Check flag** — `GET /watchlist`. If `auto_scalp_enabled` is false → idle.
2. **Session check** — verify the Neurobro composer is reachable. If a Cloudflare/login
   wall is up → alert once via Telegram + skip the cycle (no per-coin hammering).
3. **Free coins** — `GET /positions`, then filter the watchlist: drop held coins and
   coins in cooldown, enforce `max_open_positions`.
4. **Per eligible coin:** screenshot the HL chart → start a fresh Neurobro chat → upload
   + prompt → wait for the setup table to finish streaming → parse it (coin supplied by
   the loop). **Gates (fail-closed):** skip on no setup, confidence < 7, or a fresh
   mark-vs-entry deviation over `MAX_DEVIATION`. Otherwise `POST /execute`. Any skip or
   failure sets a per-coin cooldown.
5. **Resilience** — one coin's error never aborts the cycle; one bad cycle never kills
   the loop; `SIGTERM`/`SIGINT` closes Chrome cleanly.

---

## Enabling Live Trading

Trades only fire when the bot's flag is on. Toggle via Telegram:

```
/set auto_scalp_enabled true     # arm
/set auto_scalp_enabled false    # pause (scraper keeps polling)
```

---

## Deployment (k8s)

See `k8s/scraper-deployment.yaml` and `k8s/bot-service.yaml`. Prerequisites:

1. **Bot API reachable** — apply `k8s/bot-service.yaml` (ClusterIP `bot:8088`) **and**
   set the bot's `HTTP_BIND_ADDR=0.0.0.0:8088` (it binds loopback by default) plus
   `PORTFOLIO_API_TOKEN` (the API is disabled without it; that token is the scraper's
   `BOT_API_TOKEN`).
2. **Profile on the PVC** — complete the bootstrap above and copy `neurobro-profile/`
   to `/data/neurobro-profile`; the pod reuses it via `NEUROBRO_USER_DATA_DIR`.
3. **Image** — the Dockerfile installs real Chrome + Xvfb and runs headful via
   `xvfb-run`. Build/push `ghcr.io/bimapangestu28/agentic-hyperliquid-scraper` (CI should
   re-pin `:latest` → `:sha`).
4. **Telegram alerts (optional)** — set `TELEGRAM_CHAT_ID` in the manifest; the token
   comes from the shared secret.

```bash
kubectl apply -f k8s/bot-service.yaml
kubectl apply -f k8s/scraper-deployment.yaml
kubectl -n agentic-hyperliquid logs deploy/scraper -f
```

> **Caveat:** running an automated browser against Cloudflare 24/7 is inherently
> fragile. Headful + a warm persistent profile is the most reliable setup, but expect
> periodic re-logins. Validate the container in your actual VPS before trusting it with
> live capital, and consider semi-attended operation first.
