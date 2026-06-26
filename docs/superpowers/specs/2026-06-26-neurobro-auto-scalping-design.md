# Neurobro Auto-Scalping Pipeline — Design

**Date:** 2026-06-26
**Status:** Draft (pending user review)

## Overview

Automate the user's manual scalping workflow end-to-end. Today the user manually:
screenshots a coin's chart on Hyperliquid, uploads it to Neurobro's web chat, asks
for a one-TP scalping setup, and — if Neurobro's confidence is ≥ 7 — pastes the
resulting setup card into the Telegram bot and opens the position at 20x.

This project replaces that loop with an automated pipeline that runs 24/7 on the
VPS, recycling capital: when a position closes (TP or SL), the system scans the
next watchlist coin and opens a new position — **fully automatically, no manual
confirmation**, with a Telegram notification on every action.

## Goals & non-goals

**Goals:**
- Fully automatic loop: scan → setup → execute → wait for close → rescan.
- Only act on Neurobro setups with confidence ≥ 7.
- Every auto-opened position fires a Telegram notification.
- Watchlist of coins managed from Telegram (add/remove), persisted.

**Non-goals:**
- No manual confirmation step (explicit user decision — trust Neurobro).
- No multi-TP (always single TP1 100%, per the Neurobro prompt).
- No change to the existing manual paste-a-card flow (it keeps working).

## Risk acknowledgement

This is **full-auto trading with real money** at 20x leverage with position
stacking enabled, gated only by Neurobro's confidence score — no human in the
loop. A bad scrape, parse error, or wrong Neurobro call results in a real order.
The notification informs after the fact; it does not prevent. Mitigations built
into this design: a default-OFF master switch (`auto_scalp_enabled`), a
max-concurrent-positions cap, a per-coin cooldown, strict confidence gating, and
fail-closed behavior (any parse/extraction doubt → skip, never guess).

## Architecture

Two independently-testable subsystems, implemented as two plans in sequence:

1. **Bot changes (Rust)** — watchlist in settings + two API endpoints + an
   auto-execute path that sizes, orders, brackets, and notifies WITHOUT the
   Telegram confirmation button. Built and tested first.
2. **Scraper (TypeScript + Playwright)** — a separate process/container on the
   VPS that drives the browsers and calls the bot's API. Depends on subsystem 1.

```
┌────────────────────────── VPS ──────────────────────────┐
│                                                          │
│  scraper (Playwright/TS) ──HTTP──▶ bot API (axum)        │
│    │  GET /watchlist, GET /positions                     │
│    │  POST /execute  ──────────────▶ size+order+bracket  │
│    │                                  + Telegram notif 🔔 │
│    ▼                                                      │
│  Chromium (headless, saved session)                      │
│    ├─ app.hyperliquid.xyz/trade/<COIN>  → screenshot     │
│    └─ app.neurobro.ai (chat)            → upload + prompt │
│                                          → read setup card│
└──────────────────────────────────────────────────────────┘
```

## Component 1 — Bot: watchlist in settings (Rust)

- Add `watchlist: Vec<String>` to `Settings` (`src/settings.rs`), persisted via
  the existing `SettingsStore` (SQLite). Coins stored upper-cased, de-duplicated.
- Add `auto_scalp_enabled: bool` (default `false`) and `max_open_positions: u32`
  (default e.g. 5) to `Settings` as safety controls.
- Telegram commands (in `on_message`, following the `/account` pattern):
  - `/watch` — list current watchlist + auto_scalp on/off + caps.
  - `/watch add <COIN>` / `/watch remove <COIN>` — mutate and persist.
  - Reuse `/set auto_scalp_enabled true|false`, `/set max_open_positions <n>`.
- Pure functions (unit-tested): `add_coin`, `remove_coin` (normalize, dedupe),
  and render of the `/watch` message.

## Component 2 — Bot: read + execute API endpoints (Rust)

Extend `src/api/mod.rs` (axum, bearer-auth already present via `require_bearer`):

- `GET /watchlist` → `{ coins: [...], auto_scalp_enabled, max_open_positions }`.
- `GET /positions` already exists — the scraper uses it to find free slots.
- `POST /execute` (bearer) — body is a structured setup the scraper extracted:
  ```json
  { "coin": "AVAX", "direction": "long", "entry": 6.12, "stop_loss": 5.99,
    "take_profit": 6.32, "confidence": 8, "thesis": "..." }
  ```
  Behavior:
  1. Reject if `auto_scalp_enabled == false` (HTTP 409) — master kill-switch.
  2. Reject if `confidence < 7` (HTTP 422) — defense in depth (scraper also filters).
  3. Reject if open positions ≥ `max_open_positions` (HTTP 409).
  4. Build a `Setup` from the body and run the SAME sizing + entry + bracket logic
     the manual flow uses (`build_plan` with the **Aggressive** profile = 20x,
     market entry, reduce-only SL + single TP), but with **no Telegram confirm**.
  5. Send a Telegram notification to allowlisted users:
     `🤖 Auto-buka {coin} {DIR} {size} @ {entry} · SL {sl} · TP {tp} · conf {c}/10`.
  6. Return `{ "ok": true, "order_id": ... }` or an error with reason.
- Refactor: extract the size→order→bracket core out of the current
  confirm-callback path into a shared `execute_setup(exchange, setup, profile)`
  function so both the manual confirm flow and `POST /execute` call ONE
  implementation (DRY). The manual flow keeps its confirm button; only the
  execution core is shared.

## Component 3 — Scraper (TypeScript + Playwright, on VPS)

A standalone Node project in `scraper/` with its own `package.json`, `Dockerfile`,
and config via env. Runs as a separate container alongside the bot.

**Config (env):** `BOT_API_URL`, `BOT_API_TOKEN` (bearer), `NEUROBRO_STORAGE_STATE`
(path to saved session JSON), `HYPERLIQUID_URL`, `POLL_INTERVAL_SECS`,
`COOLDOWN_SECS` (per-coin re-scan cooldown after a skip).

**Session bootstrap (one-time, manual):** Neurobro login is OTP ("send code"),
which is impractical to automate. The user logs in ONCE in a headed Playwright
browser (locally or via VNC on the VPS); Playwright saves `storageState` (cookies
+ localStorage) to `NEUROBRO_STORAGE_STATE`. The loop reuses that session
headlessly. When it expires, the scraper detects the login redirect, sends a
Telegram alert ("Neurobro session expired — re-login"), and pauses scanning.

**Main loop (every `POLL_INTERVAL_SECS`):**
1. `GET /watchlist`; if `auto_scalp_enabled == false`, sleep and continue.
2. `GET /positions`; compute free coins = watchlist coins with no open position,
   not in cooldown, while total open < `max_open_positions`.
3. For each free coin (sequentially, to keep one browser context simple):
   a. Navigate Chromium to `HYPERLIQUID_URL/trade/<COIN>`; wait for the chart;
      screenshot the chart region to a buffer.
   b. Navigate to `app.neurobro.ai` chat; start a new chat; attach the screenshot;
      send the prompt (below); wait for the "Setup Trading" card to render.
   c. Extract from the card DOM: coin, direction (ARAH), entry (Masuk), SL,
      TP1, confidence (KEYAKINAN x/10), thesis (TESIS). **Fail-closed:** if any
      required numeric field is missing/unparseable, skip this coin (log + cooldown).
   d. If `confidence >= 7`: `POST /execute` with the structured setup. On success,
      the position now exists (so the coin is no longer "free"). On 4xx/5xx, log +
      cooldown.
   e. If `confidence < 7`: skip, set per-coin cooldown.
4. Sleep, repeat.

**The Neurobro prompt** (sent with each chart screenshot; `{COIN}` substituted):
> Analisa chart {COIN} ini buat scalping di Hyperliquid perpetual. Kasih SATU
> setup scalping hit-and-run: tentukan arah (LONG/SHORT), harga Masuk, Stop Loss,
> dan SATU Take Profit aja (TP1 100%, JANGAN ada TP2/TP3). Wajib sertakan angka
> harga eksplisit, tingkat Keyakinan (x/10), dan tesis singkat. Tampilkan sebagai
> kartu "Setup Trading".

**Capital-recycling trigger:** the scraper does NOT subscribe to close events. It
infers a freed slot from `GET /positions`: once a bracket TP/SL closes a position
on Hyperliquid (handled by the bot's existing `run_fill_monitor`, unchanged), that
coin drops out of `/positions`, becomes "free" on the next poll, and is rescanned.
This is the loop, decoupled and crash-safe.

## Data flow (one cycle)

```
poll ─▶ GET /watchlist + /positions ─▶ free coin C
     ─▶ HL chart(C) screenshot ─▶ Neurobro upload+prompt ─▶ setup card
     ─▶ extract {dir,entry,sl,tp,conf} ─▶ conf≥7?
          ├─ no  ─▶ cooldown(C)
          └─ yes ─▶ POST /execute ─▶ bot sizes+orders+brackets+🔔notif
                                   ─▶ (later) TP/SL closes ─▶ slot frees ─▶ rescan
```

## Error handling

| Failure | Handling |
|---|---|
| Neurobro session expired | Detect login redirect → Telegram alert → pause scan |
| Neurobro UI/selector change | Extraction fails → skip coin, log, cooldown; alert after N consecutive failures |
| Chart screenshot fails | Skip coin, log, cooldown |
| Confidence unparseable | Treat as < 7 → skip (fail-closed) |
| `POST /execute` 4xx/5xx | Log reason; cooldown; no retry storm |
| `auto_scalp_enabled` off | Bot rejects /execute (409); scraper also idles |
| Position cap reached | Scraper stops opening; resumes when a slot frees |

## Testing

**Bot (Rust):**
- Pure: `add_coin`/`remove_coin` normalize + dedupe; `/watch` render; `/execute`
  request validation (confidence gate, cap, kill-switch) via mock exchange.
- `execute_setup` shared core: a setup runs sizing + entry + bracket on
  `MockExchange` (assert order + bracket placed, profile = Aggressive).
- `POST /execute` handler: 409 when disabled, 422 when conf<7, 409 at cap,
  200 + notification path on success (axum test harness, as existing API tests).

**Scraper (TS):**
- Pure unit: card-extraction parser over saved Neurobro DOM/HTML fixtures
  (including a conf-6 fixture that must be filtered, and a malformed fixture that
  must fail-closed). Confidence parsing, free-slot computation, cooldown logic.
- Playwright flows are validated manually against the live sites during
  implementation (selectors require the real DOM); a smoke script logs in with a
  saved session and runs one dry cycle without calling `/execute`.

## Files

**Bot:** `src/settings.rs` (watchlist + flags), `src/telegram.rs` (`/watch`
commands, shared `execute_setup` extraction, auto-open notification),
`src/api/mod.rs` (`GET /watchlist`, `POST /execute`).

**Scraper (new):** `scraper/package.json`, `scraper/Dockerfile`,
`scraper/src/config.ts`, `scraper/src/botApi.ts`, `scraper/src/hyperliquid.ts`
(chart screenshot), `scraper/src/neurobro.ts` (upload + extract),
`scraper/src/extract.ts` (pure parser), `scraper/src/loop.ts` (orchestration),
`scraper/src/login.ts` (one-time session bootstrap), tests under `scraper/test/`.

**Deploy:** extend the k8s manifests / compose to run the scraper container with
the `storageState` mounted from a persistent volume.

## Open implementation-time dependencies

These cannot be finalized from a spec — they need the live DOM and are resolved
during implementation, not now:
- Exact Neurobro selectors: chat input, image-attach control, the "Setup Trading"
  card and each field. (User to provide DOM/screenshots, or done interactively.)
- Exact Hyperliquid chart container selector + the per-coin trade URL pattern.
- Neurobro session lifetime (how often re-login is needed in practice).
