# Design: `/account` Telegram command (live balance + open positions)

**Date:** 2026-06-18
**Project:** agentic-hyperliquid (Telegram trading bot)
**Status:** Approved design — ready for implementation plan

## Overview

Add a `/account` command that shows the live account state on demand: current
equity (balance) and all currently open perp positions, plus how much of the
daily-risk budget has been used today. This complements `/stats` (which shows
only realized historical performance) and reuses the existing equity and
daily-cap building blocks.

## Background (verified in code)

- `Exchange::equity()` already returns the current balance and handles both
  account modes: unified reads the spot USDC balance, non-unified reads the
  perp `account_value`.
- `Exchange::position_size(coin)` already reads
  `info.user_state(address).asset_positions` — the **same** source works for
  both unified and non-unified accounts, so listing all positions needs no
  mode-specific branching.
- The SDK `PositionData` exposes: `coin`, `szi` (signed size — sign gives
  long/short), `entry_px: Option<String>`, `unrealized_pnl`, `leverage`
  (`Leverage { value: u32, .. }`), `liquidation_px: Option<String>`,
  plus `position_value`, `margin_used`, `return_on_equity` (not shown).
- The daily cap (`max_daily_risk_pct` in `Settings`) and used-today
  (`journal.risk_used_since(start_of_utc_day)`) already exist.

## Feature

### New domain type + Exchange method

Add to `src/hyperliquid/mod.rs`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPosition {
    pub coin: String,
    pub is_long: bool,
    pub size: f64,            // absolute size (coin units)
    pub entry_px: f64,        // 0.0 when the SDK reports none
    pub unrealized_pnl: f64,
    pub leverage: u32,
    pub liquidation_px: Option<f64>,
}
```

New trait method:

```rust
/// All currently open perp positions (zero-size entries skipped).
/// Reads user_state().asset_positions; works in unified and non-unified modes.
async fn open_positions(&self) -> anyhow::Result<Vec<OpenPosition>>;
```

Real impl: map each `asset_positions[i].position`:
- parse `szi` → `is_long = szi > 0.0`, `size = szi.abs()`; **skip** entries with
  `size == 0.0`.
- `entry_px`: parse `entry_px` when `Some`, else `0.0`.
- `unrealized_pnl`: parse, fall back to `0.0` on parse error (display field).
- `leverage`: `position.leverage.value`.
- `liquidation_px`: parse when `Some`, else `None`.

`MockExchange` impl: return a configurable `Vec<OpenPosition>` field (defaulting
empty) so the handler/render path is testable without the network.

### Render function

Add to `src/telegram.rs` a plain-text renderer (sent WITHOUT `parse_mode`):

```rust
pub fn render_account(
    equity: f64,
    positions: &[OpenPosition],
    used_today: f64,
    cap_pct: Option<f64>,
) -> String
```

Output shape:

```
💰 Account
Equity: $1,234.56
Daily risk used: $12.30 / $61.70 (5%)      // line omitted when cap_pct is None

Open positions (2):
  BTC   LONG   0.05  @ $61200.00  uPnL $+45.20  3x  liq $52100.00
  ETH   SHORT  1.2   @ $3410.00   uPnL $-12.80  2x  liq -

// when empty:
Flat — no open positions.
```

- `cap_amount` for the daily-risk line = `equity × cap_pct / 100`.
- `liq -` when `liquidation_px` is `None`.
- Plain text only (no MarkdownV2), matching `render_stats`/`render_settings`.

### Handler

Intercept `/account` in `on_message`, alongside the existing `/stats`
interception (it needs the async exchange + journal + settings, so it cannot go
through the pure `command_response`). Order does not matter relative to
`/settings`/`/set` as long as it is before the `command_response` fallback.

Flow:
1. Fetch `equity()` and `open_positions()` (errors → reply a clear message,
   return).
2. Read `cap_pct` from settings (`max_daily_risk_pct`), copied out of the lock.
3. Compute `used_today = journal.risk_used_since(start_of_utc_day(now))`
   (mirrors the daily-cap check; `0.0` on error).
4. `bot.send_message(chat_id, render_account(...))` as plain text.

### Help text

Add `/account` to `WELCOME_TEXT` (and thus `/help`).

## Error handling

- `equity()` or `open_positions()` failure → single plain-text message
  (`"Could not fetch account state: {error}"`), no partial card.
- Per-position parse failures: `unrealized_pnl`/`entry_px` fall back to `0.0`,
  `liquidation_px` to `None`; a malformed `szi` (the one field that defines the
  position) propagates as an error from `open_positions()` rather than emitting
  a bogus zero-size row.

## Testing

- `render_account`: with multiple positions (long + short, with/without liq),
  flat (empty → "Flat" line), cap present vs `None` (daily-risk line shown vs
  omitted), sign formatting of uPnL.
- `OpenPosition` mapping: a `MockExchange` (or a small pure mapper) verifies
  signed-`szi` → `is_long`/`size`, zero-size skipped, missing `entry_px`/`liq`
  handled.
- Existing suite stays green.

## Out of scope (YAGNI)

- Spot balances breakdown beyond the single equity number.
- Position notional / ROE / funding columns (available but not shown).
- Auto-refreshing or push updates — `/account` is on-demand pull only.
