//! Pure reconstruction of round-trip trades from a chronological fill stream,
//! plus the API response shape. No network, no journal — testable in isolation.

use crate::hyperliquid::FillDetail;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClosedTrade {
    pub external_id: String,
    pub coin: String,
    pub direction: String, // "long" | "short"
    pub size: f64,
    pub entry_px: f64,
    pub exit_px: f64,
    pub realized_pnl: f64,
    pub fee: f64,
    pub opened_at_ms: i64,
    pub closed_at_ms: i64,
    pub entry_oid: Option<u64>,
}

/// Flat-position tolerance: a signed position whose magnitude is below this is
/// treated as flat.
const EPS: f64 = 1e-9;

/// Signed size delta a fill applies to the running position, derived from the
/// Hyperliquid `dir` string.
///
/// A fill is a BUY (increases the signed position) when it is "Open Long" or
/// "Close Short"; it is a SELL (decreases the signed position) when it is
/// "Open Short" or "Close Long".
///
/// # Arguments
/// * `dir` - the SDK `dir` string, e.g. `"Open Long"` / `"Close Short"`
/// * `sz`  - absolute fill size (always > 0)
///
/// # Returns
/// `+sz` for buys, `-sz` for sells.
fn fill_signed_delta(dir: &str, sz: f64) -> f64 {
    let is_buy = (dir.contains("Open") && dir.contains("Long"))
        || (dir.contains("Close") && dir.contains("Short"));
    if is_buy {
        sz
    } else {
        -sz
    }
}

/// In-progress accumulator for a single position leg (one flat → … → flat
/// life-cycle). Entry/exit prices are accumulated as notional sums so they can
/// be turned into size-weighted averages when the leg is emitted.
struct Leg {
    direction: String, // "long" | "short" (sign of the position while open)
    open_size: f64,    // total absolute size opened in this leg
    entry_notional: f64, // Σ(open_fill.px * open_fill.sz)
    close_size: f64,   // total absolute size closed in this leg
    exit_notional: f64, // Σ(close_fill.px * close_fill.sz)
    realized_pnl: f64, // Σ(closed_pnl) over closing fills in this leg
    fee: f64,          // Σ(fee) over all fills attributed to this leg
    opened_at_ms: i64, // time of first opening fill
    closed_at_ms: i64, // time of last closing fill
    entry_oid: u64,    // oid of first opening fill
}

impl Leg {
    /// Finalize the leg into a `ClosedTrade` with size-weighted entry/exit.
    fn into_closed_trade(self, coin: &str) -> ClosedTrade {
        ClosedTrade {
            external_id: format!("{}:{}:{}", coin, self.entry_oid, self.closed_at_ms),
            coin: coin.to_string(),
            direction: self.direction,
            size: self.open_size,
            entry_px: self.entry_notional / self.open_size,
            exit_px: self.exit_notional / self.close_size,
            realized_pnl: self.realized_pnl,
            fee: self.fee,
            opened_at_ms: self.opened_at_ms,
            closed_at_ms: self.closed_at_ms,
            entry_oid: Some(self.entry_oid),
        }
    }
}

/// Group a chronological fill stream into completed round-trip trades by
/// replaying a running signed position per coin.
///
/// One `ClosedTrade` is emitted per position life-cycle: each time a coin's
/// signed position leaves flat (0) and later returns to flat. While a leg is
/// open, fills are classified against the running position:
///
/// * **Open / scale-in** — moves further from flat. Opening fills accumulate
///   size, notional and fee; `entry_px` is the size-weighted average.
/// * **Reduce / close** — moves toward flat without crossing. Closing fills
///   accumulate exit notional, realized PnL and fee; `exit_px` is the
///   size-weighted average. A partial close keeps the leg open and emits
///   nothing; the trade is emitted only when the position reaches flat.
/// * **Flip (zero-crossing)** — a single fill that crosses through flat is
///   split: the `|pos_before|` portion closes the current leg (which is emitted,
///   with the fill's whole `closed_pnl` and `fee` attributed to it), and the
///   `|new_pos|` portion opens a fresh leg in the opposite direction.
///
/// A still-open leg at the end of the stream emits nothing (an open position is
/// not a completed trade).
///
/// # Assumptions & limits
/// * Side is inferred from the four standard Hyperliquid `dir` strings via
///   [`fill_signed_delta`] ("Open Long" / "Open Short" / "Close Long" /
///   "Close Short").
/// * A closing fill with no matching open leg (the opening fills fell outside
///   a windowed history query) is dropped — it cannot form a complete trade.
/// * Flatness is detected within `EPS = 1e-9`.
pub fn assemble_trades(fills: &[FillDetail]) -> Vec<ClosedTrade> {
    use std::collections::HashMap;

    let mut ordered = fills.to_vec();
    ordered.sort_by_key(|f| f.time_ms);

    let mut positions: HashMap<String, f64> = HashMap::new();
    let mut legs: HashMap<String, Leg> = HashMap::new();
    let mut out = Vec::new();

    for fill in &ordered {
        let pos_before = *positions.get(&fill.coin).unwrap_or(&0.0);
        let delta = fill_signed_delta(&fill.dir, fill.sz);
        let new_pos = pos_before + delta;

        let before_flat = pos_before.abs() < EPS;
        let moving_away = !before_flat && (delta > 0.0) == (pos_before > 0.0);

        if before_flat || moving_away {
            // Opening / scale-in.
            //
            // Guard: when starting from a flat position with no existing leg,
            // only create a new leg for actual OPEN fills. A "Close …" fill that
            // arrives while the position is flat and has no open leg is an
            // orphan close (its opening fills are outside the query window) and
            // must be dropped — do not fabricate a phantom position.
            let has_existing_leg = legs.contains_key(&fill.coin);
            if before_flat && !has_existing_leg && !fill.dir.contains("Open") {
                // Orphan close from flat: skip entirely so `pos` stays flat.
                continue;
            }
            let leg = legs.entry(fill.coin.clone()).or_insert_with(|| Leg {
                direction: if delta > 0.0 { "long".into() } else { "short".into() },
                open_size: 0.0,
                entry_notional: 0.0,
                close_size: 0.0,
                exit_notional: 0.0,
                realized_pnl: 0.0,
                fee: 0.0,
                opened_at_ms: fill.time_ms,
                entry_oid: fill.oid,
                closed_at_ms: fill.time_ms,
            });
            leg.open_size += fill.sz;
            leg.entry_notional += fill.px * fill.sz;
            leg.fee += fill.fee;
        } else if fill.sz <= pos_before.abs() + EPS {
            // Reduction / close without zero-crossing.
            if let Some(mut leg) = legs.remove(&fill.coin) {
                leg.close_size += fill.sz;
                leg.exit_notional += fill.px * fill.sz;
                leg.realized_pnl += fill.closed_pnl;
                leg.fee += fill.fee;
                leg.closed_at_ms = fill.time_ms;

                if new_pos.abs() < EPS {
                    out.push(leg.into_closed_trade(&fill.coin));
                } else {
                    // Partial close — keep the leg open.
                    legs.insert(fill.coin.clone(), leg);
                }
            }
            // else: orphan close (no open leg) → drop.
        } else {
            // Flip (zero-crossing): close the existing leg, open the opposite.
            let closing_portion = pos_before.abs();
            let opening_portion = new_pos.abs();

            if let Some(mut leg) = legs.remove(&fill.coin) {
                leg.close_size += closing_portion;
                leg.exit_notional += fill.px * closing_portion;
                leg.realized_pnl += fill.closed_pnl;
                leg.fee += fill.fee;
                leg.closed_at_ms = fill.time_ms;
                out.push(leg.into_closed_trade(&fill.coin));

                legs.insert(
                    fill.coin.clone(),
                    Leg {
                        direction: if new_pos > 0.0 { "long".into() } else { "short".into() },
                        open_size: opening_portion,
                        entry_notional: fill.px * opening_portion,
                        close_size: 0.0,
                        exit_notional: 0.0,
                        realized_pnl: 0.0,
                        fee: 0.0,
                        opened_at_ms: fill.time_ms,
                        entry_oid: fill.oid,
                        closed_at_ms: fill.time_ms,
                    },
                );
            }
            // else: orphan oversized close (no open leg) → drop entirely.
        }

        positions.insert(fill.coin.clone(), new_pos);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(coin: &str, oid: u64, dir: &str, px: f64, sz: f64, pnl: f64, fee: f64, t: i64) -> FillDetail {
        FillDetail {
            coin: coin.into(), oid, dir: dir.into(), px, sz,
            closed_pnl: pnl, fee, time_ms: t, start_position: 0.0,
        }
    }

    #[test]
    fn assembles_one_long_round_trip() {
        let fills = vec![
            fill("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            fill("ETH", 2, "Close Long", 2100.0, 1.0, 100.0, 1.0, 2000),
        ];
        let trades = assemble_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.coin, "ETH");
        assert_eq!(t.direction, "long");
        assert_eq!(t.size, 1.0);
        assert_eq!(t.entry_px, 2000.0);
        assert_eq!(t.exit_px, 2100.0);
        assert_eq!(t.realized_pnl, 100.0);
        assert_eq!(t.fee, 2.0); // entry + exit fee
        assert_eq!(t.opened_at_ms, 1000);
        assert_eq!(t.closed_at_ms, 2000);
        assert_eq!(t.entry_oid, Some(1));
        assert_eq!(t.external_id, "ETH:1:2000"); // coin:entry_oid:closed_at_ms
    }

    #[test]
    fn open_position_without_close_is_not_a_trade() {
        let fills = vec![fill("BTC", 9, "Open Short", 50000.0, 0.1, 0.0, 0.5, 1000)];
        assert!(assemble_trades(&fills).is_empty());
    }

    #[test]
    fn fill_signed_delta_maps_the_four_dir_strings() {
        assert_eq!(fill_signed_delta("Open Long", 2.0), 2.0);
        assert_eq!(fill_signed_delta("Close Short", 2.0), 2.0);
        assert_eq!(fill_signed_delta("Open Short", 2.0), -2.0);
        assert_eq!(fill_signed_delta("Close Long", 2.0), -2.0);
    }

    #[test]
    fn scale_in_uses_size_weighted_entry() {
        let fills = vec![
            fill("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            fill("ETH", 2, "Open Long", 2100.0, 1.0, 0.0, 1.0, 1500),
            fill("ETH", 3, "Close Long", 2200.0, 2.0, 300.0, 2.0, 2000),
        ];
        let trades = assemble_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.direction, "long");
        assert_eq!(t.size, 2.0);
        assert!((t.entry_px - 2050.0).abs() < 1e-9);
        assert!((t.exit_px - 2200.0).abs() < 1e-9);
        assert_eq!(t.realized_pnl, 300.0);
        assert_eq!(t.fee, 4.0);
        assert_eq!(t.opened_at_ms, 1000);
        assert_eq!(t.closed_at_ms, 2000);
        assert_eq!(t.entry_oid, Some(1));
    }

    #[test]
    fn partial_close_emits_one_trade_on_full_exit() {
        let open = fill("ETH", 1, "Open Long", 2000.0, 2.0, 0.0, 0.0, 1000);
        let close1 = fill("ETH", 2, "Close Long", 2100.0, 1.0, 50.0, 0.0, 2000);
        let close2 = fill("ETH", 3, "Close Long", 2200.0, 1.0, 100.0, 0.0, 3000);

        // After only the open + first partial close: no completed trade yet.
        let partial = assemble_trades(&[open.clone(), close1.clone()]);
        assert!(partial.is_empty());

        let trades = assemble_trades(&[open, close1, close2]);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.size, 2.0);
        assert!((t.entry_px - 2000.0).abs() < 1e-9);
        assert!((t.exit_px - 2150.0).abs() < 1e-9); // size-weighted (2100+2200)/2
        assert_eq!(t.realized_pnl, 150.0);
        assert_eq!(t.closed_at_ms, 3000);
    }

    #[test]
    fn short_round_trip() {
        let fills = vec![
            fill("ETH", 1, "Open Short", 2000.0, 1.0, 0.0, 0.0, 1000),
            fill("ETH", 2, "Close Short", 1900.0, 1.0, 100.0, 0.0, 2000),
        ];
        let trades = assemble_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.direction, "short");
        assert!((t.entry_px - 2000.0).abs() < 1e-9);
        assert!((t.exit_px - 1900.0).abs() < 1e-9);
        assert_eq!(t.realized_pnl, 100.0);
    }

    #[test]
    fn single_fill_flip_closes_then_opens() {
        let mut flip = fill("ETH", 2, "Close Long", 1900.0, 3.0, -100.0, 0.0, 2000);
        flip.start_position = 1.0;
        let fills = vec![
            fill("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 0.0, 1000),
            flip,
            fill("ETH", 3, "Close Short", 1850.0, 2.0, 100.0, 0.0, 3000),
        ];
        let trades = assemble_trades(&fills);
        assert_eq!(trades.len(), 2);

        let first = &trades[0];
        assert_eq!(first.direction, "long");
        assert!((first.size - 1.0).abs() < 1e-9);
        assert!((first.entry_px - 2000.0).abs() < 1e-9);
        assert!((first.exit_px - 1900.0).abs() < 1e-9);
        assert_eq!(first.realized_pnl, -100.0);

        let second = &trades[1];
        assert_eq!(second.direction, "short");
        assert!((second.size - 2.0).abs() < 1e-9);
        assert!((second.entry_px - 1900.0).abs() < 1e-9);
        assert!((second.exit_px - 1850.0).abs() < 1e-9);
        assert_eq!(second.realized_pnl, 100.0);
    }

    #[test]
    fn orphan_close_is_dropped() {
        let fills = vec![fill("ETH", 1, "Close Long", 2100.0, 1.0, 50.0, 0.0, 1000)];
        assert!(assemble_trades(&fills).is_empty());
    }

    /// Two consecutive orphan "Close Long" fills arriving while the position is
    /// flat (no open leg) must produce zero trades. Before the fix they would
    /// fabricate a phantom short leg and emit a spurious short trade.
    #[test]
    fn orphan_close_sequence_is_dropped() {
        let fills = vec![
            fill("ETH", 10, "Close Long", 2100.0, 1.0, 50.0, 1.0, 1000),
            fill("ETH", 11, "Close Long", 2050.0, 1.0, 30.0, 1.0, 2000),
        ];
        assert!(
            assemble_trades(&fills).is_empty(),
            "orphan close fills from flat must not produce any trades"
        );
    }

    /// A normal "Open Short" from flat must still open a leg and a subsequent
    /// "Close Short" must emit a correct short trade — the orphan gate must not
    /// block legitimate opens.
    #[test]
    fn open_short_from_flat_after_orphan_close_is_not_blocked() {
        let fills = vec![
            // Orphan close that must be dropped (no open leg).
            fill("ETH", 10, "Close Long", 2100.0, 1.0, 50.0, 1.0, 1000),
            // Real short round-trip that must still work.
            fill("ETH", 20, "Open Short", 2000.0, 1.0, 0.0, 1.0, 2000),
            fill("ETH", 21, "Close Short", 1900.0, 1.0, 100.0, 1.0, 3000),
        ];
        let trades = assemble_trades(&fills);
        assert_eq!(trades.len(), 1, "exactly one short trade expected");
        let trade = &trades[0];
        assert_eq!(trade.direction, "short");
        assert!((trade.entry_px - 2000.0).abs() < 1e-9);
        assert!((trade.exit_px - 1900.0).abs() < 1e-9);
        assert_eq!(trade.realized_pnl, 100.0);
    }
}
