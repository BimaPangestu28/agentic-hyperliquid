# Trigger/Stop Entry Orders — Design

**Date:** 2026-06-19
**Status:** Approved (pending spec review)

## Overview

The bot currently enters via market (fill now) or GTC limit (rest at a price, fill when
the market touches it from the favorable side). Neither fits a **reclaim/breakout**
setup, where the user wants to enter only *when price crosses the entry level upward*
(long) or downward (short). Limit entries for these setups simply time out because the
market never trades back to the limit.

This feature adds a **trigger/stop entry**: an order that rests dormant on the exchange
and executes (as a market order) only when price crosses the entry trigger price. The
reduce-only SL/TP bracket is armed after the entry actually fills, by a background
monitor, and survives process restarts.

## Goals / Non-goals

**Goals**
- A third `Confirm Trigger` button alongside Confirm Limit / Confirm Market.
- Place a non-reduce-only market-on-trigger entry at the setup's entry price.
- Arm the SL/TP bracket (sized to the actual filled position) after the trigger fires,
  via a background monitor that survives restarts (pending state persisted in SQLite).
- Auto-cancel an unfilled trigger after a configurable expiry.
- Warn the user when the trigger price is on the wrong side of the current mark (would
  fire immediately, i.e. behave like market).

**Non-goals**
- No in-bot cancel UI for a resting trigger (MVP: cancel via the Hyperliquid app or wait
  for expiry).
- No trailing/dynamic trigger prices.
- No change to market/limit entry behavior.

## Architecture

```
Confirm Trigger tap
  → set leverage (account/coin level)
  → place entry-trigger order on Hyperliquid (reduce_only=false, market-on-trigger)
  → persist PendingTrigger (full bracket spec) in SQLite
  → reply "🎯 Trigger {coin} @ {px} dipasang — nunggu tembus."

Background run_trigger_monitor (own spawned task, every MONITOR_POLL_SECS):
  for each active PendingTrigger:
    if an OPENING fill for entry_oid is seen in the fills feed (filled):
        arm SL/TP bracket sized to the ACTUAL FILLED size (arm_bracket)
        mark armed → notify "✅ Trigger {coin} kena — SL/TP terpasang."
        if arming fails: notify "⚠️ Posisi {coin} TERBUKA tapi GAGAL pasang SL — cek manual!" and retry next poll
    else if now > expiry_at:
        cancel resting entry-trigger (best-effort) → mark expired → notify "⏱️ Trigger {coin} kadaluarsa, dibatalkan."
```

**Fill detection must NOT use aggregate `position_size > 0`** — that breaks when a position
already exists on the same coin (it would falsely report an immediate fill). Instead the
monitor matches the *specific* entry order's opening fill (see Component 4), so it is
correct regardless of any pre-existing position.

```text
```

The trigger monitor is a **separate** spawned task from the close-notification monitor
(`run_fill_monitor`) to keep single responsibility, though both live in `src/monitor.rs`.

## Components

### 1. UX — third confirm button (`src/telegram.rs`)
- New callback constant `CB_TRIGGER` and a `🎯 Confirm Trigger` button on the summary
  keyboard, next to Confirm Limit / Confirm Market.
- The confirm handler routes `CB_TRIGGER` to a trigger path (analogous to the existing
  limit/market path), reusing the synchronous prelude (daily-risk-cap check,
  answer_callback_query, edit "Executing…"/"Memasang trigger…").

### 2. Exchange — `place_trigger_entry` (`src/hyperliquid/mod.rs`)
- New `Exchange` trait method:
  `async fn place_trigger_entry(&self, coin: &str, is_buy: bool, size: f64, trigger_price: f64) -> anyhow::Result<OrderResult>`
- Hyperliquid impl: `ClientOrderRequest { reduce_only: false, limit_px: trigger_price,
  order_type: ClientOrder::Trigger(ClientTrigger { trigger_px: trigger_price,
  is_market: true, tpsl: <see below> }) }`, parsed via the existing `parse_order_response`.
- **Open technical risk (verify in plan):** the correct `tpsl` ("tp"/"sl") + `is_buy`
  combination so a LONG entry fires when price rises through the trigger (buy-stop) and a
  SHORT entry fires when price falls through it (sell-stop). To be confirmed against
  Hyperliquid docs / a testnet probe before implementation; encapsulate the mapping in a
  small pure helper `trigger_tpsl(direction) -> &'static str` so it is unit-testable and
  changeable in one place.
- `MockExchange` impl records trigger-entry calls (for orchestration tests).

### 3. Persistence — `PendingTrigger` + `TriggerStore` (`src/trigger_store.rs`, new)
- Mirrors the `SettingsStore` pattern: its own SQLite connection on the shared
  `journal_path` file, `CREATE TABLE IF NOT EXISTS pending_triggers`.
- `PendingTrigger` fields: `id`, `coin`, `direction`, `size`, `trigger_px`, `leverage`,
  `stop_loss` (price), `take_profits` (JSON of `{price, alloc_pct}`), `entry_oid`
  (Option), `chat_id`, `created_at`, `expiry_at`, `status` ("active"|"armed"|"expired").
- Methods: `insert(&PendingTrigger)`, `list_active() -> Vec<PendingTrigger>`,
  `mark_armed(id)`, `mark_expired(id)`.

### 4. Monitor — `run_trigger_monitor` (`src/monitor.rs`)
- Spawned in `telegram::run` with its own `TriggerStore` connection, `bot`, `exchange`,
  `allowed_user_ids`, poll interval (`MONITOR_POLL_SECS`), and the expiry setting source.
- **Fill detection (robust to pre-existing positions):** the monitor polls
  `exchange.fills_detailed()` and looks for an **opening fill** (`dir` contains "Open")
  whose `oid == entry_oid` and `time_ms >= created_at`. Matching the specific entry order
  — not aggregate `position_size` — means an existing position on the same coin does NOT
  cause a false "filled" detection. The bracket is armed to the matched fill's `sz` (the
  actual filled size). If `oid`-matching proves unreliable on Hyperliquid (verify in
  plan), fall back to matching by `coin` + opening `dir` + `time_ms >= created_at`.
  - The fills feed is already polled by `run_fill_monitor`; the trigger monitor polls it
    for opening fills (vs. the close monitor's closing fills) — separate concerns, same
    cheap query.
- Pure helpers (TDD): `is_expired(now, expiry_at)`; a `matches_entry_fill(fill,
  pending) -> bool` predicate (opening dir + oid/time match); the fill/expiry decision.
- Never panics: all poll/exchange/DB/send errors are logged via `tracing` and swallowed.

### 5. Refactor — extract `arm_bracket` (`src/telegram.rs`)
- Extract the SL + TP bracket-placement block from `execute_plan` into a reusable
  `async fn arm_bracket(exchange, coin, direction, size, stop_loss, take_profits)` so both
  `execute_plan` (limit/market) and `run_trigger_monitor` (trigger) place brackets
  identically. `execute_plan` is updated to call it; existing execute_plan tests must
  still pass.

### 6. Settings — `trigger_expiry_secs` (`src/settings.rs`, `src/telegram.rs`)
- New `Settings.trigger_expiry_secs: u64` (default `14400` = 4h), seeded from config,
  validated (`>= 1`) via `/set`, persisted/loaded, shown in `render_settings`. Follows the
  exact pattern used for `entry_fill_timeout_secs`.
- New config/env `TRIGGER_EXPIRY_SECS` (default 14400).

### 7. Validation & warning (`src/telegram.rs` confirm flow)
- Before placing, compare `trigger_px` to the current mark. If LONG and `trigger_px <=
  mark` (or SHORT and `trigger_px >= mark`), the trigger would fire immediately — append a
  warning to the confirmation/acknowledgement so the user knows it behaves like market.

## Notifications
- Placed: `🎯 Trigger {coin} @ ${px} dipasang — nunggu harga menembus.`
- Filled + armed: `✅ Trigger {coin} kena — SL/TP terpasang.`
- Arming failed (critical, retried each poll): `⚠️ Posisi {coin} TERBUKA tapi GAGAL pasang SL — cek manual SEKARANG!`
- Expired: `⏱️ Trigger {coin} kadaluarsa ({expiry}) — dibatalkan, tidak ada posisi.`

## Error handling
- Monitor loop swallows+logs all errors; one bad trigger never stops the loop.
- Arming failure does NOT mark the trigger armed — it stays active so the next poll retries
  (at-least-once arming; the position is never silently left without a stop).
- Restart-safety: pending triggers live in SQLite; on boot the monitor reloads active rows
  and resumes arming/expiry. The entry trigger order itself lives on the exchange.

## Testing (TDD)
- Pure: `trigger_tpsl(direction)`; `is_expired(now, expiry_at)`; `matches_entry_fill(fill,
  pending)` (opening-dir + oid/time match, incl. the pre-existing-position case where a
  closing or older fill must NOT match); trigger-side warning predicate; `PendingTrigger`
  JSON (take_profits) round-trip;
  `TriggerStore` insert/list_active/mark_armed/mark_expired; settings `trigger_expiry_secs`
  validation + persist/load.
- Orchestration: `arm_bracket` via `MockExchange` (SL + scaled TPs placed); confirm the
  refactor keeps existing `execute_plan` tests green.
- The monitor loop and confirm handler are integration glue (no live Bot in unit tests),
  verified by build + suite green, per the existing codebase convention.

## Files touched
- `src/telegram.rs` — CB_TRIGGER button + handler path, `arm_bracket` extraction,
  trigger-side warning, monitor spawn, settings display.
- `src/hyperliquid/mod.rs` — `place_trigger_entry` (trait + Hyperliquid + Mock).
- `src/trigger_store.rs` — **new**: `PendingTrigger` + `TriggerStore`.
- `src/monitor.rs` — `run_trigger_monitor` + pure helpers.
- `src/settings.rs` — `trigger_expiry_secs`.
- `src/config.rs` — `TRIGGER_EXPIRY_SECS`.
- `src/main.rs` — `mod trigger_store;`.
- `.env.example` / `README.md` — document the trigger button + `TRIGGER_EXPIRY_SECS`.
