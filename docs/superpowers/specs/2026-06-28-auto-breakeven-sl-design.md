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

## Mechanism: exchange-truth (no journal dependency)

The auto-scalp path (`api/mod.rs`) does **not** journal its trades, and the
stop-loss order id is not persisted anywhere. So the monitor reads ground truth
from the exchange instead of the journal: it lists the resting orders for each
position, finds the stop-loss trigger, and uses its trigger price as the current
stop and its order id as the cancel target. This covers **every** position
(auto-scalp and manual) with no schema change and no persisted idempotency flag.

**Idempotency without a flag:** a long's original stop sits *below* entry; once
moved to breakeven it sits *at/above* entry. So "already moved" is detected purely
from state — if the resting stop is already on the profitable side of entry, skip.
(Mirror for shorts.)

## Architecture

A new background loop, `run_breakeven_monitor`, mirroring the existing
`run_fill_monitor` / `run_pnl_monitor` shape (single responsibility, never panics,
logs and continues on error). It runs at the fill-monitor cadence (`poll_secs`,
default 30s) — fast enough for a stop move, and far quicker than the 900s
`pnl_push_secs` P&L cadence.

Each tick:

1. Read live settings; if `breakeven_enabled` is false, idle and continue.
2. Poll `positions()`.
3. For each open position, call `open_orders(coin)` and find the **stop-loss
   trigger**: `is_trigger && reduce_only && !is_take_profit`. Skip the position if
   there is no such order (nothing to move).
4. Let `stop = sl.trigger_price`. **Skip** if the stop is already on the profitable
   side of entry (`already_at_breakeven` → already moved), or if
   `risk_distance = |entry_px - stop|` is effectively zero.
5. Compute `profit_distance` (mark vs entry, direction-aware).
6. If `reached_breakeven_threshold` (`profit_distance >= breakeven_trigger_r *
   risk_distance`):
   1. Compute `be_price = breakeven_price(direction, entry_px, breakeven_buffer_pct)`.
   2. **Guard:** `be_price` must be a valid stop relative to mark (below mark for a
      long, above for a short). If not (only when `R*risk < buffer`, rare), log and
      skip.
   3. Place the **new** stop-loss trigger at `be_price`, reduce-only, sized to the
      current position size.
   4. **Then** cancel the **old** stop-loss by `sl.oid`.
   5. Notify the user: `🛡️ SL {coin} digeser ke breakeven`.

**Ordering — place new before cancelling old:** the position is never left without
a stop. If the cancel fails, two reduce-only stops rest briefly; the breakeven one
is nearer the mark and triggers first, and reduce-only prevents any over-close. Any
leftover is swept by `cancel_orders_for_coin` when the position finally closes.

## Pure core (unit-tested, no I/O)

`Direction` is the existing `crate::parser::Direction` enum (`Long` / `Short`).

```rust
/// True when profit has reached `trigger_r` times the initial risk distance.
/// risk = |entry - stop_loss|; profit = (mark-entry) for Long, (entry-mark) for Short.
fn reached_breakeven_threshold(
    direction: Direction, entry: f64, stop_loss: f64, mark: f64, trigger_r: f64,
) -> bool;

/// Breakeven stop price: entry nudged past itself by `buffer_pct`% to cover fees.
/// Long → entry * (1 + buffer_pct/100); short → entry * (1 - buffer_pct/100).
fn breakeven_price(direction: Direction, entry: f64, buffer_pct: f64) -> f64;

/// True when the resting `stop` is already on the profitable side of `entry`
/// (Long: stop >= entry; Short: stop <= entry) — i.e. it has already been moved.
fn already_at_breakeven(direction: Direction, entry: f64, stop: f64) -> bool;
```

The monitor loop is a thin wiring layer over these three functions plus exchange I/O.

## Exchange trait addition

A read-only query, mirroring the existing `frontendOpenOrders` usage in
`cancel_orders_for_coin`:

```rust
async fn open_orders(&self, coin: &str) -> anyhow::Result<Vec<OpenOrder>>;

pub struct OpenOrder {
    pub coin: String,
    pub oid: u64,
    pub trigger_price: f64,  // 0.0 for non-trigger orders
    pub is_trigger: bool,
    pub reduce_only: bool,
    pub is_take_profit: bool, // from HL orderType: "Take Profit *" → true, "Stop *" → false
}
```

Real impl: raw POST `{"type":"frontendOpenOrders","user":<addr>}` (same pattern as
`usdc_flows`), mapping `triggerPx`, `isTrigger`, `reduceOnly`, and `orderType`.
Mock impl: returns a seeded `Vec<OpenOrder>` for tests.

No journal schema changes are required.

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
- `already_at_breakeven`: long stop below/at/above entry; short mirror.
- Settings: `breakeven_trigger_r` rejects ≤ 0; `breakeven_buffer_pct` rejects < 0;
  persist/reload round-trips all three keys.

**Integration (MockExchange, seeded positions + open_orders):**
- Position past threshold with a far stop → new stop placed at breakeven, old stop
  cancelled (recorded in `cancels`), one notification.
- Position below threshold → no order activity.
- Stop already at/past entry (already moved) → no-op.
- No reduce-only stop trigger present → no-op.
- `breakeven_enabled` false → loop idles, no order activity.

## Out-of-scope / future

- Trailing stop beyond breakeven.
- Promoting the fee buffer to a per-leverage or per-coin value.
