//! Pure helpers for the auto-breakeven stop-loss feature. No I/O — every
//! function is a deterministic computation over prices, so the decision logic
//! is unit-tested without an exchange or a clock.

use crate::hyperliquid::{Exchange, OpenOrder, OpenPosition, TriggerOrder};
use crate::parser::Direction;

/// True when profit has reached `trigger_r` times the initial risk distance.
/// `risk = |entry - stop_loss|`; profit is `mark - entry` for a long and
/// `entry - mark` for a short. Returns false when risk is ~zero (degenerate setup).
pub fn reached_breakeven_threshold(
    direction: Direction,
    entry: f64,
    stop_loss: f64,
    mark: f64,
    trigger_r: f64,
) -> bool {
    let risk_distance = (entry - stop_loss).abs();
    if risk_distance < f64::EPSILON {
        return false;
    }
    let profit_distance = match direction {
        Direction::Long => mark - entry,
        Direction::Short => entry - mark,
    };
    profit_distance >= trigger_r * risk_distance
}

/// Breakeven stop price: `entry` nudged past itself by `buffer_pct`% to cover fees,
/// so a stop-out closes ~flat after round-trip fees. Long → above entry; short → below.
pub fn breakeven_price(direction: Direction, entry: f64, buffer_pct: f64) -> f64 {
    let buffer = entry * (buffer_pct / 100.0);
    match direction {
        Direction::Long => entry + buffer,
        Direction::Short => entry - buffer,
    }
}

/// True when the resting `stop` is already on the profitable side of `entry`
/// (long: `stop >= entry`; short: `stop <= entry`) — i.e. it has already been
/// moved to breakeven and must not be moved again.
pub fn already_at_breakeven(direction: Direction, entry: f64, stop: f64) -> bool {
    match direction {
        Direction::Long => stop >= entry,
        Direction::Short => stop <= entry,
    }
}

/// The concrete order change to apply when a stop should be moved to breakeven.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakevenAction {
    pub be_price: f64,
    pub size: f64,
    /// Side of the replacement stop (a long position closes with a sell → false).
    pub close_is_buy: bool,
    /// The order id of the stop to cancel after the new one is placed.
    pub old_oid: u64,
}

/// Decides whether `position`'s stop should be moved to breakeven, given the
/// resting stop-loss order (`stop`) and the configured thresholds. Returns `None`
/// to skip: no stop found, already moved, degenerate risk, profit below threshold,
/// or the computed breakeven price is not a valid stop relative to the mark.
pub fn decide_breakeven(
    position: &OpenPosition,
    stop: Option<&OpenOrder>,
    trigger_r: f64,
    buffer_pct: f64,
) -> Option<BreakevenAction> {
    let stop = stop?;
    let direction = if position.direction.eq_ignore_ascii_case("long") {
        Direction::Long
    } else {
        Direction::Short
    };
    let entry = position.entry_px;
    let stop_price = stop.trigger_price;

    if already_at_breakeven(direction, entry, stop_price) {
        return None;
    }
    if !reached_breakeven_threshold(direction, entry, stop_price, position.mark_px, trigger_r) {
        return None;
    }
    let be_price = breakeven_price(direction, entry, buffer_pct);
    // Guard: the new stop must sit on the correct side of the mark (below for a
    // long, above for a short). Only violated when R*risk < buffer (rare).
    let valid = match direction {
        Direction::Long => be_price < position.mark_px,
        Direction::Short => be_price > position.mark_px,
    };
    if !valid {
        return None;
    }
    let close_is_buy = matches!(direction, Direction::Short);
    Some(BreakevenAction { be_price, size: position.size, close_is_buy, old_oid: stop.oid })
}

/// Applies a breakeven move: places the new stop FIRST (position never
/// unprotected), then cancels the old stop by id.
pub async fn apply_breakeven<E: Exchange + ?Sized>(
    exchange: &E,
    coin: &str,
    action: &BreakevenAction,
) -> anyhow::Result<()> {
    exchange
        .place_trigger(&TriggerOrder {
            coin: coin.to_string(),
            is_buy: action.close_is_buy,
            size: action.size,
            trigger_price: action.be_price,
            is_take_profit: false,
        })
        .await?;
    exchange.cancel_order(coin, action.old_oid).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyperliquid::{OpenOrder, OpenPosition};

    fn long_position(entry: f64, mark: f64) -> OpenPosition {
        OpenPosition {
            coin: "BTC".into(), direction: "long".into(), size: 0.5,
            entry_px: entry, mark_px: mark, unrealized_pnl: 0.0, leverage: 10.0, notional: 0.0,
        }
    }

    fn stop_order(oid: u64, trigger: f64) -> OpenOrder {
        OpenOrder { coin: "BTC".into(), oid, trigger_price: trigger, is_trigger: true, reduce_only: true, is_take_profit: false }
    }

    #[test]
    fn decide_moves_short_past_threshold() {
        let position = OpenPosition {
            coin: "BTC".into(), direction: "short".into(), size: 0.5,
            entry_px: 100.0, mark_px: 95.0, unrealized_pnl: 0.0, leverage: 10.0, notional: 0.0,
        };
        let stop = OpenOrder { coin: "BTC".into(), oid: 9, trigger_price: 105.0, is_trigger: true, reduce_only: true, is_take_profit: false };
        let action = decide_breakeven(&position, Some(&stop), 1.0, 0.1).unwrap();
        assert!((action.be_price - 99.9).abs() < 1e-9);
        assert!(action.close_is_buy); // short closes with a buy
        assert_eq!(action.old_oid, 9);
    }

    #[test]
    fn decide_skips_when_breakeven_price_wrong_side_of_mark() {
        let position = OpenPosition {
            coin: "BTC".into(), direction: "long".into(), size: 0.5,
            entry_px: 100.0, mark_px: 100.002, unrealized_pnl: 0.0, leverage: 10.0, notional: 0.0,
        };
        let stop = OpenOrder { coin: "BTC".into(), oid: 3, trigger_price: 99.999, is_trigger: true, reduce_only: true, is_take_profit: false };
        // 1R cleared, but breakeven price (100.1) is above the mark (100.002) -> invalid stop -> skip.
        assert!(decide_breakeven(&position, Some(&stop), 1.0, 0.1).is_none());
    }

    #[test]
    fn decide_moves_long_past_threshold() {
        // entry 100 stop 95 risk 5; mark 105 = 1R -> move. BE = 100.1, close side = sell (false).
        let action = decide_breakeven(&long_position(100.0, 105.0), Some(&stop_order(7, 95.0)), 1.0, 0.1).unwrap();
        assert!((action.be_price - 100.1).abs() < 1e-9);
        assert_eq!(action.old_oid, 7);
        assert!(!action.close_is_buy); // long closes with a sell
        assert!((action.size - 0.5).abs() < 1e-9);
    }

    #[test]
    fn decide_skips_below_threshold() {
        assert!(decide_breakeven(&long_position(100.0, 104.0), Some(&stop_order(7, 95.0)), 1.0, 0.1).is_none());
    }

    #[test]
    fn decide_skips_when_already_at_breakeven() {
        // stop already at/above entry -> already moved.
        assert!(decide_breakeven(&long_position(100.0, 110.0), Some(&stop_order(7, 100.0)), 1.0, 0.1).is_none());
    }

    #[test]
    fn decide_skips_when_no_stop_order() {
        assert!(decide_breakeven(&long_position(100.0, 110.0), None, 1.0, 0.1).is_none());
    }

    #[test]
    fn threshold_met_for_long_at_and_above_one_r() {
        // entry 100, stop 95 -> risk 5. 1R target = mark 105.
        assert!(!reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 104.9, 1.0));
        assert!(reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 105.0, 1.0));
        assert!(reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 110.0, 1.0));
    }

    #[test]
    fn threshold_met_for_short_at_and_above_one_r() {
        // entry 100, stop 105 -> risk 5. 1R target = mark 95.
        assert!(!reached_breakeven_threshold(Direction::Short, 100.0, 105.0, 95.1, 1.0));
        assert!(reached_breakeven_threshold(Direction::Short, 100.0, 105.0, 95.0, 1.0));
        assert!(reached_breakeven_threshold(Direction::Short, 100.0, 105.0, 90.0, 1.0));
    }

    #[test]
    fn threshold_respects_trigger_r_other_than_one() {
        // entry 100, stop 95 -> risk 5. 2R target = mark 110.
        assert!(!reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 109.0, 2.0));
        assert!(reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 110.0, 2.0));
    }

    #[test]
    fn threshold_false_when_risk_distance_zero() {
        // Degenerate setup: entry == stop. Never fires (guards divide-by-zero).
        assert!(!reached_breakeven_threshold(Direction::Long, 100.0, 100.0, 200.0, 1.0));
    }

    #[test]
    fn breakeven_price_nudges_past_entry() {
        // Long: above entry by buffer_pct%.
        assert!((breakeven_price(Direction::Long, 100.0, 0.1) - 100.1).abs() < 1e-9);
        // Short: below entry by buffer_pct%.
        assert!((breakeven_price(Direction::Short, 100.0, 0.1) - 99.9).abs() < 1e-9);
    }

    #[test]
    fn already_moved_when_stop_on_profitable_side_of_entry() {
        // Long: stop at/above entry means already moved.
        assert!(!already_at_breakeven(Direction::Long, 100.0, 95.0));
        assert!(already_at_breakeven(Direction::Long, 100.0, 100.0));
        assert!(already_at_breakeven(Direction::Long, 100.0, 100.1));
        // Short: stop at/below entry means already moved.
        assert!(!already_at_breakeven(Direction::Short, 100.0, 105.0));
        assert!(already_at_breakeven(Direction::Short, 100.0, 100.0));
        assert!(already_at_breakeven(Direction::Short, 100.0, 99.9));
    }
}
