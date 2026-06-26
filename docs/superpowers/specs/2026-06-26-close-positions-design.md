# Close Positions — Design

**Date:** 2026-06-26
**Status:** Approved (pending spec review)

## Overview

Add manual position-closing commands to the Telegram bot. Today the bot can open
positions (market/limit/trigger entries) and attach SL/TP brackets, but there is
**no way to close an open position from Telegram** — the user must do it on the
Hyperliquid UI. This feature adds two commands behind a confirmation button:

- `/closeall` — flatten every open position at once.
- `/close <COIN>` — flatten a single named position.

### Why not an auto take-profit feature?

The original ask was an "auto-close at +$2 profit" loop for fast scalping in a
sideways market. After brainstorming we rejected it (YAGNI):

- The bot **already** supports per-trade take-profit via the existing SL/TP
  bracket system. A user wanting tighter profit-taking simply sets a tighter TP
  per signal — no new feature needed.
- An aggregate "basket" auto-close (close all when combined PnL crosses a
  threshold) force-closes losing positions alongside winners, which is harmful in
  a mean-reverting sideways market.
- A polling auto-close uses market (taker) orders, whose fees meaningfully erode a
  small profit target.

The only genuinely missing primitive is **closing a position**, which this spec
delivers. Auto take-profit can be layered on later if real usage demands it.

## Scope

**In scope:**
- `/closeall` and `/close <COIN>` commands with inline-button confirmation.
- A market reduce-only close primitive on the `Exchange` trait.
- Cancelling a position's resting orders (orphaned SL/TP) as part of closing it.
- Best-effort, per-position execution with a result summary.

**Out of scope (explicitly):**
- Any automatic / scheduled / threshold-based closing.
- Partial closes (always flattens the full position size).
- Closing spot balances (perp positions only).

## Architecture

The feature reuses three existing patterns verbatim:

1. **Manual command dispatch** in `on_message` (`src/telegram.rs`) — commands are
   matched by their first word (cf. the `/account` and `/stats` handlers), not via
   a `BotCommands` enum.
2. **Confirm/cancel inline keyboards** — cf. `confirmation_keyboard` and the
   `CB_CANCEL` / `CB_CANCEL_FILL_PREFIX` callback handling in `on_callback`.
3. **The `Exchange` trait** (`src/hyperliquid/mod.rs`) as the only network-touching
   seam, with a `MockExchange` test double mirroring every method.

### Component 1 — `Exchange` trait additions

Two new async methods on `trait Exchange`, with real + mock implementations.

```rust
/// Closes (flattens) the full open position for `coin` with a market
/// reduce-only order. `is_buy` is the CLOSING side: true to close a short,
/// false to close a long. Returns the order result (filled + avg price).
async fn close_position(&self, coin: &str, size: f64, is_buy: bool)
    -> anyhow::Result<OrderResult>;

/// Cancels every resting order for `coin` (e.g. orphaned SL/TP triggers).
/// Returns the number of orders cancelled. Best-effort: a per-order cancel
/// failure is logged and skipped, not propagated.
async fn cancel_orders_for_coin(&self, coin: &str) -> anyhow::Result<usize>;
```

**Real implementation notes:**
- `close_position`: market reduce-only order. Prefer the SDK's `market_close`
  helper if it covers reduce-only flattening; otherwise send a `market_open`-style
  IOC order with `reduce_only: true` and the closing side. Reuse the existing
  `parse_order_response` mapper. 1% slippage, consistent with `place_entry`.
- `cancel_orders_for_coin`: fetch `info.open_orders(address)` (already used by
  `open_order_count`), filter by coin (case-insensitive), and call the existing
  `cancel_order(coin, oid)` for each. Count successes.

**Mock implementation:**
- `close_position` records `(coin, size, is_buy)` into a new `closes: Mutex<Vec<..>>`
  field and returns a filled `OrderResult`.
- `cancel_orders_for_coin` records the coin and returns the count of seeded
  `open_orders` matching it (reuse the existing `open_orders` field), pushing each
  matched entry into the existing `cancels` vec for assertion.

### Component 2 — Commands (`on_message`)

Both commands are async (need `exchange.positions()`), so they are handled inline
in `on_message` alongside `/account`, BEFORE the synchronous `command_response`
fallthrough.

**`/closeall`:**
1. Fetch `positions()`. On error → reply with the error text.
2. If empty → reply "Tidak ada posisi terbuka." and return.
3. Else → send a confirmation message rendered by `render_close_all_prompt`
   (lists each position with direction/size/uPnL and the combined uPnL), with a
   keyboard: `[✅ Tutup semua]` (`CB_CLOSE_ALL`) and `[❌ Batal]` (`CB_CANCEL`).

**`/close <COIN>`:**
1. Parse the coin argument. If missing → reply usage hint "/close <COIN>".
2. Fetch `positions()`, find the one matching the coin (case-insensitive).
3. If not found → reply "Tidak ada posisi <COIN> terbuka."
4. Else → confirmation message via `render_close_one_prompt`, keyboard:
   `[✅ Tutup <COIN>]` (`CB_CLOSE_ONE_PREFIX` + coin) and `[❌ Batal]`.

New callback constants:
```rust
pub const CB_CLOSE_ALL: &str = "close_all";
pub const CB_CLOSE_ONE_PREFIX: &str = "close_one:";
```
The existing `CB_CANCEL` handler already edits the message to "Cancelled." — the
Batal button reuses it (it harmlessly `store.remove`s a non-existent key).

### Component 3 — Execution (`on_callback`)

A shared async helper performs the close so both buttons share one path:

```rust
async fn execute_close<E: Exchange>(
    exchange: &E,
    targets: &[OpenPosition],
) -> Vec<CloseOutcome>;
```

For each target position:
1. `cancel_orders_for_coin(coin)` first — clears resting SL/TP so they don't
   linger on a flat position. Failure is logged, does not abort the close.
2. `close_position(coin, size, is_buy)` where `is_buy = direction == "short"`.
3. Record a `CloseOutcome { coin, ok: bool, error: Option<String> }`.

Best-effort: one position failing does not stop the others. After execution, edit
the confirmation message in place with a summary rendered by
`render_close_result` (e.g. "✅ BTC ditutup · ✅ ETH ditutup · ⚠️ SOL gagal: …").

**Callback wiring:**
- `CB_CLOSE_ALL` → fetch current `positions()` again (freshness; the book may have
  moved since the prompt), close all, edit message with the result.
- data starting with `CB_CLOSE_ONE_PREFIX` → strip the coin, fetch positions, find
  the match, close just that one, edit message with the result.

Re-fetching positions at confirm time (rather than trusting the prompt snapshot)
avoids closing a stale size if a position changed between prompt and confirm.

## Data Flow

```
/closeall ─▶ on_message ─▶ positions() ─▶ render_close_all_prompt + keyboard
                                                      │
                                              user taps ✅
                                                      ▼
CB_CLOSE_ALL ─▶ on_callback ─▶ positions() (fresh) ─▶ execute_close
                                                          │ per position:
                                                          │  cancel_orders_for_coin
                                                          │  close_position (mkt RO)
                                                          ▼
                                              render_close_result ─▶ edit message
```

## Error Handling

- **Fetch failures** (`positions()`): reply with the error, do nothing else.
- **Empty book**: friendly "no open positions" message, no keyboard.
- **Per-position close failure**: caught, recorded in `CloseOutcome`, surfaced in
  the result summary with a ⚠️ marker; other positions still close.
- **Orphan-cancel failure**: logged (`tracing::warn!`), does not block the close —
  a reduce-only order on a flat position is harmless even if left behind.
- **Expired/empty callback** (`regular_message()` None): return `Ok(())` per the
  existing on_callback convention.

## Testing

Following the repo's pattern of pure render functions + mock-driven async tests:

**Pure unit tests (no I/O):**
- `render_close_all_prompt` lists every position and the correct combined uPnL.
- `render_close_one_prompt` shows the single coin, size, direction, uPnL.
- `render_close_result` marks successes and failures distinctly.
- Closing-side logic: a long yields `is_buy == false`, a short `is_buy == true`.

**Mock-driven async tests (`MockExchange`):**
- `execute_close` on a seeded long position calls `cancel_orders_for_coin` then
  `close_position` with the right `(coin, size, is_buy)`.
- A position with seeded resting orders has them cancelled (assert via the mock's
  `cancels` vec).
- Best-effort: when one position's `close_position` errors (seed a failing coin),
  the others still close and the outcome list flags the failure.
- `/close UNKNOWN` (no matching position) produces the not-found path.

## Files Touched

- `src/hyperliquid/mod.rs` — trait methods, real impl, mock impl + fields, tests.
- `src/telegram.rs` — command handlers, callback constants + handlers, render
  functions, execute_close helper, tests.
- `WELCOME_TEXT` / `/help` text — document the two new commands.
```
