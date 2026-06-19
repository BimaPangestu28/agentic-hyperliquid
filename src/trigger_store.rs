//! SQLite-backed store of pending trigger ENTRY orders awaiting fill or expiry.
//! Shares the journal DB file (separate connection), mirroring `SettingsStore`.

use rusqlite::Connection;
use std::sync::Mutex;

/// One take-profit leg of a pending trigger's bracket (price + % allocation of size).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PendingLeg {
    pub price: f64,
    pub alloc_pct: f64,
}

/// A trigger entry resting on the exchange, with the bracket to arm once it fills.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTrigger {
    pub id: i64,
    pub coin: String,
    pub direction: String, // "Long" | "Short"
    pub size: f64,
    pub trigger_px: f64,
    pub leverage: u32,
    pub stop_loss: f64,
    pub take_profits: Vec<PendingLeg>,
    pub entry_oid: Option<u64>,
    pub chat_id: i64,
    pub created_at: i64,
    pub expiry_at: i64,
    pub status: String, // "active" | "armed" | "expired"
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pending_triggers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    coin TEXT NOT NULL,
    direction TEXT NOT NULL,
    size REAL NOT NULL,
    trigger_px REAL NOT NULL,
    leverage INTEGER NOT NULL,
    stop_loss REAL NOT NULL,
    take_profits TEXT NOT NULL,
    entry_oid INTEGER,
    chat_id INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expiry_at INTEGER NOT NULL,
    status TEXT NOT NULL
)";

pub struct TriggerStore {
    connection: Mutex<Connection>,
}

impl TriggerStore {
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

    /// Inserts a pending trigger (status forced to "active") and returns its row id.
    pub fn insert(&self, trigger: &PendingTrigger) -> anyhow::Result<i64> {
        let take_profits_json = serde_json::to_string(&trigger.take_profits).unwrap_or_else(|_| "[]".to_string());
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO pending_triggers
               (coin, direction, size, trigger_px, leverage, stop_loss, take_profits, entry_oid, chat_id, created_at, expiry_at, status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active')",
            rusqlite::params![
                trigger.coin, trigger.direction, trigger.size, trigger.trigger_px,
                trigger.leverage, trigger.stop_loss, take_profits_json,
                trigger.entry_oid.map(|v| v as i64), trigger.chat_id,
                trigger.created_at, trigger.expiry_at,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    /// Returns all triggers whose status is "active".
    pub fn list_active(&self) -> anyhow::Result<Vec<PendingTrigger>> {
        let connection = self.connection.lock().unwrap();
        let mut stmt = connection.prepare(
            "SELECT id, coin, direction, size, trigger_px, leverage, stop_loss, take_profits,
                    entry_oid, chat_id, created_at, expiry_at, status
             FROM pending_triggers WHERE status = 'active' ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let take_profits_json: String = row.get(7)?;
            let take_profits = serde_json::from_str::<Vec<PendingLeg>>(&take_profits_json).unwrap_or_default();
            Ok(PendingTrigger {
                id: row.get(0)?,
                coin: row.get(1)?,
                direction: row.get(2)?,
                size: row.get(3)?,
                trigger_px: row.get(4)?,
                leverage: row.get::<_, i64>(5)? as u32,
                stop_loss: row.get(6)?,
                take_profits,
                entry_oid: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                chat_id: row.get(9)?,
                created_at: row.get(10)?,
                expiry_at: row.get(11)?,
                status: row.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn mark_armed(&self, id: i64) -> anyhow::Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE pending_triggers SET status = 'armed' WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    pub fn mark_expired(&self, id: i64) -> anyhow::Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE pending_triggers SET status = 'expired' WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PendingTrigger {
        PendingTrigger {
            id: 0, coin: "SOL".into(), direction: "Long".into(), size: 0.5, trigger_px: 68.53,
            leverage: 3, stop_loss: 68.02,
            take_profits: vec![PendingLeg { price: 69.42, alloc_pct: 60.0 }, PendingLeg { price: 70.88, alloc_pct: 40.0 }],
            entry_oid: Some(7), chat_id: 123, created_at: 1000, expiry_at: 1000 + 14400, status: "active".into(),
        }
    }

    #[test]
    fn insert_and_list_active_round_trip() {
        let store = TriggerStore::open_in_memory().unwrap();
        let id = store.insert(&sample()).unwrap();
        let active = store.list_active().unwrap();
        assert_eq!(active.len(), 1);
        let got = &active[0];
        assert_eq!(got.id, id);
        assert_eq!(got.coin, "SOL");
        assert_eq!(got.take_profits, sample().take_profits);
        assert_eq!(got.entry_oid, Some(7));
    }

    #[test]
    fn mark_armed_and_expired_drop_from_active() {
        let store = TriggerStore::open_in_memory().unwrap();
        let a = store.insert(&sample()).unwrap();
        let b = store.insert(&sample()).unwrap();
        store.mark_armed(a).unwrap();
        store.mark_expired(b).unwrap();
        assert!(store.list_active().unwrap().is_empty());
    }
}
