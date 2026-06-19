# P&L Push Monitoring — Design

**Date:** 2026-06-19
**Status:** Approved (pending spec review)

## Overview

`/account` already shows each open position's unrealized PnL on demand, but it has no
**aggregate total** and is pull-only. This feature adds a background task that
periodically **pushes** a running-PnL summary (total + per-position) to the user while any
position is open, and adds the missing total line to `/account`.

Pre-existing positions are tracked automatically: the summary is built from
`exchange.positions()`, which returns every open position regardless of how it was opened
(manual, market, limit, or a future trigger entry).

## Goals / Non-goals

**Goals**
- Periodically push a running-PnL summary (total unrealized PnL + per-position lines) to
  all allowlisted users, on a configurable interval, only while positions are open.
- Add a TOTAL unrealized-PnL line to `/account`.

**Non-goals**
- No price-threshold/alert triggers (every-interval push only; YAGNI).
- No per-position push toggles.
- No change to how positions are fetched or to realized-PnL/close notifications (those are
  the separate fill-monitor feature).

## Architecture

```
Background run_pnl_monitor (own spawned task, every pnl_push_secs):
  if pnl_push_secs == 0: idle (feature disabled)
  positions = exchange.positions()
  if positions is empty: skip (no spam when flat)
  else: build render_pnl_summary(equity, positions) → send to all allowed_user_ids
```

A separate spawned task from the fill/trigger monitors (single responsibility), living in
`src/monitor.rs`.

## Components

### 1. Shared formatter — `render_pnl_summary` (`src/telegram.rs`)
- `pub fn render_pnl_summary(equity: f64, positions: &[OpenPosition]) -> String`:
  header + a **TOTAL uPnL** line (sum of `position.unrealized_pnl`) + one line per position
  (coin, direction, size, mark, uPnL) — the same per-position layout already used by
  `render_account`.
- Refactor `render_account` to compute and show the same TOTAL uPnL line (reuse a small
  helper so the per-position rows and the total are formatted identically in both places).

### 2. Monitor — `run_pnl_monitor` (`src/monitor.rs`)
- Spawned in `telegram::run` with `bot`, `exchange`, `allowed_user_ids`, and the interval
  source (`pnl_push_secs`).
- Each tick: read interval; if `0`, sleep a default and skip (disabled). Else poll
  `positions()` + `equity()`; if non-empty, send `render_pnl_summary` to every
  `ChatId(user_id)`.
- Never panics: poll/send errors logged via `tracing` and swallowed.
- Reads the interval from the live `Settings` each tick so `/set pnl_push_secs` takes
  effect without a restart (copy the value out of the lock before awaiting).

### 3. Settings — `pnl_push_secs` (`src/settings.rs`, `src/telegram.rs`)
- New `Settings.pnl_push_secs: u64` (default `900` = 15 min; `0` disables), seeded from
  config, validated (any `u64`; `0` allowed = disabled) via `/set`, persisted/loaded, shown
  in `render_settings`. Follows the `entry_fill_timeout_secs` pattern, except `0` is a
  valid (disabling) value rather than rejected.
- New config/env `PNL_PUSH_SECS` (default 900).

## Notifications
- Push (while positions open):
  ```
  📊 Running P&L
  Total uPnL: $+12.34
    SOL  LONG  0.21 @ $68.53  mark $69.10  uPnL $+0.12  3x
    ...
  ```
- Nothing sent when flat.

## Error handling
- Loop swallows+logs all errors; a failed poll or send never stops the loop.
- Disabled (`pnl_push_secs == 0`) is a normal state, not an error.

## Testing (TDD)
- Pure: `render_pnl_summary` — total is the sum of per-position uPnL (incl. mixed
  +/- positions), per-position lines present, sensible output for a single position;
  `render_account` shows the same total. Settings `pnl_push_secs` validation (accepts 0 and
  positive; round-trips through persist/load).
- The monitor loop is integration glue (no live Bot in unit tests); verified by build +
  suite green, per the existing codebase convention. The skip-when-flat and
  disabled-when-zero decisions are simple enough to cover via a pure predicate if it aids
  clarity, else asserted by reading the loop.

## Files touched
- `src/telegram.rs` — `render_pnl_summary`, `render_account` total line, monitor spawn,
  settings display.
- `src/monitor.rs` — `run_pnl_monitor`.
- `src/settings.rs` — `pnl_push_secs`.
- `src/config.rs` — `PNL_PUSH_SECS`.
- `.env.example` / `README.md` — document `PNL_PUSH_SECS` and the running-PnL push.

## Relationship to the trigger-entry feature
Independent. Both add a spawned monitor task and a `/set`-able interval, and both are built
on the existing `Settings` + `monitor.rs` patterns. They share no state. Implementation
order: trigger-entry first, then this.
