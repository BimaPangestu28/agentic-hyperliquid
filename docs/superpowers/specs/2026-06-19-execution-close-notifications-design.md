# Execution & Close Notifications — Design

**Date:** 2026-06-19
**Status:** Approved (pending spec review)

## Overview

The bot currently sends only three Telegram messages per trade: `Executing {coin}…`,
`✅ Executed {coin} with SL/TP bracket.`, and `❌ Execution failed: {error}`. It is
silent during a limit order's fill wait, silent on partial fills, and — most
importantly — silent when a TP or SL later closes the position. A user only learns
the outcome by manually running `/account` or `/stats`.

This feature adds:

1. **Concise execution-step notifications** — progress messages while the entry is
   being placed and filled.
2. **Close notifications** — a Telegram message when a TP or SL trigger actually
   fills and closes (or reduces) a position, detected by polling.
3. **Leg labelling + realized PnL** — each close message names the leg (`SL`,
   `TP1`, `TP2`) and reports realized PnL.

## Goals / Non-goals

**Goals**
- Notify the user at each meaningful execution step (concise, not per-action).
- Notify the user when a position is closed by a bracket order, with leg + PnL.
- Survive process restarts without re-notifying historical fills.
- Keep execution orchestration (`execute_plan`) decoupled from Telegram and unit-testable.

**Non-goals**
- No websocket subscription (polling chosen for simplicity/robustness).
- No real-time tick streaming or price alerts.
- No change to sizing/risk/bracket logic.

## Architecture

Three components across existing + one new module.

### Component 1 — `ProgressReporter` (execution-step notifications)

`execute_plan` in `src/telegram.rs` currently returns `Result<()>` and emits nothing
during the fill wait. Introduce an async progress sink so the wait loop can report
without coupling `execute_plan` to Telegram (preserving the existing
`execute_plan_*` unit tests, which will pass a no-op reporter).

```rust
pub enum ExecutionEvent {
    EntrySubmitted { limit: bool, price: f64 },     // market: "sent"; limit: "resting, waiting for fill"
    Filled { size: f64, partial: bool },            // full or partial fill detected
    FillTimeout { cancelled: bool },                // limit not filled within timeout
    BracketArmed { stop_loss: f64, take_profits: usize },
}

#[async_trait]
pub trait ProgressReporter: Send + Sync {
    async fn report(&self, event: ExecutionEvent);
}
```

- `execute_plan` gains a `&dyn ProgressReporter` parameter and calls `report(..)` at:
  entry submission, fill detected (full/partial), timeout, bracket armed.
- Production impl: `TelegramReporter { bot: Bot, chat_id: ChatId }` formats each event
  into an Indonesian message and sends it.
- Test impl: a recording reporter (collects events) and/or a no-op.

**Concise message set:**
- Market entry: `⏳ Entry {coin} dikirim…`
- Limit entry: `⏳ Limit {coin} @ ${price} dipasang, menunggu fill…`
- Full fill + bracket: `✅ {coin} terisi (size {size}) — SL/TP terpasang.`
- Partial fill: `⚠️ Partial fill {coin}: bracket dipasang di size {size}.`
- Timeout: `❌ Limit {coin} tak terisi dalam {secs}s — dibatalkan, tidak ada posisi.`

The confirm handler keeps journalling as today; the final success/failure text is now
driven by the events (the standalone `✅ Executed …` / `❌ Execution failed: …` lines
are replaced by reporter output). The `Executing {coin}…` placeholder edit stays.

### Component 2 — Fill monitor (close detection, polling)

New module `src/monitor.rs`. A background tokio task spawned inside `telegram::run`
(which already owns `bot`, `config`, `journal`, and `exchange`).

```
loop every MONITOR_POLL_SECS (default 30):
    fills = exchange.fills_detailed()              // oldest-first; has oid, px, sz, closed_pnl, dir, time_ms
    for fill in fills where is_closing(fill):       // dir contains "Close" OR closed_pnl != 0
        key = fill_key(fill)                        // "{oid}:{time_ms}:{px}:{sz}"
        if journal.is_fill_seen(key): continue
        if baseline_pending: mark seen, continue    // first-ever poll on a fresh DB → silence history
        label = classify + PnL → notify all allowed_user_ids
        journal.mark_fill_seen(key)
```

- **`is_closing(fill)`** — pure helper: `fill.dir` contains "Close" (case-insensitive)
  or `fill.closed_pnl != 0.0`.
- **Dedup** — new SQLite table `seen_fills(fill_key TEXT PRIMARY KEY)`. `fill_key` =
  `format!("{}:{}:{}:{}", oid, time_ms, px, sz)`.
- **Baseline anti-spam** — on startup, if `seen_fills` is empty (brand-new DB), the
  first poll inserts every current closing fill as *seen* WITHOUT notifying. A restart
  on a populated DB does NOT baseline, so closes that happened while the bot was down
  are still notified (desired). Implemented by checking `seen_fills` row count once at
  task start.
- **Target chat** — notifications go to every `config.allowed_user_ids` as
  `ChatId(user_id)` (private-chat id == user id).
- **Failure isolation** — a poll error (network/SDK) is logged via `tracing::warn!` and
  the loop continues; the monitor never panics the bot.

### Component 3 — Leg labelling + journal extensions

To name the leg, the bracket prices must be persisted. `journal.rs` stores `stop_loss`
but not TP prices.

- **Migration** (idempotent, like existing ones): add column `tp_prices TEXT` to
  `trades` — a JSON array of TP prices (e.g. `"[1.70,2.00]"`). `record()` serializes
  `plan.take_profits` prices.
- **Query** — `latest_bracket_for_coin(coin) -> Option<Bracket { stop_loss, take_profits: Vec<f64> }>`
  returns the most recent trade's SL + TP prices for that coin.
- **Pure classifier** — `classify_close_fill(px, &Bracket) -> CloseLabel` where
  `CloseLabel ∈ { StopLoss, TakeProfit(n) }`. Picks the bracket leg whose price is
  closest to `px` (by relative distance); SL vs TP determined by which set the nearest
  price belongs to. TP index `n` is 1-based in TP order.
- **Message** (PnL from `fill.closed_pnl`):
  - `🎯 TP1 kena — {coin} {+$pnl}` (pnl ≥ 0)
  - `🛑 SL kena — {coin} {−$pnl}` (pnl < 0)
  - Fallback when no journaled bracket found: `📕 Posisi {coin} ditutup — {±$pnl}`.

## Data flow

```
Confirm tap
  → confirm handler edits "Executing TAO…"
  → execute_plan(exchange, plan, use_limit, timeout, reporter=TelegramReporter)
       set_leverage → place_entry → reporter.EntrySubmitted
       (limit) poll position_size → reporter.Filled{partial?} | reporter.FillTimeout
       place SL + TPs → reporter.BracketArmed
  → handler journals trade (now incl. tp_prices)

Background (every 30s, independent of any chat)
  fill monitor → fills_detailed → new closing fills
       → latest_bracket_for_coin → classify_close_fill → message
       → send to all allowed_user_ids → mark_fill_seen
```

## Error handling

- `ProgressReporter::report` is best-effort: a Telegram send error inside the reporter
  is logged and swallowed (it must not abort execution mid-bracket).
- Monitor poll errors are logged and the loop continues.
- A fill with no matching journaled bracket still notifies via the generic fallback.
- DB write failures in `mark_fill_seen` are logged; the fill may re-notify next poll
  (acceptable — at-least-once, dedup is best-effort on DB availability).

## Testing (TDD)

Unit-testable pure logic (written test-first):
- `is_closing(fill)` — close vs open dir, zero/non-zero pnl.
- `fill_key(fill)` — stable composite key.
- `classify_close_fill(px, bracket)` — nearest-leg matching: exact SL, exact TP1,
  exact TP2, between-prices tie-break, single-TP bracket.
- `Journal`: `tp_prices` round-trips; `latest_bracket_for_coin` returns newest;
  `is_fill_seen`/`mark_fill_seen`; baseline (empty table) detection.
- `execute_plan` with a recording reporter (using `MockExchange`): asserts event
  sequence for market, limit-filled, partial, and timeout paths.

Message formatting helpers are pure (take values, return `String`) and unit-tested.

## Configuration

- `MONITOR_POLL_SECS` (default `30`) — new optional env var, parsed in `config.rs`.

## Files touched

- `src/telegram.rs` — `ExecutionEvent`, `ProgressReporter`, `TelegramReporter`,
  `execute_plan` signature + event calls, confirm handler wiring.
- `src/monitor.rs` — **new**: poll loop, `is_closing`, `fill_key`, notification send.
- `src/journal.rs` — `tp_prices` migration + serialization, `seen_fills` table,
  `latest_bracket_for_coin`, `classify_close_fill` (or co-locate classifier in monitor),
  seen-fill methods.
- `src/config.rs` — `monitor_poll_secs`.
- `telegram::run` — spawn the monitor task.
- `.env.example` / `README.md` — document `MONITOR_POLL_SECS` and the new notifications.
```
