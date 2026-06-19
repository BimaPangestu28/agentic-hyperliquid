# Bracket Leg Minimum-Notional Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reject a trade at sizing time when any bracket leg (SL or a TP) would fall below Hyperliquid's $10 minimum order value, instead of failing mid-execution and leaving a half-bracketed position.

**Architecture:** Add a `SizingError::BracketLegBelowMin` variant and, in `build_plan`, validate every bracket leg's notional ≥ `MIN_ORDER_NOTIONAL` after the legs are computed; the existing `process_setups` error path surfaces the message to the user.

**Tech Stack:** Rust, thiserror.

## Global Constraints

- Descriptive names; verbs for functions, nouns for values.
- TDD: failing test first.
- `MIN_ORDER_NOTIONAL` is `10.0` (already defined in `src/sizing.rs`).
- Fail early at sizing — do NOT auto-adjust/collapse TP allocations.
- Only `src/sizing.rs` changes.

---

### Task 1: Per-leg minimum-notional validation in `build_plan`

**Files:**
- Modify: `src/sizing.rs` (`SizingError` enum, `build_plan`, `mod tests`)

**Interfaces:**
- Produces: `SizingError::BracketLegBelowMin { leg: String, value: f64, min: f64 }`.
- Consumes: existing `MIN_ORDER_NOTIONAL`, `ExecutionPlan`, `BracketLeg`, `SizingError`.

- [ ] **Step 1: Write the failing tests**

Add to `src/sizing.rs` `mod tests`:

```rust
#[test]
fn rejects_tp_leg_below_min_notional() {
    // Fixed-USD-style small entry: pendle entry 1.40, ~$15 notional split 60/40.
    // TP2 (40%) lands at ~$8.40 notional < $10 → rejected.
    let setup = pendle_setup();
    let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
    let input = SizingInput {
        setup: &setup,
        equity: 10_000.0,
        risk_pct: 1.0,
        entry_mode: EntryMode::FixedUsd,
        entry_pct: 10.0,
        entry_fixed_usd: 15.0,
        profile: RiskProfile::Moderate,
        leverage: &leverage(),
        asset_meta: &meta,
    };
    match build_plan(&input).unwrap_err() {
        SizingError::BracketLegBelowMin { leg, value, min } => {
            assert_eq!(leg, "TP2");
            assert!(value < 10.0, "value {value} should be below min");
            assert_eq!(min, 10.0);
        }
        other => panic!("expected BracketLegBelowMin, got {other:?}"),
    }
}

#[test]
fn rejects_sl_leg_below_min_notional() {
    use crate::parser::TakeProfit;
    // Entry notional just above $10 but a distant stop → SL notional < $10.
    let setup = TradeSetup {
        coin: "AAA".into(),
        direction: Direction::Long,
        timeframe: None,
        risk_reward: None,
        confidence: None,
        entry: 1.00,
        stop_loss: 0.50, // SL value = size * 0.50, far below entry value
        take_profits: vec![TakeProfit { price: 1.30, allocation_pct: 100.0 }],
    };
    let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
    let input = SizingInput {
        setup: &setup,
        equity: 10_000.0,
        risk_pct: 1.0,
        entry_mode: EntryMode::FixedUsd,
        entry_pct: 10.0,
        entry_fixed_usd: 10.5, // notional ~10.5 (>=10), but SL ~5.25 (<10)
        profile: RiskProfile::Moderate,
        leverage: &leverage(),
        asset_meta: &meta,
    };
    match build_plan(&input).unwrap_err() {
        SizingError::BracketLegBelowMin { leg, value, .. } => {
            assert_eq!(leg, "SL");
            assert!(value < 10.0, "value {value} should be below min");
        }
        other => panic!("expected BracketLegBelowMin, got {other:?}"),
    }
}

#[test]
fn valid_plan_passes_leg_min_check() {
    // The existing well-sized case must still succeed (no regression).
    let setup = pendle_setup();
    let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
    let input = SizingInput {
        setup: &setup,
        equity: 10_000.0,
        risk_pct: 1.0,
        entry_mode: EntryMode::RiskBased,
        entry_pct: 10.0,
        entry_fixed_usd: 50.0,
        profile: RiskProfile::Moderate,
        leverage: &leverage(),
        asset_meta: &meta,
    };
    assert!(build_plan(&input).is_ok());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib sizing::tests::rejects_tp_leg_below_min_notional sizing::tests::rejects_sl_leg_below_min_notional 2>&1 | tail -20`
Expected: FAIL — `BracketLegBelowMin` variant does not exist (compile error), or the calls return `Ok`/`BelowMinSize` instead of the new variant.

- [ ] **Step 3: Add the error variant**

In `src/sizing.rs`, add to `enum SizingError` (after `MarginExceedsEquity`):

```rust
    #[error("{leg} order value ${value:.2} is below the ${min:.0} minimum; increase entry size or use fewer take-profits")]
    BracketLegBelowMin { leg: String, value: f64, min: f64 },
```

- [ ] **Step 4: Validate each leg in `build_plan`**

In `build_plan`, immediately AFTER the `let take_profits = setup.take_profits.iter().map(...).collect();` statement (and before `let mut warnings = Vec::new();`), insert:

```rust
    // Each bracket leg is placed as a separate exchange order and must independently
    // clear the $10 minimum. The SL covers the full size; each TP a fraction — a small
    // entry split across TPs can drop a leg below the minimum even when the total
    // notional clears it. Reject here so no half-bracketed position is ever opened.
    let stop_loss_value = size * setup.stop_loss;
    if stop_loss_value < MIN_ORDER_NOTIONAL {
        return Err(SizingError::BracketLegBelowMin {
            leg: "SL".to_string(),
            value: stop_loss_value,
            min: MIN_ORDER_NOTIONAL,
        });
    }
    for (index, take_profit) in take_profits.iter().enumerate() {
        let leg_value = take_profit.size * take_profit.price;
        if leg_value < MIN_ORDER_NOTIONAL {
            return Err(SizingError::BracketLegBelowMin {
                leg: format!("TP{}", index + 1),
                value: leg_value,
                min: MIN_ORDER_NOTIONAL,
            });
        }
    }
```

Note: `take_profits` here is the already-built `Vec<BracketLeg>` (each has `.size` =
floored fraction and `.price`). `size` and `setup.stop_loss` are already in scope.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib sizing:: 2>&1 | tail -20`
Expected: PASS — the two new rejection tests, `valid_plan_passes_leg_min_check`, and all pre-existing sizing tests (esp. `computes_risk_based_size_and_brackets`) green.

- [ ] **Step 6: Full suite + commit**

Run: `cargo test 2>&1 | tail -5`
Expected: PASS.

```bash
git add src/sizing.rs
git commit -m "feat(sizing): reject trades whose bracket legs fall below the \$10 minimum"
```

---

## Self-Review Notes

- **Spec coverage:** per-leg validation (SL + TPs) → Step 4; `BracketLegBelowMin` variant → Step 3; fail-early-at-sizing (no auto-adjust) → design honored; no-regression → `valid_plan_passes_leg_min_check` + existing tests. Surfacing via `process_setups` is unchanged (existing `SizingError` path).
- **Check order:** SL is checked before TPs, so the reported leg is deterministic (matches the `rejects_sl_leg_below_min_notional` expectation).
- **Type consistency:** `BracketLegBelowMin { leg: String, value: f64, min: f64 }` is referenced identically in the variant definition, the two `return Err(..)` sites, and all three tests.
