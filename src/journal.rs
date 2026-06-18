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
];

/// Signal metadata + timestamp read back from the journal for stats attribution.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub coin: String,
    pub confidence: Option<u8>,
    pub timeframe: Option<String>,
    pub opened_at: i64,
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
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO trades (coin, direction, size, entry, leverage, stop_loss, entry_order_id,
                                 confidence, timeframe, risk_reward, profile, notional, risk_amount, opened_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
