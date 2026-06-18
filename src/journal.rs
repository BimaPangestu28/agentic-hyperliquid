//! Append-only SQLite log of executed trades.

use crate::sizing::ExecutionPlan;
use rusqlite::Connection;
use std::sync::Mutex;

pub struct Journal {
    connection: Mutex<Connection>,
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    coin TEXT NOT NULL,
    direction TEXT NOT NULL,
    size REAL NOT NULL,
    entry REAL NOT NULL,
    leverage INTEGER NOT NULL,
    stop_loss REAL NOT NULL,
    entry_order_id INTEGER
)";

impl Journal {
    fn from_connection(connection: Connection) -> anyhow::Result<Self> {
        connection.execute(SCHEMA, [])?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    pub fn open(path: &str) -> anyhow::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn record(&self, plan: &ExecutionPlan, entry_order_id: Option<u64>) -> anyhow::Result<()> {
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO trades (coin, direction, size, entry, leverage, stop_loss, entry_order_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                plan.coin,
                format!("{:?}", plan.direction),
                plan.size,
                plan.entry,
                plan.leverage,
                plan.stop_loss.price,
                entry_order_id.map(|id| id as i64),
            ],
        )?;
        Ok(())
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
        journal.record(&plan, Some(42)).unwrap();
        assert_eq!(journal.count().unwrap(), 1);
    }
}
