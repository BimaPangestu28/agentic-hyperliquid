//! SQLite-backed queue of manual `/scan <COIN>` requests awaiting pickup by the scraper.
//! Shares the journal DB file (separate connection), mirroring `SettingsStore` and
//! `TriggerStore`. The Telegram side enqueues; the HTTP `/manual-scans` endpoint drains.

use rusqlite::Connection;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS manual_scans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    coin TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    status TEXT NOT NULL
)";

pub struct ManualScanStore {
    connection: Mutex<Connection>,
}

/// Current Unix time in seconds (best-effort; 0 if the clock is before the epoch).
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

impl ManualScanStore {
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

    /// Enqueues a coin (upper-cased by the caller) as a pending manual scan; returns its id.
    pub fn enqueue(&self, coin: &str) -> anyhow::Result<i64> {
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO manual_scans (coin, requested_at, status) VALUES (?1, ?2, 'pending')",
            rusqlite::params![coin, now_secs()],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Returns all pending coins (oldest first) and marks them processed in one pass, so
    /// each enqueued request is handed to exactly one scraper poll. Drains the queue.
    pub fn drain_pending(&self) -> anyhow::Result<Vec<String>> {
        let connection = self.connection.lock().unwrap();
        let mut stmt = connection
            .prepare("SELECT id, coin FROM manual_scans WHERE status = 'pending' ORDER BY id ASC")?;
        let pending: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt); // release the borrow on `connection` before the UPDATE writes below
        for (id, _) in &pending {
            connection.execute(
                "UPDATE manual_scans SET status = 'processed' WHERE id = ?1",
                rusqlite::params![id],
            )?;
        }
        Ok(pending.into_iter().map(|(_, coin)| coin).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_then_drain_returns_coins_in_order() {
        let store = ManualScanStore::open_in_memory().unwrap();
        store.enqueue("PENDLE").unwrap();
        store.enqueue("BTC").unwrap();
        assert_eq!(store.drain_pending().unwrap(), vec!["PENDLE", "BTC"]);
    }

    #[test]
    fn drain_is_idempotent_second_call_is_empty() {
        let store = ManualScanStore::open_in_memory().unwrap();
        store.enqueue("SOL").unwrap();
        assert_eq!(store.drain_pending().unwrap(), vec!["SOL"]);
        assert!(store.drain_pending().unwrap().is_empty());
    }

    #[test]
    fn coins_enqueued_after_a_drain_are_returned_by_the_next_drain() {
        let store = ManualScanStore::open_in_memory().unwrap();
        store.enqueue("SOL").unwrap();
        let _ = store.drain_pending().unwrap();
        store.enqueue("ETH").unwrap();
        assert_eq!(store.drain_pending().unwrap(), vec!["ETH"]);
    }
}
