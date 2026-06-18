# Telegram-Configurable Settings + Entry Sizing Modes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add percent-of-balance and fixed-USD entry sizing modes alongside the existing risk-based sizing, and make the trading parameters editable at runtime via Telegram (`/settings`, `/set`) with SQLite persistence seeded from env.

**Architecture:** A new `EntryMode` enum drives a branch in `sizing::build_plan`. A new `settings` module holds a runtime-mutable `Settings` struct, a pure `apply_setting` validator, and a SQLite-backed `SettingsStore` (key-value rows in the existing `trades.db`, seeded from `Config` on first boot). `BotContext` holds `Arc<Mutex<Settings>>` + `Arc<SettingsStore>`; sizing call sites and the daily-cap check read from `Settings` instead of `Config`. New `/settings` and `/set` handlers and entry-mode callback buttons mutate and persist settings.

**Tech Stack:** Rust 2021, teloxide 0.13, rusqlite 0.32 (bundled), tokio, anyhow, thiserror.

## Global Constraints

- Settings are **global** (single trader; allowlist gates access). No per-user state.
- Env vars are the **seed/default** only; once a key exists in the `settings` table the DB is authoritative.
- Minimum order notional is **$10** (`sizing::MIN_ORDER_NOTIONAL`); sub-minimum entries are rejected.
- `plan.risk_amount` is the **actual** risk = `size × stop_distance` in every mode.
- Never hold a `std::sync::Mutex` guard across an `.await`: clone `Settings` out of the lock, drop the guard, then await.
- Follow existing patterns: SQLite stores mirror `Journal` (`Mutex<Connection>`, `open` / `open_in_memory`); pure logic lives in free functions with unit tests; Telegram replies that aren't MarkdownV2 send plain text (no escaping).

---

### Task 1: Entry sizing modes in `sizing.rs`

**Files:**
- Modify: `src/sizing.rs`

**Interfaces:**
- Produces: `pub enum EntryMode { RiskBased, PercentBalance, FixedUsd }` with `fn as_str(&self) -> &'static str`, `fn from_str_opt(s: &str) -> Option<EntryMode>`, `fn label(&self) -> &'static str`. `SizingInput` gains `pub entry_mode: EntryMode`, `pub entry_pct: f64`, `pub entry_fixed_usd: f64`. `build_plan` keeps its signature; `ExecutionPlan.risk_amount` becomes actual risk.

- [ ] **Step 1: Add the `EntryMode` enum + helpers**

In `src/sizing.rs`, after the `RiskProfile` enum, add:

```rust
/// How the position size is determined for a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryMode {
    /// Size from risk: `equity × risk_pct/100 ÷ stop_distance`.
    RiskBased,
    /// Size so notional equals `equity × entry_pct/100`.
    PercentBalance,
    /// Size so notional equals a fixed USD amount.
    FixedUsd,
}

impl EntryMode {
    /// Stable token used for persistence and callback data.
    pub fn as_str(&self) -> &'static str {
        match self {
            EntryMode::RiskBased => "risk",
            EntryMode::PercentBalance => "percent",
            EntryMode::FixedUsd => "fixed",
        }
    }

    /// Parses the token produced by `as_str`; `None` for anything else.
    pub fn from_str_opt(token: &str) -> Option<EntryMode> {
        match token {
            "risk" => Some(EntryMode::RiskBased),
            "percent" => Some(EntryMode::PercentBalance),
            "fixed" => Some(EntryMode::FixedUsd),
            _ => None,
        }
    }

    /// Human-readable label for the settings UI.
    pub fn label(&self) -> &'static str {
        match self {
            EntryMode::RiskBased => "Risk-based",
            EntryMode::PercentBalance => "% Balance",
            EntryMode::FixedUsd => "Fixed USD",
        }
    }
}
```

- [ ] **Step 2: Add fields to `SizingInput`**

Modify the `SizingInput` struct to add the three new fields:

```rust
#[derive(Debug)]
pub struct SizingInput<'a> {
    pub setup: &'a TradeSetup,
    pub equity: f64,
    pub risk_pct: f64,
    pub entry_mode: EntryMode,
    pub entry_pct: f64,
    pub entry_fixed_usd: f64,
    pub profile: RiskProfile,
    pub leverage: &'a LeverageMap,
    pub asset_meta: &'a AssetMeta,
}
```

- [ ] **Step 3: Branch the size computation in `build_plan`**

In `build_plan`, find the block:

```rust
    let risk_amount = input.equity * (input.risk_pct / 100.0);
    let raw_size = risk_amount / stop_distance;
    let size = floor_to(raw_size, input.asset_meta.sz_decimals);
    let notional = size * setup.entry;
    if notional < MIN_ORDER_NOTIONAL {
        return Err(SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }
```

Replace it with:

```rust
    let raw_size = match input.entry_mode {
        EntryMode::RiskBased => (input.equity * (input.risk_pct / 100.0)) / stop_distance,
        EntryMode::PercentBalance => (input.equity * (input.entry_pct / 100.0)) / setup.entry,
        EntryMode::FixedUsd => input.entry_fixed_usd / setup.entry,
    };
    let size = floor_to(raw_size, input.asset_meta.sz_decimals);
    let notional = size * setup.entry;
    if notional < MIN_ORDER_NOTIONAL {
        return Err(SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }
    // Actual risk = what we lose if the stop hits, for all modes.
    let risk_amount = size * stop_distance;
```

(The stop-side and zero-distance validations above this block are unchanged and still apply to every mode — they guard the SL/TP bracket that every mode places.)

- [ ] **Step 4: Update existing test literals + the risk-based assertion**

Every `SizingInput { … }` literal in the `#[cfg(test)] mod tests` block now needs the three new fields. Add `entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0,` to **each** `SizingInput` literal in: `computes_risk_based_size_and_brackets`, `rejects_stop_on_wrong_side_for_long`, `rejects_zero_stop_distance`, `rejects_leverage_over_asset_cap`, `rejects_below_minimum_notional`, `unknown_max_leverage_skips_cap_check`, `warns_when_liquidation_is_tighter_than_stop`.

In `computes_risk_based_size_and_brackets`, change the risk assertion (size 666.6 × distance 0.15 = 99.99, not 100.0):

```rust
        // risk = size 666.6 × stop_distance 0.15 = 99.99 (actual, post-floor)
        assert!((plan.risk_amount - 99.99).abs() < 1e-6);
```

- [ ] **Step 5: Add tests for the new modes**

Add these tests inside the `tests` module:

```rust
    #[test]
    fn percent_balance_sizes_from_equity_notional() {
        let setup = pendle_setup(); // entry 1.40
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::PercentBalance,
            entry_pct: 10.0, // target notional = 1000
            entry_fixed_usd: 50.0,
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        let plan = build_plan(&input).expect("should size");
        // raw size = 1000 / 1.40 = 714.28.. -> floor 1 decimal = 714.2
        assert_eq!(plan.size, 714.2);
        // risk_amount = 714.2 × 0.15 = 107.13
        assert!((plan.risk_amount - 107.13).abs() < 1e-6);
    }

    #[test]
    fn fixed_usd_sizes_from_notional() {
        let setup = pendle_setup(); // entry 1.40
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::FixedUsd,
            entry_pct: 10.0,
            entry_fixed_usd: 100.0, // target notional = 100
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        let plan = build_plan(&input).expect("should size");
        // raw size = 100 / 1.40 = 71.42.. -> floor 1 decimal = 71.4
        assert_eq!(plan.size, 71.4);
    }

    #[test]
    fn fixed_usd_below_min_notional_is_rejected() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::FixedUsd,
            entry_pct: 10.0,
            entry_fixed_usd: 5.0, // $5 notional < $10 minimum
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }

    #[test]
    fn entry_mode_token_round_trips() {
        for mode in [EntryMode::RiskBased, EntryMode::PercentBalance, EntryMode::FixedUsd] {
            assert_eq!(EntryMode::from_str_opt(mode.as_str()), Some(mode));
        }
        assert_eq!(EntryMode::from_str_opt("nope"), None);
    }
```

- [ ] **Step 6: Run tests**

Run: `cargo test --lib sizing 2>&1 | tail -20`
Expected: PASS (all sizing tests, including the 4 new ones).

- [ ] **Step 7: Commit**

```bash
git add src/sizing.rs
git commit -m "feat(sizing): add percent-balance and fixed-USD entry modes"
```

---

### Task 2: `Settings` struct + pure `apply_setting` validator

**Files:**
- Create: `src/settings.rs`
- Modify: `src/main.rs` (register `mod settings;`)

**Interfaces:**
- Consumes: `crate::sizing::EntryMode`, `crate::config::LeverageMap`.
- Produces: `pub struct Settings { entry_mode: EntryMode, risk_pct: f64, entry_pct: f64, entry_fixed_usd: f64, max_daily_risk_pct: Option<f64>, leverage: LeverageMap }` (derives `Debug, Clone`); `pub fn apply_setting(current: &Settings, key: &str, raw_value: &str) -> Result<Settings, String>`.

- [ ] **Step 1: Create `src/settings.rs` with the struct + validator**

```rust
//! Runtime-mutable trading settings: the parameters a user can change from
//! Telegram (`/settings`, `/set`). Persistence lives in `SettingsStore` (added
//! in a later task); this module holds the in-memory model and the pure
//! validation logic so it can be unit-tested without any I/O.

use crate::config::LeverageMap;
use crate::sizing::EntryMode;

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
}
```

- [ ] **Step 2: Register the module**

In `src/main.rs`, add `mod settings;` in the module list (keep alphabetical-ish ordering — after `mod risk;`):

```rust
mod risk;
mod settings;
mod sizing;
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib settings 2>&1 | tail -20`
Expected: PASS (7 tests).

- [ ] **Step 4: Commit**

```bash
git add src/settings.rs src/main.rs
git commit -m "feat(settings): add Settings model and pure apply_setting validator"
```

---

### Task 3: `SettingsStore` (SQLite persistence + seeding)

**Files:**
- Modify: `src/settings.rs`

**Interfaces:**
- Consumes: `Settings`, `EntryMode`, `rusqlite::Connection`.
- Produces: `pub struct SettingsStore` with `pub fn open(path: &str) -> anyhow::Result<Self>`, `pub fn open_in_memory() -> anyhow::Result<Self>`, `pub fn persist(&self, settings: &Settings) -> anyhow::Result<()>`, `pub fn load(&self, seed: Settings) -> anyhow::Result<Settings>`.

- [ ] **Step 1: Add the store at the top of `src/settings.rs`**

Add these imports under the existing ones:

```rust
use rusqlite::Connection;
use std::sync::Mutex;
```

Add the store implementation (place it after `apply_setting`, before the `tests` module):

```rust
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
            if let Ok(value) = raw.parse() { resolved.risk_pct = value; }
        }
        if let Some(raw) = self.get("entry_pct")? {
            if let Ok(value) = raw.parse() { resolved.entry_pct = value; }
        }
        if let Some(raw) = self.get("entry_fixed_usd")? {
            if let Ok(value) = raw.parse() { resolved.entry_fixed_usd = value; }
        }
        if let Some(raw) = self.get("max_daily_risk_pct")? {
            resolved.max_daily_risk_pct = if raw.trim().is_empty() { None } else { raw.parse().ok() };
        }
        if let Some(raw) = self.get("leverage_conservative")? {
            if let Ok(value) = raw.parse() { resolved.leverage.conservative = value; }
        }
        if let Some(raw) = self.get("leverage_moderate")? {
            if let Ok(value) = raw.parse() { resolved.leverage.moderate = value; }
        }
        if let Some(raw) = self.get("leverage_aggressive")? {
            if let Ok(value) = raw.parse() { resolved.leverage.aggressive = value; }
        }
        // Seed any missing keys and normalize storage.
        self.persist(&resolved)?;
        Ok(resolved)
    }
}
```

- [ ] **Step 2: Add persistence tests**

Add to the `tests` module in `src/settings.rs`:

```rust
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
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib settings 2>&1 | tail -20`
Expected: PASS (10 tests total).

- [ ] **Step 4: Commit**

```bash
git add src/settings.rs
git commit -m "feat(settings): add SQLite-backed SettingsStore with seeding"
```

---

### Task 4: `Config` seed fields + `Settings::from_config` + `.env.example`

**Files:**
- Modify: `src/config.rs`
- Modify: `src/settings.rs`
- Modify: `.env.example`

**Interfaces:**
- Consumes: `Config`.
- Produces: `Config` gains `pub entry_mode: EntryMode`, `pub entry_pct: f64`, `pub entry_fixed_usd: f64`. `Settings` gains `pub fn from_config(config: &crate::config::Config) -> Settings`.

- [ ] **Step 1: Add seed fields to `Config`**

In `src/config.rs`, add `use crate::sizing::EntryMode;` near the top (after the module doc comment). Add three fields to the `Config` struct, after `risk_pct`:

```rust
    pub risk_pct: f64,
    /// Default entry sizing mode; seeds the runtime Settings on first boot.
    pub entry_mode: EntryMode,
    /// Percent-of-equity notional for `EntryMode::PercentBalance` (seed).
    pub entry_pct: f64,
    /// Fixed USD notional for `EntryMode::FixedUsd` (seed).
    pub entry_fixed_usd: f64,
```

- [ ] **Step 2: Populate the seed fields in `from_env`**

In `from_env`, after the `risk_pct:` line inside the `Ok(Config { … })` literal, add:

```rust
        risk_pct: parse_env_or("RISK_PCT", 1.0_f64)?,
        entry_mode: match std::env::var("ENTRY_MODE").as_deref() {
            Ok("percent") => EntryMode::PercentBalance,
            Ok("fixed") => EntryMode::FixedUsd,
            _ => EntryMode::RiskBased,
        },
        entry_pct: parse_env_or("ENTRY_PCT", 10.0_f64)?,
        entry_fixed_usd: parse_env_or("ENTRY_FIXED_USD", 50.0_f64)?,
```

(The `risk_pct:` line already exists — keep it; add the three new lines beneath it.)

- [ ] **Step 3: Fix the existing `Config` test literal**

The `#[cfg(test)] mod tests` block in `config.rs` builds a `Config { … }` literal (in `allowlist_membership_check`). Add the three new fields to it so it compiles:

```rust
            risk_pct: 1.0,
            entry_mode: crate::sizing::EntryMode::RiskBased,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
```

(Add immediately after the existing `risk_pct: 1.0,` line in that literal. If `risk_pct` is absent from the literal, add all four lines together in field order.)

- [ ] **Step 4: Add `Settings::from_config`**

In `src/settings.rs`, add to the `impl`-less area an associated constructor. Add an `impl Settings` block after the struct:

```rust
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
```

- [ ] **Step 5: Document the new env vars in `.env.example`**

In `.env.example`, after the `RISK_PCT=1.0` line, add:

```bash
# Default entry sizing mode (seeds the runtime setting; change later via Telegram /set).
#   risk    = size from RISK_PCT and stop distance (default)
#   percent = size notional to ENTRY_PCT % of equity
#   fixed   = size notional to ENTRY_FIXED_USD
ENTRY_MODE=risk
# Notional as % of equity, used when ENTRY_MODE=percent.
ENTRY_PCT=10.0
# Fixed USD notional, used when ENTRY_MODE=fixed (must clear the $10 min order).
ENTRY_FIXED_USD=50.0
```

- [ ] **Step 6: Run tests**

Run: `cargo test --lib config settings 2>&1 | tail -20`
Expected: PASS (config tests + settings tests still green).

- [ ] **Step 7: Commit**

```bash
git add src/config.rs src/settings.rs .env.example
git commit -m "feat(config): seed entry-mode settings from env"
```

---

### Task 5: Wire `Settings` into `BotContext` and the sizing call sites

**Files:**
- Modify: `src/telegram.rs`

**Interfaces:**
- Consumes: `crate::settings::{Settings, SettingsStore}`, `crate::sizing::EntryMode`.
- Produces: `BotContext` gains `pub settings: Arc<std::sync::Mutex<Settings>>` and `pub settings_store: Arc<SettingsStore>`. `recompute_plan` signature changes to `recompute_plan(trade: &PendingTrade, profile: RiskProfile, settings: &Settings) -> Result<ExecutionPlan, SizingError>`.

- [ ] **Step 1: Extend `BotContext` and imports**

In `src/telegram.rs`, add imports near the other `use` lines:

```rust
use crate::settings::{Settings, SettingsStore};
use crate::sizing::EntryMode;
```

Add two fields to `BotContext`:

```rust
pub struct BotContext<E: Exchange + 'static> {
    pub config: crate::config::Config,
    pub exchange: Arc<E>,
    pub store: Arc<PendingStore>,
    pub journal: Arc<Journal>,
    pub settings: Arc<std::sync::Mutex<Settings>>,
    pub settings_store: Arc<SettingsStore>,
    pub http: reqwest::Client,
}
```

- [ ] **Step 2: Build settings in `run`**

In `run`, replace the `let context …` construction:

```rust
    let settings_store = Arc::new(SettingsStore::open("trades.db")?);
    let seeded = settings_store.load(Settings::from_config(&config))?;
    let context: Arc<BotContext<E>> = Arc::new(BotContext {
        config,
        exchange,
        store: Arc::new(PendingStore::new()),
        journal: Arc::new(Journal::open("trades.db")?),
        settings: Arc::new(std::sync::Mutex::new(seeded)),
        settings_store,
        http,
    });
```

- [ ] **Step 3: Update `recompute_plan` to take `&Settings`**

Replace the `recompute_plan` function body:

```rust
pub fn recompute_plan(
    trade: &PendingTrade,
    profile: RiskProfile,
    settings: &Settings,
) -> Result<ExecutionPlan, SizingError> {
    build_plan(&SizingInput {
        setup: &trade.setup,
        equity: trade.equity,
        risk_pct: settings.risk_pct,
        entry_mode: settings.entry_mode,
        entry_pct: settings.entry_pct,
        entry_fixed_usd: settings.entry_fixed_usd,
        profile,
        leverage: &settings.leverage,
        asset_meta: &trade.asset_meta,
    })
}
```

(Remove the now-unused `use crate::config::LeverageMap;` import if the compiler flags it as unused after this change.)

- [ ] **Step 4: Update the profile-switch call site in `on_callback`**

In `on_callback`, the profile branch currently calls `recompute_plan(&trade, profile, context.config.risk_pct, &context.config.leverage)`. Replace with a settings clone:

```rust
    if let Some(profile) = profile_from_callback(&data) {
        if let Some(mut trade) = context.store.get(key) {
            let settings = context.settings.lock().unwrap().clone();
            match recompute_plan(&trade, profile, &settings) {
```

(Leave the rest of that block unchanged.)

- [ ] **Step 5: Update the sizing call site in `process_setups`**

In `process_setups`, replace the block that sets `let profile = RiskProfile::Moderate;` and calls `build_plan(&SizingInput { … })`:

```rust
        let profile = RiskProfile::Moderate;
        let settings = context.settings.lock().unwrap().clone();
        let plan = match build_plan(&SizingInput {
            setup: &setup,
            equity,
            risk_pct: settings.risk_pct,
            entry_mode: settings.entry_mode,
            entry_pct: settings.entry_pct,
            entry_fixed_usd: settings.entry_fixed_usd,
            profile,
            leverage: &settings.leverage,
            asset_meta: &asset_meta,
        }) {
            Ok(plan) => plan,
            Err(error) => {
                bot.send_message(
                    chat_id,
                    format!("{}: cannot size — {error} — skipped.", setup.coin),
                )
                .await?;
                continue;
            }
        };
```

- [ ] **Step 6: Update the multi-signal heads-up message in `process_setups`**

Replace the `if setups.len() > 1 { … }` block (which reads `context.config.risk_pct`) with a mode-aware message:

```rust
    if setups.len() > 1 {
        let settings = context.settings.lock().unwrap().clone();
        let message = match settings.entry_mode {
            EntryMode::RiskBased => format!(
                "Found {} signals. Each sized at {}% risk — confirming all = {:.1}% total risk.",
                setups.len(), settings.risk_pct, settings.risk_pct * setups.len() as f64,
            ),
            EntryMode::PercentBalance => format!(
                "Found {} signals. Each sized at {}% of balance.",
                setups.len(), settings.entry_pct,
            ),
            EntryMode::FixedUsd => format!(
                "Found {} signals. Each sized at ${:.2} notional.",
                setups.len(), settings.entry_fixed_usd,
            ),
        };
        bot.send_message(chat_id, message).await?;
    }
```

- [ ] **Step 7: Update the daily-cap check in `on_callback`**

Replace `if let Some(cap_pct) = context.config.max_daily_risk_pct {` with a read from settings (clone the `Option<f64>` out of the lock before the async body):

```rust
    let cap_pct_opt = context.settings.lock().unwrap().max_daily_risk_pct;
    if let Some(cap_pct) = cap_pct_opt {
```

(Everything inside the block — `now_secs`, `risk_used_since`, `within_daily_cap`, the rejection message — is unchanged.)

- [ ] **Step 8: Build and run the full test suite**

Run: `cargo test 2>&1 | tail -25`
Expected: PASS (all existing tests still green; the crate compiles with the new fields).

- [ ] **Step 9: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): drive sizing and daily cap from runtime Settings"
```

---

### Task 6: `/settings` and `/set` commands + entry-mode buttons

**Files:**
- Modify: `src/telegram.rs`

**Interfaces:**
- Consumes: `crate::settings::apply_setting`, `Settings`, `SettingsStore`, `EntryMode`.
- Produces: `render_settings(settings: &Settings) -> String`; `settings_keyboard(active: EntryMode) -> InlineKeyboardMarkup`; callback constants `CB_MODE_RISK`, `CB_MODE_PERCENT`, `CB_MODE_FIXED`; `entry_mode_from_callback(data: &str) -> Option<EntryMode>`.

- [ ] **Step 1: Add callback constants + render/keyboard/parse helpers**

In `src/telegram.rs`, add near the other `CB_*` constants:

```rust
pub const CB_MODE_RISK: &str = "entry_mode:risk";
pub const CB_MODE_PERCENT: &str = "entry_mode:percent";
pub const CB_MODE_FIXED: &str = "entry_mode:fixed";
```

Add these functions (near `render_summary` / `confirmation_keyboard`):

```rust
/// Plain-text settings card (no MarkdownV2 — sent without parse_mode).
pub fn render_settings(settings: &Settings) -> String {
    let cap = match settings.max_daily_risk_pct {
        Some(value) => format!("{value}%"),
        None => "disabled".to_string(),
    };
    format!(
        "⚙️ Settings\n\n\
         Entry mode: {}\n\
         risk_pct: {}%\n\
         entry_pct: {}%\n\
         entry_fixed_usd: ${}\n\
         max_daily_risk_pct: {}\n\
         leverage_conservative: {}x\n\
         leverage_moderate: {}x\n\
         leverage_aggressive: {}x\n\n\
         Change a number:  /set <key> <value>\n\
         e.g.  /set entry_pct 10\n\
         Switch entry mode with the buttons below.",
        settings.entry_mode.label(),
        settings.risk_pct,
        settings.entry_pct,
        settings.entry_fixed_usd,
        cap,
        settings.leverage.conservative,
        settings.leverage.moderate,
        settings.leverage.aggressive,
    )
}

/// Inline keyboard with the three entry-mode buttons, ✓ on the active one.
pub fn settings_keyboard(active: EntryMode) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(
            label("Risk-based", active == EntryMode::RiskBased), CB_MODE_RISK),
        InlineKeyboardButton::callback(
            label("% Balance", active == EntryMode::PercentBalance), CB_MODE_PERCENT),
        InlineKeyboardButton::callback(
            label("Fixed USD", active == EntryMode::FixedUsd), CB_MODE_FIXED),
    ]])
}

fn entry_mode_from_callback(data: &str) -> Option<EntryMode> {
    match data {
        CB_MODE_RISK => Some(EntryMode::RiskBased),
        CB_MODE_PERCENT => Some(EntryMode::PercentBalance),
        CB_MODE_FIXED => Some(EntryMode::FixedUsd),
        _ => None,
    }
}
```

- [ ] **Step 2: Handle `/settings` and `/set` in `on_message`**

In `on_message`, immediately **before** the `if let Some(reply) = command_response(text) {` block, insert:

```rust
    // /settings — show current values + entry-mode buttons.
    if first_command_word(text) == "/settings" {
        let settings = context.settings.lock().unwrap().clone();
        bot.send_message(message.chat.id, render_settings(&settings))
            .reply_markup(settings_keyboard(settings.entry_mode))
            .await?;
        return Ok(());
    }

    // /set <key> <value> — validate, persist, confirm.
    if first_command_word(text) == "/set" {
        let mut parts = text.trim().split_whitespace();
        parts.next(); // skip "/set"
        let key = parts.next();
        let value = parts.collect::<Vec<_>>().join(" ");
        match key {
            None => {
                bot.send_message(message.chat.id,
                    "Usage: /set <key> <value> — send /settings to see the keys.").await?;
            }
            Some(key) => {
                let current = context.settings.lock().unwrap().clone();
                match crate::settings::apply_setting(&current, key, &value) {
                    Ok(next) => {
                        if let Err(error) = context.settings_store.persist(&next) {
                            bot.send_message(message.chat.id,
                                format!("Could not save setting: {error}")).await?;
                            return Ok(());
                        }
                        *context.settings.lock().unwrap() = next.clone();
                        bot.send_message(message.chat.id, render_settings(&next)).await?;
                    }
                    Err(error_message) => {
                        bot.send_message(message.chat.id, error_message).await?;
                    }
                }
            }
        }
        return Ok(());
    }
```

Add the small helper near `command_response`:

```rust
/// First whitespace-separated token of `text`, lowercased, with any @botname
/// suffix stripped. Returns "" when there is no token.
fn first_command_word(text: &str) -> String {
    text.trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}
```

- [ ] **Step 3: Handle entry-mode buttons in `on_callback`**

In `on_callback`, **before** the `if let Some(profile) = profile_from_callback(&data) {` block, insert:

```rust
    // Entry-mode switch from the /settings keyboard.
    if let Some(mode) = entry_mode_from_callback(&data) {
        let next = {
            let mut guard = context.settings.lock().unwrap();
            guard.entry_mode = mode;
            guard.clone()
        };
        context.settings_store.persist(&next).ok();
        bot.edit_message_text(message.chat.id, message.id, render_settings(&next))
            .reply_markup(settings_keyboard(mode))
            .await?;
        bot.answer_callback_query(&query.id).await?;
        return Ok(());
    }
```

- [ ] **Step 4: Mention the commands in `WELCOME_TEXT`**

In the `WELCOME_TEXT` constant, change the trailing `Commands: /start, /help, /stats` to:

```
Commands: /start, /help, /stats, /settings, /set <key> <value>
```

- [ ] **Step 5: Add tests for the pure helpers**

Add to the `#[cfg(test)] mod tests` block in `src/telegram.rs`:

```rust
    #[test]
    fn settings_card_lists_keys_and_mode() {
        let settings = crate::settings::Settings {
            entry_mode: EntryMode::PercentBalance,
            risk_pct: 1.0,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            max_daily_risk_pct: Some(5.0),
            leverage: crate::config::LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
        };
        let text = super::render_settings(&settings);
        assert!(text.contains("% Balance"));
        assert!(text.contains("entry_pct"));
        assert!(text.contains("max_daily_risk_pct"));
    }

    #[test]
    fn disabled_cap_renders_as_disabled() {
        let settings = crate::settings::Settings {
            entry_mode: EntryMode::RiskBased,
            risk_pct: 1.0, entry_pct: 10.0, entry_fixed_usd: 50.0,
            max_daily_risk_pct: None,
            leverage: crate::config::LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
        };
        assert!(super::render_settings(&settings).contains("disabled"));
    }

    #[test]
    fn settings_keyboard_marks_active_mode() {
        let markup = super::settings_keyboard(EntryMode::FixedUsd);
        let row = &markup.inline_keyboard[0];
        assert!(row[2].text.contains('✓')); // Fixed USD marked
        assert!(!row[0].text.contains('✓'));
    }

    #[test]
    fn entry_mode_callback_parses() {
        assert_eq!(super::entry_mode_from_callback(super::CB_MODE_PERCENT), Some(EntryMode::PercentBalance));
        assert_eq!(super::entry_mode_from_callback("nope"), None);
    }
```

- [ ] **Step 6: Build and run the full test suite**

Run: `cargo test 2>&1 | tail -25`
Expected: PASS (all tests, including the 4 new telegram tests).

- [ ] **Step 7: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): add /settings and /set commands with entry-mode buttons"
```

---

## Self-Review Notes

- **Spec coverage:** Entry modes → Task 1. Daily cap across modes (actual risk) → Task 1 (risk_amount) + Task 5 Step 7. Settings model + validation → Task 2. SQLite persistence + seeding → Task 3. Config seed + `.env.example` → Task 4. Runtime wiring → Task 5. `/settings` + `/set` + buttons → Task 6. Min-notional reject for `$5` → Task 1 Step 5.
- **Manual verification (after Task 6):** `cargo run`, then in Telegram: `/settings` shows the card + buttons; tap `% Balance`; `/set entry_pct 10`; paste a card and confirm the sized notional ≈ 10% equity; `/set entry_fixed_usd 5` then a card → "order notional below $10 minimum".
- The plan touches one module per task; each task ends green and independently reviewable.
