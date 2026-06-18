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

/// Group a chronological fill stream into completed round-trip trades, one per
/// coin position life-cycle. A trade opens on the first fill that moves the
/// position away from flat and closes on the fill that returns it to flat
/// (identified by a non-zero `closed_pnl`). Still-open positions emit nothing.
///
/// # Known limitations — one-shot, full-size round trips only
///
/// This implementation assumes each position is opened in a single fill and
/// closed in a single fill. The following scenarios are **NOT** faithfully
/// reconstructed:
///
/// 1. **Scale-ins** — when a position is built across multiple opening fills,
///    `entry_px` is the price of the *first* opening fill, not the
///    size-weighted average. Subsequent opening fills only accumulate `size`
///    and `fee`; their prices are discarded.
///
/// 2. **Partial closes** — the first closing fill emits a `ClosedTrade` using
///    the full accumulated open size, and the open leg is removed from state.
///    Any further fills that close the remainder of the position are treated
///    as orphaned close fills (no matching open entry) and are silently
///    dropped, producing an incomplete trade record.
///
/// 3. **Single-fill position flips** — a fill whose `closed_pnl` is non-zero
///    but that also opens a new position in the opposite direction (i.e. the
///    fill crosses through flat) is treated as a *close only*. The new
///    opposite leg is not recorded; it will be missed entirely unless a
///    subsequent opening fill arrives for that coin.
///
/// # Future extension point
///
/// `FillDetail.start_position` is plumbed through from the Hyperliquid API
/// and is the intended hook for a future position-aware version that can
/// correctly weight entry prices across scale-ins and handle partial closes
/// by tracking the residual position size between fills.
pub fn assemble_trades(fills: &[FillDetail]) -> Vec<ClosedTrade> {
    use std::collections::HashMap;

    struct Open {
        entry_oid: u64,
        direction: String,
        size: f64,
        entry_px: f64,
        opened_at_ms: i64,
        fee: f64,
    }

    let mut open: HashMap<String, Open> = HashMap::new();
    let mut out = Vec::new();
    let mut ordered = fills.to_vec();
    ordered.sort_by_key(|f| f.time_ms);

    for f in &ordered {
        let is_close = f.closed_pnl != 0.0 || f.dir.contains("Close");
        if !is_close {
            // Opening (or adding to) a position: record/extend the open leg.
            let entry = open.entry(f.coin.clone()).or_insert(Open {
                entry_oid: f.oid,
                direction: if f.dir.contains("Long") { "long".into() } else { "short".into() },
                size: 0.0,
                entry_px: f.px,
                opened_at_ms: f.time_ms,
                fee: 0.0,
            });
            entry.size += f.sz;
            entry.fee += f.fee;
        } else if let Some(o) = open.remove(&f.coin) {
            out.push(ClosedTrade {
                external_id: format!("{}:{}:{}", f.coin, o.entry_oid, f.time_ms),
                coin: f.coin.clone(),
                direction: o.direction,
                size: o.size,
                entry_px: o.entry_px,
                exit_px: f.px,
                realized_pnl: f.closed_pnl,
                fee: o.fee + f.fee,
                opened_at_ms: o.opened_at_ms,
                closed_at_ms: f.time_ms,
                entry_oid: Some(o.entry_oid),
            });
        }
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
}
