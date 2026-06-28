//! Background fill monitor: polls Hyperliquid fill history and notifies the
//! Telegram user when a TP or SL trigger closes a position.

use crate::hyperliquid::{Exchange, FillDetail};
use crate::journal::{Bracket, Journal};
use crate::settings::Settings;
use crate::trigger_store::{PendingTrigger, TriggerStore};
use std::sync::{Arc, Mutex};
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

/// True when `now_secs` is past the trigger's expiry timestamp.
pub fn is_expired(now_secs: i64, expiry_at: i64) -> bool {
    now_secs > expiry_at
}

/// True when `fill` is the OPENING fill of `pending`'s entry order: an opening
/// `dir`, matching `entry_oid` (when known), and `time_ms >= created_at`. Matching
/// the specific order id keeps this correct even if a position already existed on
/// the coin (an older or closing fill never matches).
pub fn matches_entry_fill(fill: &FillDetail, pending: &PendingTrigger) -> bool {
    let is_opening = fill.dir.to_ascii_lowercase().contains("open");
    let oid_ok = match pending.entry_oid {
        Some(oid) => fill.oid == oid,
        None => fill.coin.eq_ignore_ascii_case(&pending.coin),
    };
    is_opening && oid_ok && fill.time_ms >= pending.created_at
}

/// Total filled size across all opening fills that belong to `pending`'s entry
/// order. Summing (not first-match) keeps the bracket correctly sized when a
/// market-on-trigger entry fills in multiple prints.
pub fn sum_entry_fill_size(fills: &[FillDetail], pending: &PendingTrigger) -> f64 {
    fills.iter().filter(|fill| matches_entry_fill(fill, pending)).map(|fill| fill.sz).sum()
}

/// Formats a signed USD PnL as `+$12.30` / `-$8.10`.
pub fn format_pnl(closed_pnl: f64) -> String {
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
            // Label against the bracket of the trade that was live when this fill
            // happened (most recent entry at or before fill.time_ms), so a re-entry
            // opened later on the same coin can't steal the label. `closed_pnl` is always
            // correct; this keeps the leg label (SL/TP) attributed to the right trade.
            let label = journal
                .bracket_for_coin_at(&fill.coin, fill.time_ms)
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

/// Polls for trigger-entry fills (arming the bracket) and expiries (cancelling the
/// resting order). Never panics: all errors are logged and the loop continues.
pub async fn run_trigger_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    triggers: Arc<TriggerStore>,
    allowed_user_ids: Vec<i64>,
    poll_secs: u64,
) {
    let interval = Duration::from_secs(poll_secs.max(1));
    loop {
        tokio::time::sleep(interval).await;

        let active = match triggers.list_active() {
            Ok(active) => active,
            Err(error) => { tracing::warn!("trigger monitor list_active failed: {error}"); continue; }
        };
        if active.is_empty() { continue; }

        let fills = exchange.fills_detailed().await.unwrap_or_else(|error| {
            tracing::warn!("trigger monitor fills_detailed failed: {error}");
            Vec::new()
        });
        let now_secs = now_unix_secs();

        for pending in active {
            // 1. Filled? Sum ALL matching opening fills so the bracket is sized to
            //    the actual position even when the entry filled in multiple prints.
            let filled_size = sum_entry_fill_size(&fills, &pending);
            if filled_size > 0.0 {
                let close_is_buy = !pending.direction.eq_ignore_ascii_case("long");
                // Place the SL FIRST. Only once it is on the book do we mark the
                // trigger armed — so a retry never double-places the SL (if the SL
                // itself fails, nothing was placed and the next poll retries cleanly).
                let stop_loss = crate::hyperliquid::TriggerOrder {
                    coin: pending.coin.clone(),
                    is_buy: close_is_buy,
                    size: filled_size,
                    trigger_price: pending.stop_loss,
                    is_take_profit: false,
                };
                match exchange.place_trigger(&stop_loss).await {
                    Err(error) => {
                        tracing::warn!("trigger SL placement failed for {}: {error}", pending.coin);
                        notify_users(&bot, &allowed_user_ids,
                            &format!("⚠️ Posisi {} TERBUKA tapi GAGAL pasang SL — cek manual SEKARANG!", pending.coin)).await;
                        // leave active → retry next poll (no SL placed yet, no duplicate)
                        continue;
                    }
                    Ok(_) => {
                        // Position is now protected; mark armed so the SL is never re-placed.
                        let _ = triggers.mark_armed(pending.id);
                        // Place TPs best-effort, sized to the actual filled size.
                        let mut failed_tps: Vec<usize> = Vec::new();
                        for (index, leg) in pending.take_profits.iter().enumerate() {
                            let tp = crate::hyperliquid::TriggerOrder {
                                coin: pending.coin.clone(),
                                is_buy: close_is_buy,
                                size: filled_size * (leg.alloc_pct / 100.0),
                                trigger_price: leg.price,
                                is_take_profit: true,
                            };
                            if let Err(error) = exchange.place_trigger(&tp).await {
                                tracing::warn!("trigger TP{} placement failed for {}: {error}", index + 1, pending.coin);
                                failed_tps.push(index + 1);
                            }
                        }
                        let message = if failed_tps.is_empty() {
                            format!("✅ Trigger {} kena — SL/TP terpasang.", pending.coin)
                        } else {
                            format!("✅ Trigger {} kena — SL terpasang. ⚠️ TP{:?} gagal, pasang manual.", pending.coin, failed_tps)
                        };
                        notify_users(&bot, &allowed_user_ids, &message).await;
                    }
                }
                continue;
            }
            // 2. Expired? Cancel the resting entry order.
            if is_expired(now_secs, pending.expiry_at) {
                if let Some(oid) = pending.entry_oid {
                    if let Err(error) = exchange.cancel_order(&pending.coin, oid).await {
                        tracing::warn!("trigger expiry cancel failed for {}: {error}", pending.coin);
                    }
                }
                let _ = triggers.mark_expired(pending.id);
                notify_users(&bot, &allowed_user_ids, &format!("⏱️ Trigger {} kadaluarsa — dibatalkan, tidak ada posisi.", pending.coin)).await;
            }
        }
    }
}

/// Periodically pushes a running-P&L summary while positions are open. Reads
/// `pnl_push_secs` from live settings each tick (0 disables → idle). Never panics.
pub async fn run_pnl_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    settings: Arc<Mutex<Settings>>,
    allowed_user_ids: Vec<i64>,
) {
    loop {
        let push_secs = settings.lock().unwrap().pnl_push_secs;
        if push_secs == 0 {
            // Disabled: idle at a fixed cadence so re-enabling via /set takes effect.
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        tokio::time::sleep(Duration::from_secs(push_secs)).await;

        let positions = match exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => { tracing::warn!("pnl monitor positions() failed: {error}"); continue; }
        };
        if positions.is_empty() { continue; }

        let equity = match exchange.equity().await {
            Ok(equity) => equity,
            Err(error) => { tracing::warn!("pnl monitor equity() failed: {error}"); continue; }
        };
        let message = crate::telegram::render_pnl_summary(equity, &positions);
        for user_id in &allowed_user_ids {
            if let Err(error) = bot.send_message(ChatId(*user_id), &message).await {
                tracing::warn!("pnl push failed for {user_id}: {error}");
            }
        }
    }
}

/// Polls open positions and moves each stop-loss to breakeven once profit reaches
/// `breakeven_trigger_r` R. Reads settings live each tick; disabled (idle) when
/// `breakeven_enabled` is false. Never panics: all errors are logged.
pub async fn run_breakeven_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    settings: Arc<Mutex<Settings>>,
    allowed_user_ids: Vec<i64>,
    poll_secs: u64,
) {
    let interval = Duration::from_secs(poll_secs.max(1));
    loop {
        tokio::time::sleep(interval).await;

        let (enabled, trigger_r, buffer_pct) = {
            let guard = settings.lock().unwrap();
            (guard.breakeven_enabled, guard.breakeven_trigger_r, guard.breakeven_buffer_pct)
        };
        if !enabled {
            continue;
        }

        let positions = match exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => { tracing::warn!("breakeven monitor positions() failed: {error}"); continue; }
        };

        for position in &positions {
            let orders = match exchange.open_orders(&position.coin).await {
                Ok(orders) => orders,
                Err(error) => { tracing::warn!("breakeven monitor open_orders({}) failed: {error}", position.coin); continue; }
            };
            // The reduce-only stop-loss trigger for this position (not a take-profit).
            let stop = orders.iter().find(|o| o.is_trigger && o.reduce_only && !o.is_take_profit);
            let action = match crate::breakeven::decide_breakeven(position, stop, trigger_r, buffer_pct) {
                Some(action) => action,
                None => continue,
            };
            match crate::breakeven::apply_breakeven(exchange.as_ref(), &position.coin, &action).await {
                Ok(()) => {
                    notify_users(&bot, &allowed_user_ids,
                        &format!("🛡️ SL {} digeser ke breakeven (${:.4}).", position.coin, action.be_price)).await;
                }
                Err(error) => {
                    tracing::warn!("breakeven move for {} failed: {error}", position.coin);
                }
            }
        }
    }
}

/// Sends `message` to every allowlisted user, logging (not propagating) send errors.
async fn notify_users(bot: &Bot, allowed_user_ids: &[i64], message: &str) {
    for user_id in allowed_user_ids {
        if let Err(error) = bot.send_message(ChatId(*user_id), message).await {
            tracing::warn!("trigger notification failed for {user_id}: {error}");
        }
    }
}

/// Current UNIX time in seconds (0 on clock error).
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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

    fn detail_full(dir: &str, oid: u64, time_ms: i64, coin: &str) -> FillDetail {
        FillDetail { coin: coin.into(), oid, dir: dir.into(), px: 68.53, sz: 0.5,
            closed_pnl: 0.0, fee: 0.0, time_ms, start_position: 0.0 }
    }

    fn pending(entry_oid: Option<u64>, created_at: i64) -> crate::trigger_store::PendingTrigger {
        crate::trigger_store::PendingTrigger {
            id: 1, coin: "SOL".into(), direction: "Long".into(), size: 0.5, trigger_px: 68.53,
            leverage: 3, stop_loss: 68.02, take_profits: vec![], entry_oid, chat_id: 1,
            created_at, expiry_at: created_at + 100, status: "active".into(),
        }
    }

    #[test]
    fn is_expired_compares_now_to_expiry() {
        assert!(!is_expired(1099, 1100));
        assert!(is_expired(1101, 1100));
    }

    #[test]
    fn sum_entry_fill_size_adds_matching_opening_fills() {
        let p = pending(Some(7), 1000);
        let fills = vec![
            detail_full("Open Long", 7, 1500, "SOL"),  // sz 0.5
            detail_full("Open Long", 7, 1600, "SOL"),  // sz 0.5
            detail_full("Open Long", 8, 1500, "SOL"),  // wrong oid → ignored
            detail_full("Close Long", 7, 1500, "SOL"), // closing → ignored
        ];
        assert!((sum_entry_fill_size(&fills, &p) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn matches_entry_fill_by_oid_and_opening_dir_and_time() {
        let p = pending(Some(7), 1000);
        let opening = detail_full("Open Long", 7, 1500, "SOL");
        assert!(matches_entry_fill(&opening, &p));
        // wrong oid
        assert!(!matches_entry_fill(&detail_full("Open Long", 8, 1500, "SOL"), &p));
        // closing dir
        assert!(!matches_entry_fill(&detail_full("Close Long", 7, 1500, "SOL"), &p));
        // older than created_at
        assert!(!matches_entry_fill(&detail_full("Open Long", 7, 999, "SOL"), &p));
        // pre-existing-position guard: an older opening fill on same coin must not match
        assert!(!matches_entry_fill(&detail_full("Open Long", 7, 500, "SOL"), &p));
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

    #[tokio::test]
    async fn apply_breakeven_preserves_old_stop_when_place_fails() {
        use crate::hyperliquid::testing::MockExchange;
        use crate::breakeven::BreakevenAction;
        let mock = MockExchange::new_for_test();
        mock.set_fail_place_trigger(true);
        let action = BreakevenAction { be_price: 100.1, size: 0.5, close_is_buy: false, old_oid: 7 };
        let result = crate::breakeven::apply_breakeven(&mock, "BTC", &action).await;
        assert!(result.is_err(), "place failure must propagate");
        // Old stop must NOT be cancelled when the new stop failed to place.
        assert!(mock.cancels.lock().unwrap().is_empty(), "old stop must remain when new stop placement fails");
    }

    #[tokio::test]
    async fn apply_breakeven_places_new_stop_then_cancels_old() {
        use crate::hyperliquid::testing::MockExchange;
        use crate::breakeven::BreakevenAction;
        let mock = MockExchange::new_for_test();
        let action = BreakevenAction { be_price: 100.1, size: 0.5, close_is_buy: false, old_oid: 7 };
        crate::breakeven::apply_breakeven(&mock, "BTC", &action).await.unwrap();
        // New reduce-only stop placed at the breakeven price.
        let triggers = mock.triggers.lock().unwrap();
        assert_eq!(triggers.len(), 1);
        assert!(!triggers[0].is_take_profit);
        assert!((triggers[0].trigger_price - 100.1).abs() < 1e-9);
        // Old stop cancelled by oid.
        let cancels = mock.cancels.lock().unwrap();
        assert_eq!(*cancels, vec![("BTC".to_string(), 7u64)]);
    }
}
