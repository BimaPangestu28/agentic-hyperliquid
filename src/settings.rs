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
    /// Seconds to wait for a limit entry to fill before cancelling.
    pub entry_fill_timeout_secs: u64,
    /// Seconds an unfilled trigger entry rests before auto-cancellation.
    pub trigger_expiry_secs: u64,
    /// Seconds between background P&L push updates (0 disables).
    pub pnl_push_secs: u64,
    /// Coins the auto-scalp scraper scans, upper-cased.
    pub watchlist: Vec<String>,
    /// Master kill-switch for auto-scalp execution (default false).
    pub auto_scalp_enabled: bool,
    /// Max concurrent open positions the auto-scalp loop may hold.
    pub max_open_positions: u32,
    /// Minimum reward:risk a setup must clear to be auto-executed. `0.0` disables
    /// the gate (default), preserving prior behavior until the user opts in.
    pub min_rr: f64,
    /// Coins barred from auto-execution entirely, upper-cased. Empty by default.
    pub coin_blacklist: Vec<String>,
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
            entry_fill_timeout_secs: config.entry_fill_timeout_secs,
            trigger_expiry_secs: config.trigger_expiry_secs,
            pnl_push_secs: config.pnl_push_secs,
            watchlist: Vec::new(),
            auto_scalp_enabled: false,
            max_open_positions: 5,
            min_rr: 0.0,
            coin_blacklist: Vec::new(),
        }
    }
}

/// Comma-separated list of valid `/set` keys, used in error messages.
const VALID_KEYS: &str = "entry_mode, risk_pct, entry_pct, entry_fixed_usd, \
max_daily_risk_pct, entry_fill_timeout_secs, trigger_expiry_secs, leverage_conservative, leverage_moderate, leverage_aggressive, pnl_push_secs, auto_scalp_enabled, max_open_positions, min_rr, coin_blacklist";

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

/// Parses a fill-timeout in seconds; must be a whole number of at least 1.
fn parse_timeout_secs(value: &str) -> Result<u64, String> {
    let parsed: u64 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
    if parsed < 1 {
        return Err("entry_fill_timeout_secs must be >= 1".to_string());
    }
    Ok(parsed)
}

/// Parses a minimum reward:risk ratio; must be a number of at least 0
/// (`0` disables the gate).
fn parse_min_rr(value: &str) -> Result<f64, String> {
    let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
    if parsed < 0.0 {
        return Err(format!("min_rr must be >= 0 (got {parsed})"));
    }
    Ok(parsed)
}

/// Parses a comma-separated coin blacklist into trimmed, upper-cased, de-duplicated
/// symbols. An empty string clears the list.
fn parse_coin_blacklist(value: &str) -> Vec<String> {
    let mut coins: Vec<String> = Vec::new();
    for raw in value.split(',') {
        let coin = raw.trim().to_uppercase();
        if !coin.is_empty() && !coins.contains(&coin) {
            coins.push(coin);
        }
    }
    coins
}

/// Parses a trigger expiry in seconds; whole number of at least 1.
fn parse_trigger_expiry_secs(value: &str) -> Result<u64, String> {
    let parsed: u64 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
    if parsed < 1 {
        return Err("trigger_expiry_secs must be >= 1".to_string());
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
        "entry_fill_timeout_secs" => next.entry_fill_timeout_secs = parse_timeout_secs(value)?,
        "trigger_expiry_secs" => next.trigger_expiry_secs = parse_trigger_expiry_secs(value)?,
        "pnl_push_secs" => {
            next.pnl_push_secs = value.parse::<u64>().map_err(|_| format!("'{value}' is not a whole number"))?;
        }
        "auto_scalp_enabled" => {
            next.auto_scalp_enabled = match value {
                "true" | "on" | "1" => true,
                "false" | "off" | "0" => false,
                _ => return Err("auto_scalp_enabled must be true or false".to_string()),
            };
        }
        "max_open_positions" => {
            let parsed: u32 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
            if parsed < 1 {
                return Err("max_open_positions must be >= 1".to_string());
            }
            next.max_open_positions = parsed;
        }
        "min_rr" => next.min_rr = parse_min_rr(value)?,
        "coin_blacklist" => next.coin_blacklist = parse_coin_blacklist(value),
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
        self.put("entry_fill_timeout_secs", &settings.entry_fill_timeout_secs.to_string())?;
        self.put("trigger_expiry_secs", &settings.trigger_expiry_secs.to_string())?;
        self.put("pnl_push_secs", &settings.pnl_push_secs.to_string())?;
        self.put("watchlist", &settings.watchlist.join(","))?;
        self.put("auto_scalp_enabled", &settings.auto_scalp_enabled.to_string())?;
        self.put("max_open_positions", &settings.max_open_positions.to_string())?;
        self.put("min_rr", &settings.min_rr.to_string())?;
        self.put("coin_blacklist", &settings.coin_blacklist.join(","))?;
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
        if let Some(raw) = self.get("entry_fill_timeout_secs")? {
            match raw.parse() {
                Ok(value) => resolved.entry_fill_timeout_secs = value,
                Err(_) => tracing::warn!(key = "entry_fill_timeout_secs", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("trigger_expiry_secs")? {
            match raw.parse() {
                Ok(value) => resolved.trigger_expiry_secs = value,
                Err(_) => tracing::warn!(key = "trigger_expiry_secs", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("pnl_push_secs")? {
            match raw.parse() {
                Ok(value) => resolved.pnl_push_secs = value,
                Err(_) => tracing::warn!(key = "pnl_push_secs", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("watchlist")? {
            resolved.watchlist = raw.split(',').map(|c| c.trim().to_uppercase())
                .filter(|c| !c.is_empty()).collect();
        }
        if let Some(raw) = self.get("auto_scalp_enabled")? {
            resolved.auto_scalp_enabled = matches!(raw.as_str(), "true" | "on" | "1");
        }
        if let Some(raw) = self.get("max_open_positions")? {
            if let Ok(value) = raw.parse() { resolved.max_open_positions = value; }
        }
        if let Some(raw) = self.get("min_rr")? {
            match raw.parse() {
                Ok(value) => resolved.min_rr = value,
                Err(_) => tracing::warn!(key = "min_rr", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("coin_blacklist")? {
            resolved.coin_blacklist = parse_coin_blacklist(&raw);
        }
        // Seed any missing keys and normalize storage.
        self.persist(&resolved)?;
        Ok(resolved)
    }
}

/// Test-only seed `Settings`, reused across module test suites.
#[cfg(test)]
pub fn sample() -> Settings {
    Settings {
        entry_mode: EntryMode::RiskBased,
        risk_pct: 1.0,
        entry_pct: 10.0,
        entry_fixed_usd: 50.0,
        max_daily_risk_pct: Some(5.0),
        leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
        entry_fill_timeout_secs: 300,
        trigger_expiry_secs: 14400,
        pnl_push_secs: 900,
        watchlist: Vec::new(),
        auto_scalp_enabled: false,
        max_open_positions: 5,
        min_rr: 0.0,
        coin_blacklist: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn sets_entry_fill_timeout_secs() {
        let next = apply_setting(&sample(), "entry_fill_timeout_secs", "1800").unwrap();
        assert_eq!(next.entry_fill_timeout_secs, 1800);
    }

    #[test]
    fn rejects_zero_timeout() {
        assert!(apply_setting(&sample(), "entry_fill_timeout_secs", "0").is_err());
        assert!(apply_setting(&sample(), "entry_fill_timeout_secs", "abc").is_err());
    }

    #[test]
    fn timeout_persists_and_reloads() {
        let store = SettingsStore::open_in_memory().unwrap();
        store.load(sample()).unwrap();
        let mut changed = sample();
        changed.entry_fill_timeout_secs = 1800;
        store.persist(&changed).unwrap();
        assert_eq!(store.load(sample()).unwrap().entry_fill_timeout_secs, 1800);
    }

    #[test]
    fn sets_trigger_expiry_secs() {
        let next = apply_setting(&sample(), "trigger_expiry_secs", "7200").unwrap();
        assert_eq!(next.trigger_expiry_secs, 7200);
    }

    #[test]
    fn rejects_zero_trigger_expiry() {
        assert!(apply_setting(&sample(), "trigger_expiry_secs", "0").is_err());
        assert!(apply_setting(&sample(), "trigger_expiry_secs", "abc").is_err());
    }

    #[test]
    fn trigger_expiry_persists_and_reloads() {
        let store = SettingsStore::open_in_memory().unwrap();
        store.load(sample()).unwrap();
        let mut changed = sample();
        changed.trigger_expiry_secs = 7200;
        store.persist(&changed).unwrap();
        assert_eq!(store.load(sample()).unwrap().trigger_expiry_secs, 7200);
    }

    #[test]
    fn sets_pnl_push_secs_including_zero() {
        assert_eq!(apply_setting(&sample(), "pnl_push_secs", "300").unwrap().pnl_push_secs, 300);
        assert_eq!(apply_setting(&sample(), "pnl_push_secs", "0").unwrap().pnl_push_secs, 0);
    }

    #[test]
    fn rejects_non_numeric_pnl_push_secs() {
        assert!(apply_setting(&sample(), "pnl_push_secs", "abc").is_err());
    }

    #[test]
    fn pnl_push_secs_persists_and_reloads() {
        let store = SettingsStore::open_in_memory().unwrap();
        store.load(sample()).unwrap();
        let mut changed = sample();
        changed.pnl_push_secs = 300;
        store.persist(&changed).unwrap();
        assert_eq!(store.load(sample()).unwrap().pnl_push_secs, 300);
    }

    #[test]
    fn apply_setting_toggles_auto_scalp_and_cap() {
        let base = sample();
        let on = apply_setting(&base, "auto_scalp_enabled", "true").unwrap();
        assert!(on.auto_scalp_enabled);
        let off = apply_setting(&on, "auto_scalp_enabled", "false").unwrap();
        assert!(!off.auto_scalp_enabled);
        let capped = apply_setting(&base, "max_open_positions", "3").unwrap();
        assert_eq!(capped.max_open_positions, 3);
        assert!(apply_setting(&base, "max_open_positions", "0").is_err());
    }

    #[test]
    fn sets_min_rr_and_rejects_negative() {
        assert_eq!(apply_setting(&sample(), "min_rr", "1.5").unwrap().min_rr, 1.5);
        assert_eq!(apply_setting(&sample(), "min_rr", "0").unwrap().min_rr, 0.0);
        assert!(apply_setting(&sample(), "min_rr", "-1").is_err());
        assert!(apply_setting(&sample(), "min_rr", "abc").is_err());
    }

    #[test]
    fn sets_coin_blacklist_uppercased_and_deduped() {
        let next = apply_setting(&sample(), "coin_blacklist", " xpl, zec ,XPL").unwrap();
        assert_eq!(next.coin_blacklist, vec!["XPL".to_string(), "ZEC".to_string()]);
        // Empty value clears the list.
        assert!(apply_setting(&next, "coin_blacklist", "").unwrap().coin_blacklist.is_empty());
    }

    #[test]
    fn persist_load_roundtrips_min_rr_and_blacklist() {
        let store = SettingsStore::open_in_memory().unwrap();
        let mut settings = sample();
        settings.min_rr = 1.8;
        settings.coin_blacklist = vec!["XPL".into(), "ZEC".into()];
        store.persist(&settings).unwrap();
        let loaded = store.load(sample()).unwrap();
        assert_eq!(loaded.min_rr, 1.8);
        assert_eq!(loaded.coin_blacklist, vec!["XPL".to_string(), "ZEC".to_string()]);
    }

    #[test]
    fn persist_load_roundtrips_watchlist_and_flags() {
        let store = SettingsStore::open_in_memory().unwrap();
        let mut settings = sample();
        settings.watchlist = vec!["BTC".into(), "ETH".into()];
        settings.auto_scalp_enabled = true;
        settings.max_open_positions = 4;
        store.persist(&settings).unwrap();
        let loaded = store.load(sample()).unwrap();
        assert_eq!(loaded.watchlist, vec!["BTC".to_string(), "ETH".to_string()]);
        assert!(loaded.auto_scalp_enabled);
        assert_eq!(loaded.max_open_positions, 4);
    }
}
