# Design: Telegram-configurable settings + entry sizing modes

**Date:** 2026-06-18
**Project:** agentic-hyperliquid (Telegram trading bot)
**Status:** Approved design — ready for implementation plan

## Overview

Two related features:

1. **Entry sizing modes.** Add two alternatives to the existing risk-based
   sizing: size a position as a fixed percentage of account equity, or as a
   fixed USD notional. The active mode is a global setting.
2. **Runtime configuration via Telegram.** Make the key trading parameters
   editable from chat (`/settings` + `/set`) and persist them across restarts,
   instead of being read-only environment variables loaded once at startup.

Settings are **global** (single trader on testnet; the allowlist gates access).
Environment variables become the *seed/default* values for first boot.

## Feature 1: Entry sizing modes

### Modes

The position size (in coin units, then floored to the asset's `sz_decimals`) is
computed per the active mode:

| Mode | Size formula | Requires |
|------|--------------|----------|
| `RiskBased` (existing) | `equity × risk_pct/100 ÷ stop_distance` | valid SL on correct side |
| `PercentBalance` | `(equity × entry_pct/100) ÷ entry_price` | — |
| `FixedUsd` | `entry_fixed_usd ÷ entry_price` | — |

`stop_distance = |entry − stop_loss|`.

### Rules common to all modes

- **Leverage** is still chosen by the risk profile (Conservative / Moderate /
  Aggressive). `margin = notional ÷ leverage`; `notional = size × entry_price`.
- **SL and TP** brackets always come from the parsed signal card.
- **Minimum notional ($10)** is still enforced. A fixed `$5` entry is rejected
  with a clear error (`order notional below $10 minimum`).
- **`plan.risk_amount` becomes the actual risk** = `size × stop_distance` for
  *all* modes (currently it stores the target `equity × risk_pct/100`). This
  makes the displayed "Risk: $X" the true loss-at-stop and feeds the daily cap
  accurately. For `RiskBased` this equals the old value modulo size rounding.
- `RiskBased` keeps its existing validations (`InvalidStopSide`,
  `ZeroStopDistance`). `PercentBalance` / `FixedUsd` do not require a valid stop
  side to *size* the position, but the SL bracket still uses the card's SL — if
  the SL is on the wrong side the existing bracket logic / warnings apply
  (no behavior change to bracket placement).

### Implementation sketch (`sizing.rs`)

- Add `pub enum EntryMode { RiskBased, PercentBalance, FixedUsd }`.
- `SizingInput` gains the fields needed by the chosen mode: `entry_mode`,
  `entry_pct`, `entry_fixed_usd` (alongside the existing `risk_pct`). These come
  from the runtime `Settings`.
- `build_plan` branches on `entry_mode` to compute `raw_size`, then shares the
  existing floor / min-notional / leverage / bracket / warning logic.
- Set `risk_amount = size × stop_distance` after sizing.

## Feature 2: Daily risk cap across all modes

The daily cap (`MAX_DAILY_RISK_PCT`) applies in **every** mode, using the
actual `plan.risk_amount`. A confirmed trade whose `risk_amount` would push the
day's cumulative risk past `max_daily_risk_pct × equity` is rejected — same
mechanism as today (`risk::within_daily_cap`, enforcement in `telegram.rs`).
No change to `risk.rs`; the only change is that the `new_risk` passed in is now
the actual computed risk for non-risk-based modes too.

## Feature 3: Runtime settings

### Settings model

A `Settings` struct holding the editable parameters:

| Key (for `/set`) | Type | Seed env var | Notes |
|------------------|------|--------------|-------|
| `entry_mode` | enum | (default `RiskBased`) | set via `/settings` buttons |
| `risk_pct` | f64 | `RISK_PCT` | used by `RiskBased` |
| `entry_pct` | f64 | (new, default 10.0) | used by `PercentBalance` |
| `entry_fixed_usd` | f64 | (new, default 50.0) | used by `FixedUsd` |
| `max_daily_risk_pct` | Option<f64> | `MAX_DAILY_RISK_PCT` | empty/0 disables cap |
| `leverage_conservative` | u32 | `LEVERAGE_CONSERVATIVE` | |
| `leverage_moderate` | u32 | `LEVERAGE_MODERATE` | |
| `leverage_aggressive` | u32 | `LEVERAGE_AGGRESSIVE` | |

### Persistence

- New `settings` table in the existing `trades.db` (key-value: `key TEXT PRIMARY
  KEY, value TEXT`), created idempotently like the `Journal` schema and sharing
  the same SQLite database file.
- A `SettingsStore` (mirrors `Journal`'s `Mutex<Connection>` pattern; may reuse
  the same connection wrapper) loads all rows on startup. **Seeding:** for any
  key absent from the table, the value is taken from the corresponding env var
  (or the hardcoded default) and written to the table. Thereafter the DB is the
  source of truth — env changes do not override stored settings.
- Held in `BotContext` as `Arc<Mutex<Settings>>`. Every `/set` or mode-toggle
  updates the in-memory struct **and** writes through to the table in the same
  operation.

### Runtime wiring

- `BotContext` gains `settings: Arc<Mutex<Settings>>`.
- Sizing call sites (`process_setups`, `recompute_plan`, callback handlers) read
  the current settings under the lock and pass `entry_mode`, `risk_pct`,
  `entry_pct`, `entry_fixed_usd`, and the leverage map into `SizingInput`.
- `Config` keeps non-tunable startup values (telegram token, keys, network,
  account address, unified flag, LLM/vision config). The tunable trading
  parameters move to / are mirrored by `Settings`.

### Telegram UX (hybrid)

- **`/settings`** — render current values, plus an inline keyboard with three
  entry-mode buttons (`Risk-based`, `% Balance`, `Fixed USD`) showing `✓` on the
  active mode. Tapping a button switches the mode and persists it, then re-renders.
  New callback data constants, e.g. `entry_mode:risk|percent|fixed`.
- **`/set <key> <value>`** — edit a numeric setting, e.g. `/set entry_pct 10`,
  `/set entry_fixed_usd 50`, `/set max_daily_risk_pct 8`, `/set risk_pct 2`.
  - Validate: known key, parseable number, sane range (percentages `> 0` and
    `≤ 100` where applicable; leverage `≥ 1`; `max_daily_risk_pct` accepts empty
    / `0` to disable).
  - On success: confirmation message echoing the new value. On error: a message
    listing valid keys / the range that was violated.
- Help text (`WELCOME_TEXT` / `/help`) updated to mention `/settings` and `/set`.

## Error handling

- Unknown `/set` key or unparseable value → descriptive message, no state change.
- Out-of-range value → message stating the allowed range, no state change.
- `FixedUsd` / `PercentBalance` below $10 notional → existing
  `SizingError::BelowMinSize` surfaced to the user with the min-notional message.
- SQLite write failure on persist → log the error and report failure to the
  user; in-memory change is rolled back so memory and DB stay consistent.

## Testing

- **`sizing.rs`**: unit tests per mode — `PercentBalance` and `FixedUsd` compute
  the expected size/notional; `risk_amount == size × stop_distance`; min-notional
  rejection for a too-small fixed entry; `RiskBased` regression unchanged.
- **Settings parsing**: `/set` parser tests — valid keys, unknown key, invalid
  number, out-of-range, and the empty/0 disable path for `max_daily_risk_pct`.
- **Persistence**: write-through test — set a value, reopen the store against the
  same DB, confirm it loads; seeding test — empty table seeds from env/defaults.
- **Daily cap**: cap enforced using actual computed risk under a non-risk-based
  mode (rejects when cumulative actual risk exceeds the cap).

## Out of scope (YAGNI)

- Per-user settings (single global trader for now).
- Per-trade mode selection via buttons on the confirmation card (mode is global).
- Editing non-trading config (keys, network, LLM models) via Telegram.
