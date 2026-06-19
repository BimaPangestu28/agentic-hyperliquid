//! Background fill monitor: polls Hyperliquid fill history and notifies the
//! Telegram user when a TP or SL trigger closes a position.

use crate::hyperliquid::{Exchange, FillDetail};
use crate::journal::{Bracket, Journal};
use std::sync::Arc;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::ChatId;

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

/// Formats a signed USD PnL as `+$12.30` / `-$8.10`.
fn format_pnl(closed_pnl: f64) -> String {
    let sign = if closed_pnl < 0.0 { "-" } else { "+" };
    format!("{sign}${:.2}", closed_pnl.abs())
}

/// Builds the Telegram message for a closed position.
pub fn format_close_message(coin: &str, label: Option<CloseLabel>, closed_pnl: f64) -> String {
    let pnl = format_pnl(closed_pnl);
    match label {
        Some(CloseLabel::TakeProfit(index)) => format!("🎯 TP{index} kena — {coin} {pnl}"),
        Some(CloseLabel::StopLoss) => format!("🛑 SL kena — {coin} {pnl}"),
        None => format!("📕 Posisi {coin} ditutup — {pnl}"),
    }
}

/// Polls fill history every `poll_secs` and notifies all `allowed_user_ids`
/// when a TP/SL closing fill is observed. Never panics: poll/send/DB errors are
/// logged and the loop continues.
///
/// On a brand-new database (`seen_fills` empty) the first pass baselines all
/// historical closing fills silently so the user is not spammed at first boot.
pub async fn run_fill_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    journal: Arc<Journal>,
    allowed_user_ids: Vec<i64>,
    poll_secs: u64,
) {
    // Baseline: silence pre-existing fills on a fresh database.
    if journal.seen_fills_empty().unwrap_or(false) {
        match exchange.fills_detailed().await {
            Ok(fills) => {
                for fill in fills.iter().filter(|fill| is_closing(fill)) {
                    let _ = journal.mark_fill_seen(&fill_key(fill));
                }
                tracing::info!("fill monitor baselined historical fills");
            }
            Err(error) => {
                tracing::warn!("fill monitor baseline skipped: fills_detailed failed: {error}");
            }
        }
    }

    let interval = Duration::from_secs(poll_secs.max(1));
    loop {
        tokio::time::sleep(interval).await;

        let fills = match exchange.fills_detailed().await {
            Ok(fills) => fills,
            Err(error) => {
                tracing::warn!("fill monitor poll failed: {error}");
                continue;
            }
        };

        for fill in fills.iter().filter(|fill| is_closing(fill)) {
            let key = fill_key(fill);
            match journal.is_fill_seen(&key) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!("fill monitor is_fill_seen failed for {key}: {error}");
                    continue;
                }
            }
            // Labelling uses the NEWEST journaled bracket for the coin. If multiple
            // trades on the same coin are open concurrently, an older trade's close
            // may be labelled against the newer trade's SL/TP prices. The PnL value
            // from `closed_pnl` is always correct; only the leg label (SL/TP) may
            // be misattributed in that multi-trade scenario.
            let label = journal
                .latest_bracket_for_coin(&fill.coin)
                .ok()
                .flatten()
                .map(|bracket| classify_close_fill(fill.px, &bracket));
            let message = format_close_message(&fill.coin, label, fill.closed_pnl);
            for user_id in &allowed_user_ids {
                if let Err(error) = bot.send_message(ChatId(*user_id), &message).await {
                    tracing::warn!("close notification failed for {user_id}: {error}");
                }
            }
            let _ = journal.mark_fill_seen(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_close_message_labels_leg_and_pnl() {
        let tp = format_close_message("TAO", Some(CloseLabel::TakeProfit(1)), 12.30);
        assert!(tp.contains("TP1"));
        assert!(tp.contains("TAO"));
        assert!(tp.contains("+$12.30"));

        let sl = format_close_message("TAO", Some(CloseLabel::StopLoss), -8.10);
        assert!(sl.contains("SL"));
        assert!(sl.contains("-$8.10"));

        let generic = format_close_message("TAO", None, 5.0);
        assert!(generic.contains("ditutup"));
    }

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

    #[test]
    fn classify_tie_break_prefers_take_profit() {
        // Equidistant: 310 is 30 from SL(280) and 30 from TP1(340) → TP wins.
        let bracket = Bracket { stop_loss: 280.0, take_profits: vec![340.0] };
        assert_eq!(classify_close_fill(310.0, &bracket), CloseLabel::TakeProfit(1));
    }
}
