//! Runtime-mutable trading settings: the parameters a user can change from
//! Telegram (`/settings`, `/set`). Persistence lives in `SettingsStore` (added
//! in a later task); this module holds the in-memory model and the pure
//! validation logic so it can be unit-tested without any I/O.

use crate::config::LeverageMap;
use crate::sizing::EntryMode;
use rusqlite::Connection;
use std::sync::Mutex;

/// The full set of runtime-tunable trading parameters. Cloned out of the
/// shared lock before use; never mutated in place across an await.
#[derive(Debug, Clone)]
pub struct Settings {
    pub entry_mode: EntryMode,
    pub risk_pct: f64,
    pub entry_pct: f64,
    pub entry_fixed_usd: f64,
    pub max_daily_risk_pct: Option<f64>,
    pub leverage: LeverageMap,
}

impl Settings {
    /// Builds the seed settings from the startup `Config` (env-derived values).
    pub fn from_config(config: &crate::config::Config) -> Settings {
        Settings {
            entry_mode: config.entry_mode,
            risk_pct: config.risk_pct,
            entry_pct: config.entry_pct,
            entry_fixed_usd: config.entry_fixed_usd,
            max_daily_risk_pct: config.max_daily_risk_pct,
            leverage: config.leverage,
        }
    }
}

/// Comma-separated list of valid `/set` keys, used in error messages.
const VALID_KEYS: &str = "entry_mode, risk_pct, entry_pct, entry_fixed_usd, \
max_daily_risk_pct, leverage_conservative, leverage_moderate, leverage_aggressive";

/// Parses a percentage that must be strictly positive and at most 100.
fn parse_percent(value: &str) -> Result<f64, String> {
    let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
    if parsed <= 0.0 || parsed > 100.0 {
        return Err(format!("value must be > 0 and <= 100 (got {parsed})"));
    }
    Ok(parsed)
}

/// Parses a leverage multiplier that must be at least 1.
fn parse_leverage(value: &str) -> Result<u32, String> {
    let parsed: u32 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
    if parsed < 1 {
        return Err("leverage must be >= 1".to_string());
    }
    Ok(parsed)
}

/// Returns a new `Settings` with `key` set to `raw_value`, or an error message
/// suitable for sending straight back to the user. Pure — no I/O, no mutation
/// of `current`.
///
/// `max_daily_risk_pct` accepts an empty string or `0` to disable the cap.
/// `entry_mode` accepts the tokens `risk`, `percent`, `fixed`.
pub fn apply_setting(current: &Settings, key: &str, raw_value: &str) -> Result<Settings, String> {
    let value = raw_value.trim();
    let mut next = current.clone();
    match key {
        "entry_mode" => {
            next.entry_mode = EntryMode::from_str_opt(value)
                .ok_or_else(|| "entry_mode must be one of: risk, percent, fixed".to_string())?;
        }
        "risk_pct" => next.risk_pct = parse_percent(value)?,
        "entry_pct" => next.entry_pct = parse_percent(value)?,
        "entry_fixed_usd" => {
            let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
            if parsed <= 0.0 {
                return Err(format!("entry_fixed_usd must be > 0 (got {parsed})"));
            }
            next.entry_fixed_usd = parsed;
        }
        "max_daily_risk_pct" => {
            next.max_daily_risk_pct = if value.is_empty() || value == "0" {
                None
            } else {
                Some(parse_percent(value)?)
            };
        }
        "leverage_conservative" => next.leverage.conservative = parse_leverage(value)?,
        "leverage_moderate" => next.leverage.moderate = parse_leverage(value)?,
        "leverage_aggressive" => next.leverage.aggressive = parse_leverage(value)?,
        _ => return Err(format!("Unknown setting '{key}'. Valid keys: {VALID_KEYS}")),
    }
    Ok(next)
}

const SETTINGS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
)";

/// SQLite-backed key-value store for `Settings`, sharing the `trades.db` file
/// with the trade journal (separate connection). One row per field.
pub struct SettingsStore {
    connection: Mutex<Connection>,
}

impl SettingsStore {
    fn from_connection(connection: Connection) -> anyhow::Result<Self> {
        connection.execute(SETTINGS_SCHEMA, [])?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    pub fn open(path: &str) -> anyhow::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn put(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.connection.lock().unwrap().execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = statement.query(rusqlite::params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, String>(0)?)),
            None => Ok(None),
        }
    }

    /// Writes every field of `settings` to the table (INSERT OR REPLACE).
    pub fn persist(&self, settings: &Settings) -> anyhow::Result<()> {
        self.put("entry_mode", settings.entry_mode.as_str())?;
        self.put("risk_pct", &settings.risk_pct.to_string())?;
        self.put("entry_pct", &settings.entry_pct.to_string())?;
        self.put("entry_fixed_usd", &settings.entry_fixed_usd.to_string())?;
        let cap = match settings.max_daily_risk_pct {
            Some(value) => value.to_string(),
            None => String::new(),
        };
        self.put("max_daily_risk_pct", &cap)?;
        self.put("leverage_conservative", &settings.leverage.conservative.to_string())?;
        self.put("leverage_moderate", &settings.leverage.moderate.to_string())?;
        self.put("leverage_aggressive", &settings.leverage.aggressive.to_string())?;
        Ok(())
    }

    /// Returns the persisted settings, falling back to `seed` for any missing
    /// key. Missing keys are written back so the table becomes fully populated
    /// (first-boot seeding). After this, the DB is the source of truth.
    pub fn load(&self, seed: Settings) -> anyhow::Result<Settings> {
        let mut resolved = seed;
        if let Some(raw) = self.get("entry_mode")? {
            if let Some(mode) = EntryMode::from_str_opt(&raw) {
                resolved.entry_mode = mode;
            }
        }
        if let Some(raw) = self.get("risk_pct")? {
            match raw.parse() {
                Ok(value) => resolved.risk_pct = value,
                Err(_) => tracing::warn!(key = "risk_pct", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("entry_pct")? {
            match raw.parse() {
                Ok(value) => resolved.entry_pct = value,
                Err(_) => tracing::warn!(key = "entry_pct", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("entry_fixed_usd")? {
            match raw.parse() {
                Ok(value) => resolved.entry_fixed_usd = value,
                Err(_) => tracing::warn!(key = "entry_fixed_usd", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("max_daily_risk_pct")? {
            if raw.trim().is_empty() {
                resolved.max_daily_risk_pct = None;
            } else {
                match raw.parse() {
                    Ok(value) => resolved.max_daily_risk_pct = Some(value),
                    Err(_) => tracing::warn!(key = "max_daily_risk_pct", value = %raw, "failed to parse stored setting; keeping seed value"),
                }
            }
        }
        if let Some(raw) = self.get("leverage_conservative")? {
            match raw.parse() {
                Ok(value) => resolved.leverage.conservative = value,
                Err(_) => tracing::warn!(key = "leverage_conservative", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("leverage_moderate")? {
            match raw.parse() {
                Ok(value) => resolved.leverage.moderate = value,
                Err(_) => tracing::warn!(key = "leverage_moderate", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("leverage_aggressive")? {
            match raw.parse() {
                Ok(value) => resolved.leverage.aggressive = value,
                Err(_) => tracing::warn!(key = "leverage_aggressive", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        // Seed any missing keys and normalize storage.
        self.persist(&resolved)?;
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            entry_mode: EntryMode::RiskBased,
            risk_pct: 1.0,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            max_daily_risk_pct: Some(5.0),
            leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
        }
    }

    #[test]
    fn sets_a_valid_numeric_key() {
        let next = apply_setting(&sample(), "entry_pct", "20").unwrap();
        assert_eq!(next.entry_pct, 20.0);
    }

    #[test]
    fn sets_entry_mode_token() {
        let next = apply_setting(&sample(), "entry_mode", "percent").unwrap();
        assert_eq!(next.entry_mode, EntryMode::PercentBalance);
    }

    #[test]
    fn empty_or_zero_disables_daily_cap() {
        assert_eq!(apply_setting(&sample(), "max_daily_risk_pct", "").unwrap().max_daily_risk_pct, None);
        assert_eq!(apply_setting(&sample(), "max_daily_risk_pct", "0").unwrap().max_daily_risk_pct, None);
    }

    #[test]
    fn rejects_unknown_key() {
        let err = apply_setting(&sample(), "wat", "1").unwrap_err();
        assert!(err.contains("Unknown setting"));
    }

    #[test]
    fn rejects_non_numeric_value() {
        assert!(apply_setting(&sample(), "risk_pct", "abc").is_err());
    }

    #[test]
    fn rejects_percent_out_of_range() {
        assert!(apply_setting(&sample(), "risk_pct", "0").is_err());
        assert!(apply_setting(&sample(), "risk_pct", "150").is_err());
    }

    #[test]
    fn rejects_leverage_below_one() {
        assert!(apply_setting(&sample(), "leverage_moderate", "0").is_err());
    }

    #[test]
    fn load_seeds_from_defaults_when_empty() {
        let store = SettingsStore::open_in_memory().unwrap();
        let resolved = store.load(sample()).unwrap();
        assert_eq!(resolved.risk_pct, 1.0);
        // Seeding wrote the row back.
        assert_eq!(store.get("risk_pct").unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn persisted_value_overrides_seed_on_reload() {
        let store = SettingsStore::open_in_memory().unwrap();
        store.load(sample()).unwrap(); // seed
        let mut changed = sample();
        changed.entry_pct = 25.0;
        changed.entry_mode = EntryMode::PercentBalance;
        store.persist(&changed).unwrap();

        // A reload with a *different* seed must still return the persisted values.
        let mut other_seed = sample();
        other_seed.entry_pct = 99.0;
        let resolved = store.load(other_seed).unwrap();
        assert_eq!(resolved.entry_pct, 25.0);
        assert_eq!(resolved.entry_mode, EntryMode::PercentBalance);
    }

    #[test]
    fn disabled_cap_round_trips_as_none() {
        let store = SettingsStore::open_in_memory().unwrap();
        let mut seed = sample();
        seed.max_daily_risk_pct = None;
        store.persist(&seed).unwrap();
        assert_eq!(store.load(sample()).unwrap().max_daily_risk_pct, None);
    }
}
