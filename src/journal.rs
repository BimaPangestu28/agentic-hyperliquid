//! Append-only SQLite log of executed trades.

use crate::sizing::ExecutionPlan;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct Journal {
    connection: Mutex<Connection>,
}

/// Full schema with all signal-metadata and timestamp columns.
/// New databases receive this directly; existing databases are migrated via
/// idempotent `ALTER TABLE` statements in `MIGRATIONS`.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    coin TEXT NOT NULL,
    direction TEXT NOT NULL,
    size REAL NOT NULL,
    entry REAL NOT NULL,
    leverage INTEGER NOT NULL,
    stop_loss REAL NOT NULL,
    entry_order_id INTEGER,
    confidence INTEGER,
    timeframe TEXT,
    risk_reward REAL,
    profile TEXT,
    notional REAL,
    risk_amount REAL,
    opened_at INTEGER
)";

/// Idempotent migrations for existing databases that predate the signal-metadata
/// columns. Each statement is executed and its error is silently ignored —
/// SQLite returns an error if the column already exists, which is expected.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE trades ADD COLUMN confidence INTEGER",
    "ALTER TABLE trades ADD COLUMN timeframe TEXT",
    "ALTER TABLE trades ADD COLUMN risk_reward REAL",
    "ALTER TABLE trades ADD COLUMN profile TEXT",
    "ALTER TABLE trades ADD COLUMN notional REAL",
    "ALTER TABLE trades ADD COLUMN risk_amount REAL",
    "ALTER TABLE trades ADD COLUMN opened_at INTEGER",
    "ALTER TABLE trades ADD COLUMN tp_prices TEXT",
];

/// Signal metadata + timestamp read back from the journal for stats attribution.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub coin: String,
    pub confidence: Option<u8>,
    pub timeframe: Option<String>,
    pub opened_at: i64,
}

/// The bracket prices of a journaled trade, used to label which leg
/// (SL / TP1 / TP2) closed a position when a closing fill is observed.
#[derive(Debug, Clone, PartialEq)]
pub struct Bracket {
    pub stop_loss: f64,
    pub take_profits: Vec<f64>,
}

/// Strategy metadata for one journaled entry, used to enrich exchange-derived
/// round-trip trades. Keyed by the entry order id recorded at submission time.
#[derive(Debug, Clone)]
pub struct TradeMeta {
    pub confidence: Option<u8>,
    pub timeframe: Option<String>,
    pub profile: Option<String>,
    pub leverage: i64,
}

impl Journal {
    fn from_connection(connection: Connection) -> anyhow::Result<Self> {
        connection.execute(SCHEMA, [])?;
        connection.execute(
            "CREATE TABLE IF NOT EXISTS seen_fills (fill_key TEXT PRIMARY KEY)",
            [],
        )?;
        // Run migrations idempotently — ignore "duplicate column" errors on
        // pre-existing databases.
        for migration in MIGRATIONS {
            let _ = connection.execute(migration, []);
        }
        Ok(Self { connection: Mutex::new(connection) })
    }

    pub fn open(path: &str) -> anyhow::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Records an executed trade with its signal metadata and entry timestamp.
    ///
    /// - `confidence`: signal confidence score (0–10), stored as `Option<i64>`.
    /// - `timeframe`: signal timeframe string (e.g. "swing", "scalp").
    /// - `risk_reward`: signal risk:reward ratio.
    /// - `profile`: risk profile label (e.g. "Moderate").
    /// - `opened_at`: UNIX seconds when the trade was submitted.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        plan: &ExecutionPlan,
        entry_order_id: Option<u64>,
        confidence: Option<u8>,
        timeframe: Option<&str>,
        risk_reward: Option<f64>,
        profile: &str,
        opened_at: i64,
    ) -> anyhow::Result<()> {
        let tp_prices: Vec<f64> = plan.take_profits.iter().map(|leg| leg.price).collect();
        let tp_prices_json = serde_json::to_string(&tp_prices).unwrap_or_else(|_| "[]".to_string());
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO trades (coin, direction, size, entry, leverage, stop_loss, entry_order_id,
                                 confidence, timeframe, risk_reward, profile, notional, risk_amount, opened_at, tp_prices)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                plan.coin,
                format!("{:?}", plan.direction),
                plan.size,
                plan.entry,
                plan.leverage,
                plan.stop_loss.price,
                entry_order_id.map(|id| id as i64),
                confidence.map(|c| c as i64),
                timeframe,
                risk_reward,
                profile,
                plan.notional,
                plan.risk_amount,
                opened_at,
                tp_prices_json,
            ],
        )?;
        Ok(())
    }

    /// Returns a map of `entry_order_id -> TradeMeta` for every journaled entry
    /// that has a non-null order id. Used to enrich exchange-derived round-trip
    /// trades with strategy metadata recorded at submission time.
    ///
    /// # Errors
    /// Returns an error if the SQL query fails or a row cannot be decoded.
    pub fn metadata_by_order_id(&self) -> anyhow::Result<HashMap<u64, TradeMeta>> {
        let connection = self.connection.lock().unwrap();
        let mut stmt = connection.prepare(
            "SELECT entry_order_id, confidence, timeframe, profile, leverage
             FROM trades WHERE entry_order_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let order_id: i64 = row.get(0)?;
            Ok((
                order_id as u64,
                TradeMeta {
                    confidence: row.get::<_, Option<i64>>(1)?.map(|v| v as u8),
                    timeframe: row.get(2)?,
                    profile: row.get(3)?,
                    leverage: row.get(4)?,
                },
            ))
        })?;
        let mut map = HashMap::new();
        for row_result in rows {
            let (order_id, trade_meta) = row_result?;
            map.insert(order_id, trade_meta);
        }
        Ok(map)
    }

    /// Sum of `risk_amount` over trades opened at or after `since_ts` (unix seconds).
    /// Used to enforce the daily risk cap. NULL/absent risk_amount counts as 0.
    pub fn risk_used_since(&self, since_ts: i64) -> anyhow::Result<f64> {
        let connection = self.connection.lock().unwrap();
        let total: f64 = connection.query_row(
            "SELECT COALESCE(SUM(risk_amount), 0.0) FROM trades WHERE opened_at >= ?1",
            rusqlite::params![since_ts],
            |row| row.get(0),
        )?;
        Ok(total)
    }

    /// Returns the SL + TP prices of the trade for `coin` (case-insensitive) that was
    /// the most recently opened **at or before** `at_or_before_ms`, or `Ok(None)` if none.
    ///
    /// Used by the fill monitor to label which bracket leg closed a position. Bounding by
    /// the fill's own timestamp matches the close to the trade that was actually live when
    /// it happened — not the globally newest trade for the coin, which may have been opened
    /// by a *later* re-entry and would mislabel the leg (a profit-taking close attributed
    /// to the re-entry's SL, etc.). `opened_at` is unix seconds; NULL (pre-migration rows)
    /// is treated as 0 so it always sorts oldest.
    ///
    /// Limitation: `opened_at` has second granularity, so a re-entry opened in the *same
    /// second* as the prior trade's close could still be picked. The scraper's
    /// post-execute cooldown makes same-coin re-entries that close are spaced well apart,
    /// so this edge does not arise in practice.
    pub fn bracket_for_coin_at(&self, coin: &str, at_or_before_ms: i64) -> anyhow::Result<Option<Bracket>> {
        let at_or_before_secs = at_or_before_ms / 1000;
        let connection = self.connection.lock().unwrap();
        let row = connection.query_row(
            "SELECT stop_loss, tp_prices FROM trades
             WHERE coin = ?1 COLLATE NOCASE AND COALESCE(opened_at, 0) <= ?2
             ORDER BY COALESCE(opened_at, 0) DESC, id DESC LIMIT 1",
            rusqlite::params![coin, at_or_before_secs],
            |row| {
                let stop_loss: f64 = row.get(0)?;
                let tp_json: Option<String> = row.get(1)?;
                Ok((stop_loss, tp_json))
            },
        );
        match row {
            Ok((stop_loss, tp_json)) => {
                let take_profits = match tp_json.as_deref() {
                    Some(raw) => match serde_json::from_str::<Vec<f64>>(raw) {
                        Ok(prices) => prices,
                        Err(error) => {
                            tracing::warn!(coin, raw, %error, "malformed tp_prices JSON; treating as no take-profits");
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                };
                Ok(Some(Bracket { stop_loss, take_profits }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns all journaled trade records ordered by entry time, for stats attribution.
    pub fn all_trades(&self) -> anyhow::Result<Vec<TradeRecord>> {
        let connection = self.connection.lock().unwrap();
        let mut stmt = connection.prepare(
            "SELECT coin, confidence, timeframe, opened_at FROM trades ORDER BY opened_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TradeRecord {
                coin: row.get(0)?,
                confidence: row.get::<_, Option<i64>>(1)?.map(|v| v as u8),
                timeframe: row.get(2)?,
                opened_at: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Returns true if `fill_key` has already been recorded as notified.
    pub fn is_fill_seen(&self, fill_key: &str) -> anyhow::Result<bool> {
        let connection = self.connection.lock().unwrap();
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM seen_fills WHERE fill_key = ?1",
            rusqlite::params![fill_key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Records `fill_key` as notified. Idempotent (INSERT OR IGNORE).
    pub fn mark_fill_seen(&self, fill_key: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT OR IGNORE INTO seen_fills (fill_key) VALUES (?1)",
            rusqlite::params![fill_key],
        )?;
        Ok(())
    }

    /// Returns true when no fills have been recorded yet — used once at monitor
    /// startup to baseline historical fills silently on a brand-new database.
    pub fn seen_fills_empty(&self) -> anyhow::Result<bool> {
        let connection = self.connection.lock().unwrap();
        let count: i64 = connection.query_row("SELECT COUNT(*) FROM seen_fills", [], |row| row.get(0))?;
        Ok(count == 0)
    }

    #[cfg(test)]
    fn count(&self) -> anyhow::Result<i64> {
        let connection = self.connection.lock().unwrap();
        Ok(connection.query_row("SELECT COUNT(*) FROM trades", [], |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Direction;
    use crate::sizing::BracketLeg;

    #[test]
    fn metadata_by_order_id_maps_entry_orders() {
        use crate::sizing::BracketLeg;

        let journal = Journal::open_in_memory().unwrap();
        let plan = ExecutionPlan {
            coin: "ETH".into(),
            direction: Direction::Long,
            size: 1.0,
            entry: 2000.0,
            leverage: 5,
            notional: 2000.0,
            margin: 400.0,
            risk_amount: 100.0,
            liquidation_price: 1600.0,
            stop_loss: BracketLeg { price: 1900.0, size: 1.0 },
            take_profits: vec![],
            warnings: vec![],
        };
        // record with entry_order_id = Some(42) and leverage 5
        journal
            .record(&plan, Some(42), None, None, None, "Moderate", 1_700_000_000)
            .unwrap();
        let map = journal.metadata_by_order_id().unwrap();
        let trade_meta = map.get(&42).expect("entry 42 must be present");
        assert_eq!(trade_meta.leverage, 5);
    }

    #[test]
    fn records_a_trade_row() {
        let journal = Journal::open_in_memory().unwrap();
        let plan = ExecutionPlan {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            size: 100.0,
            entry: 1.40,
            leverage: 3,
            notional: 140.0,
            margin: 46.6,
            risk_amount: 100.0,
            liquidation_price: 0.93,
            stop_loss: BracketLeg { price: 1.25, size: 100.0 },
            take_profits: vec![],
            warnings: vec![],
        };
        journal.record(&plan, Some(42), Some(8), Some("swing"), Some(2.8), "Moderate", 1_700_000_000).unwrap();
        assert_eq!(journal.count().unwrap(), 1);
    }

    #[test]
    fn bracket_for_coin_at_returns_sl_and_tps() {
        let journal = Journal::open_in_memory().unwrap();
        let plan = ExecutionPlan {
            coin: "TAO".into(),
            direction: Direction::Long,
            size: 10.0,
            entry: 300.0,
            leverage: 3,
            notional: 3000.0,
            margin: 1000.0,
            risk_amount: 50.0,
            liquidation_price: 200.0,
            stop_loss: BracketLeg { price: 280.0, size: 10.0 },
            take_profits: vec![
                BracketLeg { price: 340.0, size: 6.0 },
                BracketLeg { price: 380.0, size: 4.0 },
            ],
            warnings: vec![],
        };
        let opened_at = 1_700_000_000_i64;
        journal.record(&plan, None, None, None, None, "Moderate", opened_at).unwrap();

        // A fill that happens after the entry resolves to that trade's bracket.
        let bracket = journal
            .bracket_for_coin_at("TAO", (opened_at + 60) * 1000)
            .unwrap()
            .expect("bracket present");
        assert_eq!(bracket.stop_loss, 280.0);
        assert_eq!(bracket.take_profits, vec![340.0, 380.0]);
        assert!(journal.bracket_for_coin_at("ETH", (opened_at + 60) * 1000).unwrap().is_none());
    }

    #[test]
    fn bracket_for_coin_at_ignores_a_later_reentry() {
        // Regression: an old trade's close fill must be labelled against the OLD bracket,
        // not a re-entry opened after the fill (which previously stole the label).
        let journal = Journal::open_in_memory().unwrap();
        let make = |sl: f64, tp: f64| ExecutionPlan {
            coin: "HYPE".into(),
            direction: Direction::Short,
            size: 1.0,
            entry: 63.5,
            leverage: 3,
            notional: 63.5,
            margin: 21.0,
            risk_amount: 1.0,
            liquidation_price: 80.0,
            stop_loss: BracketLeg { price: sl, size: 1.0 },
            take_profits: vec![BracketLeg { price: tp, size: 1.0 }],
            warnings: vec![],
        };
        let first_open = 1_700_000_000_i64;
        let reentry_open = first_open + 200; // opened later, after the first trade's close
        journal.record(&make(64.5, 62.5), None, None, None, None, "Moderate", first_open).unwrap();
        journal.record(&make(65.0, 63.0), None, None, None, None, "Moderate", reentry_open).unwrap();

        // A fill timestamped between the two entries must resolve to the FIRST bracket.
        let fill_ms = (first_open + 100) * 1000;
        let bracket = journal.bracket_for_coin_at("HYPE", fill_ms).unwrap().expect("bracket present");
        assert_eq!(bracket.stop_loss, 64.5);
        assert_eq!(bracket.take_profits, vec![62.5]);
    }

    #[test]
    fn seen_fills_dedup_lifecycle() {
        let journal = Journal::open_in_memory().unwrap();
        assert!(journal.seen_fills_empty().unwrap(), "fresh db has no seen fills");
        assert!(!journal.is_fill_seen("123:1000:300.0:10.0").unwrap());

        journal.mark_fill_seen("123:1000:300.0:10.0").unwrap();
        assert!(journal.is_fill_seen("123:1000:300.0:10.0").unwrap());
        assert!(!journal.seen_fills_empty().unwrap());

        // Idempotent: marking the same key twice does not error.
        journal.mark_fill_seen("123:1000:300.0:10.0").unwrap();
    }

    #[test]
    fn risk_used_since_sums_only_trades_at_or_after_cutoff() {
        let journal = Journal::open_in_memory().unwrap();

        // Build a helper closure so we can easily vary risk_amount and opened_at.
        let make_plan = |coin: &str, risk_amount: f64| ExecutionPlan {
            coin: coin.into(),
            direction: Direction::Long,
            size: 10.0,
            entry: 1.0,
            leverage: 3,
            notional: 10.0,
            margin: 3.33,
            risk_amount,
            liquidation_price: 0.66,
            stop_loss: BracketLeg { price: 0.90, size: 10.0 },
            take_profits: vec![],
            warnings: vec![],
        };

        let cutoff: i64 = 1_700_000_000;

        // Trade BEFORE the cutoff — should NOT be counted.
        let plan_before = make_plan("BTC", 25.0);
        journal.record(&plan_before, None, None, None, None, "Moderate", cutoff - 1).unwrap();

        // Two trades AT and AFTER the cutoff — both should be counted.
        let plan_at = make_plan("ETH", 30.0);
        journal.record(&plan_at, None, None, None, None, "Moderate", cutoff).unwrap();

        let plan_after = make_plan("SOL", 15.0);
        journal.record(&plan_after, None, None, None, None, "Moderate", cutoff + 3600).unwrap();

        let risk_used = journal.risk_used_since(cutoff).unwrap();
        assert!(
            (risk_used - 45.0).abs() < 1e-9,
            "expected 45.0 but got {risk_used}"
        );
    }
}
