//! Background fill monitor: polls Hyperliquid fill history and notifies the
//! Telegram user when a TP or SL trigger closes a position.

use crate::hyperliquid::FillDetail;
use crate::journal::Bracket;

/// Which bracket leg a closing fill corresponds to.
#[derive(Debug, Clone, PartialEq)]
pub enum CloseLabel {
    StopLoss,
    /// 1-based take-profit index (TP1, TP2, ...).
    TakeProfit(usize),
}

/// True when a fill closes/reduces a position rather than opening one.
pub fn is_closing(fill: &FillDetail) -> bool {
    fill.dir.to_ascii_lowercase().contains("close") || fill.closed_pnl != 0.0
}

/// Stable dedup key for a single fill: order id, time, price, size.
pub fn fill_key(fill: &FillDetail) -> String {
    format!("{}:{}:{}:{}", fill.oid, fill.time_ms, fill.px, fill.sz)
}

/// Labels a closing fill by matching its price to the nearest bracket leg.
///
/// Compares `fill_price` against the stop-loss price and each take-profit
/// price, returning the leg with the smallest absolute price distance. On a
/// tie the take-profit wins (profit-taking is the common case).
pub fn classify_close_fill(fill_price: f64, bracket: &Bracket) -> CloseLabel {
    let stop_distance = (fill_price - bracket.stop_loss).abs();
    let mut best_label = CloseLabel::StopLoss;
    let mut best_distance = stop_distance;
    for (index, take_profit_price) in bracket.take_profits.iter().enumerate() {
        let distance = (fill_price - take_profit_price).abs();
        if distance <= best_distance {
            best_distance = distance;
            best_label = CloseLabel::TakeProfit(index + 1);
        }
    }
    best_label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(dir: &str, closed_pnl: f64, oid: u64, px: f64, sz: f64, time_ms: i64) -> FillDetail {
        FillDetail {
            coin: "TAO".into(),
            oid,
            dir: dir.into(),
            px,
            sz,
            closed_pnl,
            fee: 0.0,
            time_ms,
            start_position: 0.0,
        }
    }

    #[test]
    fn is_closing_detects_close_dir_and_pnl() {
        assert!(is_closing(&detail("Close Long", 0.0, 1, 1.0, 1.0, 1)));
        assert!(is_closing(&detail("Open Long", 5.0, 1, 1.0, 1.0, 1)));
        assert!(!is_closing(&detail("Open Long", 0.0, 1, 1.0, 1.0, 1)));
    }

    #[test]
    fn fill_key_is_composite_and_stable() {
        let fill = detail("Close Long", 1.0, 42, 300.5, 10.0, 1700);
        assert_eq!(fill_key(&fill), "42:1700:300.5:10");
    }

    #[test]
    fn classify_matches_nearest_leg() {
        let bracket = Bracket { stop_loss: 280.0, take_profits: vec![340.0, 380.0] };
        assert_eq!(classify_close_fill(279.5, &bracket), CloseLabel::StopLoss);
        assert_eq!(classify_close_fill(341.0, &bracket), CloseLabel::TakeProfit(1));
        assert_eq!(classify_close_fill(379.0, &bracket), CloseLabel::TakeProfit(2));
    }

    #[test]
    fn classify_with_only_stop_loss() {
        let bracket = Bracket { stop_loss: 280.0, take_profits: vec![] };
        assert_eq!(classify_close_fill(999.0, &bracket), CloseLabel::StopLoss);
    }
}
