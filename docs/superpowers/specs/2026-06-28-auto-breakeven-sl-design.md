# Auto-Breakeven Stop-Loss — Design

**Date:** 2026-06-28
**Status:** Approved (pending implementation plan)

## Problem

Realized `/stats` show a positive edge (44% win rate, net positive — winners are
larger than losers) dragged down by trades that move into profit and then reverse
into the stop-loss. The user keeps "getting stopped out" on setups that were
briefly green. Cutting take-profit closer would invert the reward:risk and destroy
the edge (and increase fee drag). The correct lever is to stop giving back open
profit: once a position has earned enough, move its stop-loss to breakeven so a
reversal closes flat instead of at a loss.

## Goal

When an open position's profit reaches a configurable multiple of its initial risk
(R-multiple), automatically move its stop-loss to just past the entry price
(breakeven plus a small fee buffer), sized to the position that is actually held.
Applies to **all** positions — auto-scalp (single TP) and manual multi-TP setups
alike. Disabled by default; opt-in per the user's risk appetite.

## Non-Goals

- Trailing the stop beyond breakeven (a future enhancement).
- Partial-TP-driven breakeven (the trigger is profit threshold, not a TP fill).
- Changing take-profit orders. The breakeven move touches the stop-loss only.

## Decisions (locked during brainstorming)

1. **Trigger metric:** R-multiple. Move when
   `profit_distance >= trigger_r * risk_distance`, where
   `risk_distance = |entry - stop_loss|`. Adaptive to each setup's stop width.
2. **Scope:** every open position, not just multi-TP setups.
3. **New stop placement:** entry plus a fee buffer (above entry for longs, below
   for shorts), so a breakeven stop closes at ~$0 net after round-trip fees rather
   than a small loss.
4. **Default off.** Behaviour is unchanged until the user enables it.

## Architecture

A new background loop, `run_breakeven_monitor`, mirroring the existing
`run_fill_monitor` / `run_pnl_monitor` shape (single responsibility, never panics,
logs and continues on error). It runs at the fill-monitor cadence (`poll_secs`,
default 30s) — fast enough for a stop move, and far quicker than the 900s
`pnl_push_secs` P&L cadence.

Each tick:

1. Read live settings; if `breakeven_enabled` is false, idle and continue.
2. Poll `positions()`.
3. For each open position, look up the most recent journaled trade for that coin
   (the trade live now) to obtain `stop_loss`, `direction`, `sl_order_id`, and the
   `breakeven_moved` flag.
4. **Skip** the position when any of: `breakeven_moved` is already set;
   `sl_order_id` is unknown; or `risk_distance` is effectively zero.
5. Compute `risk_distance = |position.entry_px - stop_loss|` (using the position's
   actual average fill) and `profit_distance` (mark vs entry, direction-aware).
6. If `profit_distance >= breakeven_trigger_r * risk_distance`:
   1. Compute `be_price = breakeven_price(direction, entry_px, breakeven_buffer_pct)`.
   2. **Guard:** `be_price` must be a valid stop relative to mark (below mark for a
      long, above for a short). If not (only when `R*risk < buffer`, rare), log and
      skip.
   3. Place the **new** stop-loss trigger at `be_price`, reduce-only, sized to the
      current position size → obtain the new order id.
   4. **Then** cancel the **old** stop-loss by `sl_order_id`.
   5. Persist `sl_order_id = new_oid` and `breakeven_moved = 1` for that trade.
   6. Notify the user: `🛡️ SL {coin} digeser ke breakeven`.

**Ordering — place new before cancelling old:** the position is never left without
a stop. If the cancel fails, two reduce-only stops rest briefly; the breakeven one
is nearer the mark and triggers first, and reduce-only prevents any over-close. Any
leftover is swept by `cancel_orders_for_coin` when the position finally closes.

## Pure core (unit-tested, no I/O)

```rust
/// True when profit has reached `trigger_r` times the initial risk distance.
fn reached_breakeven_threshold(
    direction: Direction, entry: f64, stop_loss: f64, mark: f64, trigger_r: f64,
) -> bool;

/// Breakeven stop price: entry nudged past itself by `buffer_pct`% to cover fees.
/// Long → entry * (1 + buffer_pct/100); short → entry * (1 - buffer_pct/100).
fn breakeven_price(direction: Direction, entry: f64, buffer_pct: f64) -> f64;
```

The monitor loop is a thin wiring layer over these two functions plus exchange and
journal I/O.

## Data changes (journal)

Two columns added to the `trades` table via the existing additive
`ALTER TABLE ADD COLUMN` migration pattern:

- `sl_order_id INTEGER` — the order id of the live stop-loss. Captured when the
  bracket is placed (`place_bracket` in `telegram.rs` and the trigger-entry arming
  path in `monitor.rs`), where `place_trigger` already returns an `OrderResult`
  carrying `order_id` (currently discarded). Updated when the breakeven move
  re-places the stop.
- `breakeven_moved INTEGER NOT NULL DEFAULT 0` — idempotency flag so the move runs
  exactly once per position.

A small journal read returns `(stop_loss, direction, sl_order_id, breakeven_moved)`
for the most recent trade of a coin; a small journal write updates
`sl_order_id`/`breakeven_moved` for that trade.

## Settings (runtime-tunable via `/set`, persisted, default-safe)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `breakeven_enabled` | bool | `false` | master toggle |
| `breakeven_trigger_r` | f64 | `1.0` | move once profit ≥ this many R |
| `breakeven_buffer_pct` | f64 | `0.1` | fee buffer past entry (~round-trip taker) |

Validation: `breakeven_trigger_r > 0`; `breakeven_buffer_pct >= 0`. Added to
`VALID_KEYS`, `apply_setting`, `persist`, `load`, `from_config`, `sample`, and the
`/settings` render text (with a `/set` example).

## Error handling

- `risk_distance ≈ 0` → skip (guards division-by-zero and nonsensical setups).
- `be_price` on the wrong side of mark → log and skip.
- Position closed between poll and action → `place_trigger` (reduce-only) fails →
  log; `breakeven_moved` stays false (a retry is harmless, or the position is gone).
- New-stop placement failure → log, do not cancel the old stop, leave flag false so
  the next tick retries with the original protection still in place.

## Testing

**Unit (pure):**
- `reached_breakeven_threshold`: long & short, just-below vs just-at vs above
  threshold, `trigger_r` other than 1.0, degenerate `risk_distance = 0`.
- `breakeven_price`: long above entry, short below entry, buffer scales correctly.
- Settings: `breakeven_trigger_r` rejects ≤ 0; `breakeven_buffer_pct` rejects < 0;
  persist/reload round-trips all three keys and the two new journal columns.

**Integration (MockExchange):**
- Position past threshold → new stop placed at breakeven, old stop cancelled,
  `breakeven_moved` set, one notification.
- Position below threshold → no order activity.
- `breakeven_moved` already set → no-op.
- `breakeven_enabled` false → loop idles, no order activity.

## Out-of-scope / future

- Trailing stop beyond breakeven.
- Promoting the fee buffer to a per-leverage or per-coin value.
