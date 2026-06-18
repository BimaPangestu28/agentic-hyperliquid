//! Aggregates realized performance from exchange fills, attributed to the
//! journaled signal metadata. Pure functions — unit-tested without network.

use crate::hyperliquid::Fill;
use crate::journal::TradeRecord;

#[derive(Debug, Clone, PartialEq)]
pub struct CoinStat {
    pub coin: String,
    pub pnl: f64,
    pub closes: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BucketStat {
    pub label: String,
    pub wins: u32,
    pub losses: u32,
    pub pnl: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub total_pnl: f64,
    pub total_fees: f64,
    pub closed_count: u32,
    pub wins: u32,
    pub losses: u32,
    pub per_coin: Vec<CoinStat>,
    pub by_confidence: Vec<BucketStat>,
    pub by_timeframe: Vec<BucketStat>,
    pub trades_taken: u32,
}

impl Stats {
    pub fn win_rate(&self) -> f64 {
        let decided = self.wins + self.losses;
        if decided == 0 {
            0.0
        } else {
            self.wins as f64 / decided as f64 * 100.0
        }
    }
}

/// Returns true if the fill represents a position close (carries realized PnL).
fn is_close(dir: &str) -> bool {
    dir.to_ascii_lowercase().contains("close")
}

/// For a close on `coin` at `time_ms`, find the most recent journaled trade for
/// that coin opened at or before the fill (best-effort attribution).
fn attribute<'a>(trades: &'a [TradeRecord], coin: &str, time_ms: u64) -> Option<&'a TradeRecord> {
    let fill_sec = (time_ms / 1000) as i64;
    trades
        .iter()
        .filter(|t| t.coin.eq_ignore_ascii_case(coin) && t.opened_at <= fill_sec)
        .max_by_key(|t| t.opened_at)
}

pub fn compute_stats(fills: &[Fill], trades: &[TradeRecord]) -> Stats {
    use std::collections::BTreeMap;
    let mut total_pnl = 0.0;
    let mut total_fees = 0.0;
    let mut closed = 0u32;
    let mut wins = 0u32;
    let mut losses = 0u32;
    let mut per_coin: BTreeMap<String, CoinStat> = BTreeMap::new();
    let mut conf: BTreeMap<String, BucketStat> = BTreeMap::new();
    let mut tf: BTreeMap<String, BucketStat> = BTreeMap::new();

    for f in fills {
        total_fees += f.fee;
        if !is_close(&f.dir) {
            continue;
        }
        closed += 1;
        total_pnl += f.closed_pnl;
        let win = f.closed_pnl > 0.0;
        if win {
            wins += 1;
        } else if f.closed_pnl < 0.0 {
            losses += 1;
        }

        let coin_stat = per_coin.entry(f.coin.clone()).or_insert(CoinStat {
            coin: f.coin.clone(),
            pnl: 0.0,
            closes: 0,
        });
        coin_stat.pnl += f.closed_pnl;
        coin_stat.closes += 1;

        if let Some(trade_record) = attribute(trades, &f.coin, f.time_ms) {
            let confidence_label = trade_record
                .confidence
                .map(|v| format!("{v}/10"))
                .unwrap_or_else(|| "n/a".into());
            let conf_bucket = conf.entry(confidence_label.clone()).or_insert(BucketStat {
                label: confidence_label,
                wins: 0,
                losses: 0,
                pnl: 0.0,
            });
            conf_bucket.pnl += f.closed_pnl;
            if win {
                conf_bucket.wins += 1;
            } else if f.closed_pnl < 0.0 {
                conf_bucket.losses += 1;
            }

            let timeframe_label = trade_record
                .timeframe
                .clone()
                .unwrap_or_else(|| "n/a".into());
            let tf_bucket = tf.entry(timeframe_label.clone()).or_insert(BucketStat {
                label: timeframe_label,
                wins: 0,
                losses: 0,
                pnl: 0.0,
            });
            tf_bucket.pnl += f.closed_pnl;
            if win {
                tf_bucket.wins += 1;
            } else if f.closed_pnl < 0.0 {
                tf_bucket.losses += 1;
            }
        }
    }

    Stats {
        total_pnl,
        total_fees,
        closed_count: closed,
        wins,
        losses,
        per_coin: per_coin.into_values().collect(),
        by_confidence: conf.into_values().collect(),
        by_timeframe: tf.into_values().collect(),
        trades_taken: trades.len() as u32,
    }
}

/// Plain-text performance report (no Markdown — sent without parse_mode).
pub fn render_stats(stats: &Stats) -> String {
    let mut out = String::new();
    out.push_str("Performance (realized, from Hyperliquid)\n");
    out.push_str(&format!(
        "Total PnL: ${:+.2}  (fees ${:.2})\n",
        stats.total_pnl, stats.total_fees
    ));
    out.push_str(&format!(
        "Closed: {}  Win rate: {:.0}% ({}W / {}L)\n",
        stats.closed_count,
        stats.win_rate(),
        stats.wins,
        stats.losses
    ));
    out.push_str(&format!(
        "Signals taken (journaled): {}\n",
        stats.trades_taken
    ));
    if !stats.per_coin.is_empty() {
        out.push_str("\nBy coin:\n");
        for coin_stat in &stats.per_coin {
            out.push_str(&format!(
                "  {:<8} ${:+.2}  ({} closes)\n",
                coin_stat.coin, coin_stat.pnl, coin_stat.closes
            ));
        }
    }
    if !stats.by_confidence.is_empty() {
        out.push_str("\nBy confidence:\n");
        for bucket in &stats.by_confidence {
            out.push_str(&format!(
                "  {:<6} {}W/{}L  ${:+.2}\n",
                bucket.label, bucket.wins, bucket.losses, bucket.pnl
            ));
        }
    }
    if !stats.by_timeframe.is_empty() {
        out.push_str("\nBy timeframe:\n");
        for bucket in &stats.by_timeframe {
            out.push_str(&format!(
                "  {:<8} {}W/{}L  ${:+.2}\n",
                bucket.label, bucket.wins, bucket.losses, bucket.pnl
            ));
        }
    }
    if stats.closed_count == 0 {
        out.push_str(
            "\n(No closed trades yet — stats populate once positions hit TP/SL.)\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyperliquid::Fill;
    use crate::journal::TradeRecord;

    fn trade_record(coin: &str, conf: u8, tf: &str, opened_at: i64) -> TradeRecord {
        TradeRecord {
            coin: coin.into(),
            confidence: Some(conf),
            timeframe: Some(tf.into()),
            opened_at,
        }
    }

    fn fill(coin: &str, pnl: f64, dir: &str, time_ms: u64) -> Fill {
        Fill {
            coin: coin.into(),
            closed_pnl: pnl,
            dir: dir.into(),
            time_ms,
            fee: 0.1,
        }
    }

    #[test]
    fn aggregates_pnl_winrate_and_buckets() {
        let trades = vec![
            trade_record("BTC", 8, "swing", 1000),
            trade_record("SOL", 6, "scalp", 1000),
        ];
        let fills = vec![
            fill("BTC", 0.0, "Open Long", 1_000_000),
            fill("BTC", 50.0, "Close Long", 2_000_000),  // win, conf 8/10 swing
            fill("SOL", -20.0, "Close Long", 2_000_000), // loss, conf 6/10 scalp
        ];
        let stats = compute_stats(&fills, &trades);
        assert_eq!(stats.closed_count, 2);
        assert_eq!(stats.wins, 1);
        assert_eq!(stats.losses, 1);
        assert!((stats.total_pnl - 30.0).abs() < 1e-9);
        assert_eq!(stats.win_rate() as u32, 50);
        assert!(stats
            .by_confidence
            .iter()
            .any(|b| b.label == "8/10" && b.wins == 1));
        assert!(stats
            .by_timeframe
            .iter()
            .any(|b| b.label == "scalp" && b.losses == 1));
    }

    #[test]
    fn no_closes_is_zero() {
        let stats = compute_stats(&[fill("BTC", 0.0, "Open Long", 1)], &[]);
        assert_eq!(stats.closed_count, 0);
        assert_eq!(stats.win_rate(), 0.0);
        assert!(render_stats(&stats).contains("No closed trades"));
    }
}
