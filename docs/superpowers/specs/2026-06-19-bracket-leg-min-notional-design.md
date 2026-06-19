# Bracket Leg Minimum-Notional Validation — Design

**Date:** 2026-06-19
**Status:** Approved (pending spec review)

## Problem

Hyperliquid rejects any order below $10 notional. `build_plan` only validates the **total**
entry notional (`MIN_ORDER_NOTIONAL`), not the individual bracket legs. With a small fixed
entry split across take-profits (e.g. $15 entry, TP1 60% ≈ $9, TP2 40% ≈ $6), the entry and
SL place fine but the TP legs are rejected mid-execution with
`order rejected: Order must have minimum value of $10`. Because the SL is placed before the
TPs, this can leave a position open with only a partial bracket while the user sees a generic
"Execution failed" message.

## Goal

Reject such trades **at sizing time** (before the confirmation card / execution) with a
clear, actionable error, so no half-bracketed position is ever opened.

## Non-goals

- No auto-adjusting/collapsing of TP allocations (the user's exit strategy is not changed
  silently). Chosen behavior: fail early and tell the user.
- No change to how `SizingError` is surfaced — the existing `process_setups` path already
  sends `SizingError` text to the user.

## Design

In `build_plan` (`src/sizing.rs`), after the bracket legs are computed, validate that
**every** bracket leg's notional is ≥ `MIN_ORDER_NOTIONAL`:

- Each take-profit leg: `take_profit.size * take_profit.price`.
- The stop-loss leg: `size * stop_loss_price` (the SL covers the full size; with a small
  entry near the minimum and a distant stop, the SL notional can also fall below $10).

The entry/total notional check already exists and stays. The new per-leg check runs after
the `take_profits` vector is built and before `Ok(ExecutionPlan { .. })`.

On the first failing leg, return a new error variant:

```rust
#[error("{leg} order value ${value:.2} is below the ${min:.0} minimum; increase entry size or use fewer take-profits")]
BracketLegBelowMin { leg: String, value: f64, min: f64 },
```

`leg` is `"SL"`, `"TP1"`, `"TP2"`, … (1-based for TPs). `value` is the leg's notional;
`min` is `MIN_ORDER_NOTIONAL`. Check the SL first, then TPs in order, so the reported leg is
deterministic.

## Data flow

`process_setups` → `build_plan` → `Err(BracketLegBelowMin { .. })` → existing error path
sends the message to the user. No confirmation card is shown; nothing is executed.

## Testing (TDD)

- A small fixed-USD-style plan whose TP legs fall below $10 (e.g. entry notional ~$15 split
  60/40) → `build_plan` returns `BracketLegBelowMin { leg: "TP1", .. }` (or "TP2" depending
  on which is checked first — the test asserts the variant and that `value < 10`).
- An SL leg below $10 (entry just above $10 with a distant stop) → `BracketLegBelowMin {
  leg: "SL", .. }`.
- A normally-sized plan (existing `computes_risk_based_size_and_brackets` case) still
  succeeds — the new check must not regress valid trades.

## Files touched

- `src/sizing.rs` — new `SizingError::BracketLegBelowMin` variant + per-leg validation in
  `build_plan` + tests.
