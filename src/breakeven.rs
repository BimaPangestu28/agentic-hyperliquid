//! Pure helpers for the auto-breakeven stop-loss feature. No I/O — every
//! function is a deterministic computation over prices, so the decision logic
//! is unit-tested without an exchange or a clock.

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

#[cfg(test)]
mod tests {
    use super::*;

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
