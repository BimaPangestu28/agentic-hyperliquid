# Trigger/Stop Entry Orders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `Confirm Trigger` entry that rests on Hyperliquid and fires (market) when price crosses the entry level, with the SL/TP bracket armed post-fill by a restart-safe background monitor.

**Architecture:** A third confirm button places a non-reduce-only market-on-trigger order and persists a `PendingTrigger` (with full bracket spec) in SQLite. A spawned `run_trigger_monitor` polls the fills feed, matches the entry order's opening fill by order id (correct even when a position already exists on the coin), arms the bracket sized to the actual fill, or cancels the order after a configurable expiry.

**Tech Stack:** Rust, tokio, teloxide, rusqlite, serde_json, async-trait, hyperliquid_rust_sdk 0.6.

## Global Constraints

- Descriptive names; verbs for functions, nouns for values; no abbreviations.
- TDD: behavioral changes preceded by a failing test; pure logic unit-tested.
- Monitors never panic: poll/exchange/DB/send errors logged via `tracing` and swallowed.
- SQLite table creation idempotent (`CREATE TABLE IF NOT EXISTS`); stores use their own connection on the shared `journal_path`, mirroring `SettingsStore`.
- Fill detection matches the specific entry order (opening fill by `oid`), never aggregate `position_size` — must stay correct with a pre-existing position on the same coin.
- Indonesian user-facing copy, matching existing message style.
- Notifications target every `config.allowed_user_ids` as `teloxide::types::ChatId(user_id)`.
- Arming failure must NOT mark a trigger armed — it stays active and retries next poll (position never silently left without a stop).

---

### Task 1: Config + Settings — `trigger_expiry_secs`

**Files:**
- Modify: `src/config.rs` (struct `Config`, `from_env`, test literal), `src/settings.rs` (struct `Settings`, `from_config`, `apply_setting`, `VALID_KEYS`, `persist`, `load`, `sample()`), `src/telegram.rs` (`render_settings` + any `Settings { .. }` test literals)

**Interfaces:**
- Produces: `Config.trigger_expiry_secs: u64` (default `14400`); `Settings.trigger_expiry_secs: u64`; `/set` key `trigger_expiry_secs` (whole number ≥ 1).

- [ ] **Step 1: Write failing settings tests** — add to `src/settings.rs` `mod tests` and update `sample()`:

```rust
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
```

Update `sample()` to add `trigger_expiry_secs: 14400,`.

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib settings:: 2>&1 | tail -20`
Expected: FAIL (field/key/`sample()` mismatch).

- [ ] **Step 3: Implement**

`src/config.rs` — add to `struct Config` (after `monitor_poll_secs`):

```rust
    /// Seconds an unfilled trigger entry rests before auto-cancellation.
    /// Set via `TRIGGER_EXPIRY_SECS` (default 14400 = 4h).
    pub trigger_expiry_secs: u64,
```

In `from_env()` (after `monitor_poll_secs: ...`):

```rust
        trigger_expiry_secs: parse_env_or("TRIGGER_EXPIRY_SECS", 14400_u64)?,
```

In the `allowlist_membership_check` test `Config { .. }` literal add `trigger_expiry_secs: 14400,`.

`src/settings.rs` — add field to `struct Settings` (after `entry_fill_timeout_secs`):

```rust
    /// Seconds an unfilled trigger entry rests before auto-cancellation.
    pub trigger_expiry_secs: u64,
```

In `Settings::from_config` add `trigger_expiry_secs: config.trigger_expiry_secs,`.

Add a parser near `parse_timeout_secs`:

```rust
/// Parses a trigger expiry in seconds; whole number of at least 1.
fn parse_trigger_expiry_secs(value: &str) -> Result<u64, String> {
    let parsed: u64 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
    if parsed < 1 {
        return Err("trigger_expiry_secs must be >= 1".to_string());
    }
    Ok(parsed)
}
```

`apply_setting` arm (before `_ =>`):

```rust
        "trigger_expiry_secs" => next.trigger_expiry_secs = parse_trigger_expiry_secs(value)?,
```

Extend `VALID_KEYS` by appending `, trigger_expiry_secs`.

`persist` (after the `entry_fill_timeout_secs` put):

```rust
        self.put("trigger_expiry_secs", &settings.trigger_expiry_secs.to_string())?;
```

`load` (mirror the `entry_fill_timeout_secs` block):

```rust
        if let Some(raw) = self.get("trigger_expiry_secs")? {
            match raw.parse() {
                Ok(value) => resolved.trigger_expiry_secs = value,
                Err(_) => tracing::warn!(key = "trigger_expiry_secs", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
```

`src/telegram.rs` `render_settings` — add a line after the `entry_fill_timeout_secs` line:

```rust
         entry_fill_timeout_secs: {}s\n\
         trigger_expiry_secs: {}s\n\n\
```

and add `settings.entry_fill_timeout_secs, settings.trigger_expiry_secs,` as the final two args (replacing the single existing `entry_fill_timeout_secs` arg). Also add `trigger_expiry_secs: 14400,` to every `Settings { .. }` literal in `src/telegram.rs` tests (search for `entry_fill_timeout_secs:` to find them).

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: PASS (all).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/settings.rs src/telegram.rs
git commit -m "feat(config): add trigger_expiry_secs setting"
```

---

### Task 2: Exchange — `place_trigger_entry` + `entry_trigger_tpsl`

**Files:**
- Modify: `src/hyperliquid/mod.rs` (Exchange trait, `HyperliquidExchange` impl, `MockExchange` impl, tests)

**Interfaces:**
- Produces:
  - `fn entry_trigger_tpsl() -> &'static str` (pure; returns the Hyperliquid `tpsl` tag for a stop entry)
  - `Exchange::place_trigger_entry(&self, coin: &str, is_buy: bool, size: f64, trigger_price: f64) -> anyhow::Result<OrderResult>`
- Consumes: `OrderResult { order_id: Option<u64>, .. }`, `ClientOrderRequest`, `ClientOrder::Trigger`, `ClientTrigger`, `parse_order_response` (all existing in this file).

**Mapping note (verify before trusting on mainnet):** A reclaim/breakout entry is a STOP
order — it fires when the market trades *through* the trigger. On Hyperliquid the trigger
direction is given by `is_buy` + `tpsl`; for a stop entry the tag is `"sl"` for both long
(`is_buy=true`, fires when price rises to `trigger_px`) and short (`is_buy=false`, fires
when price falls to `trigger_px`). `entry_trigger_tpsl()` returns `"sl"` and is the single
place to change if a testnet probe shows otherwise.

- [ ] **Step 1: Write failing tests** — add to `src/hyperliquid/mod.rs` `mod mock` tests:

```rust
#[test]
fn entry_trigger_tpsl_is_stop() {
    assert_eq!(super::entry_trigger_tpsl(), "sl");
}

#[tokio::test]
async fn mock_records_trigger_entry() {
    let exchange = MockExchange::default();
    let result = exchange.place_trigger_entry("SOL", true, 0.5, 68.53).await.unwrap();
    assert_eq!(result.order_id, Some(7));
    let calls = exchange.trigger_entries.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0], ("SOL".to_string(), true, 0.5, 68.53));
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib hyperliquid:: 2>&1 | tail -20`
Expected: FAIL (method/field/function missing).

- [ ] **Step 3: Implement**

Add the pure helper near `parse_order_response`:

```rust
/// Returns the Hyperliquid `tpsl` tag for a trigger ENTRY order. Entries on a
/// reclaim/breakout are stop orders (fire when price trades through the trigger),
/// so the tag is `"sl"`; the order side (`is_buy`) selects the trigger direction.
fn entry_trigger_tpsl() -> &'static str {
    "sl"
}
```

Add to the `Exchange` trait (after `place_trigger`):

```rust
    /// Places a market-on-trigger ENTRY order (opens a position when price crosses
    /// `trigger_price`). Not reduce-only. Returns the resting order's id in `order_id`.
    async fn place_trigger_entry(&self, coin: &str, is_buy: bool, size: f64, trigger_price: f64) -> anyhow::Result<OrderResult>;
```

`HyperliquidExchange` impl (near `place_trigger`):

```rust
    async fn place_trigger_entry(&self, coin: &str, is_buy: bool, size: f64, trigger_price: f64) -> anyhow::Result<OrderResult> {
        let request = ClientOrderRequest {
            asset: coin.to_string(),
            is_buy,
            reduce_only: false,
            limit_px: trigger_price,
            sz: size,
            cloid: None,
            order_type: ClientOrder::Trigger(ClientTrigger {
                trigger_px: trigger_price,
                is_market: true,
                tpsl: entry_trigger_tpsl().to_string(),
            }),
        };
        let response = self
            .exchange
            .order(request, None)
            .await
            .map_err(|e| anyhow::anyhow!("place_trigger_entry failed: {e}"))?;
        parse_order_response(response)
    }
```

`MockExchange`: add a field `pub trigger_entries: Mutex<Vec<(String, bool, f64, f64)>>,` (the `#[derive(Default)]` covers it) and the impl method:

```rust
        async fn place_trigger_entry(&self, coin: &str, is_buy: bool, size: f64, trigger_price: f64) -> anyhow::Result<OrderResult> {
            self.trigger_entries.lock().unwrap().push((coin.to_string(), is_buy, size, trigger_price));
            Ok(OrderResult { order_id: Some(7), filled: false, avg_price: None })
        }
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test --lib hyperliquid:: 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/hyperliquid/mod.rs
git commit -m "feat(hyperliquid): add place_trigger_entry for stop entries"
```

---

### Task 3: `TriggerStore` + `PendingTrigger` persistence

**Files:**
- Create: `src/trigger_store.rs`
- Modify: `src/main.rs` (add `mod trigger_store;`)

**Interfaces:**
- Produces:
  - `pub struct PendingLeg { pub price: f64, pub alloc_pct: f64 }`
  - `pub struct PendingTrigger { pub id: i64, pub coin: String, pub direction: String, pub size: f64, pub trigger_px: f64, pub leverage: u32, pub stop_loss: f64, pub take_profits: Vec<PendingLeg>, pub entry_oid: Option<u64>, pub chat_id: i64, pub created_at: i64, pub expiry_at: i64, pub status: String }`
  - `TriggerStore::{open, open_in_memory, insert(&PendingTrigger)->i64, list_active()->Vec<PendingTrigger>, mark_armed(i64), mark_expired(i64)}`

- [ ] **Step 1: Write the failing test** — create `src/trigger_store.rs`:

```rust
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
```

Add `mod trigger_store;` to `src/main.rs` (alongside the other `mod` lines).

- [ ] **Step 2: Run — expect FAIL then PASS**

Run: `cargo test --lib trigger_store:: 2>&1 | tail -15`
Expected: the two tests pass once the module compiles (tests + impl ship together in the new file). If the crate fails to compile, fix and re-run.

- [ ] **Step 3: Commit**

```bash
git add src/trigger_store.rs src/main.rs
git commit -m "feat(trigger_store): persist pending trigger entries"
```

---

### Task 4: Extract reusable `arm_bracket`

**Files:**
- Modify: `src/telegram.rs` (extract from `execute_plan`)

**Interfaces:**
- Produces: `pub async fn arm_bracket<E: Exchange>(exchange: &E, coin: &str, direction: Direction, planned_size: f64, effective_size: f64, stop_loss: f64, take_profits: &[BracketLeg]) -> anyhow::Result<()>` — places the reduce-only SL (full `effective_size`) + each TP (scaled by `effective_size/planned_size`), closing side opposite `direction`.
- Consumes: `TriggerOrder`, `Exchange::place_trigger` (existing).

- [ ] **Step 1: Write the failing test** — add to `src/telegram.rs` `mod tests`:

```rust
#[tokio::test]
async fn arm_bracket_places_sl_and_scaled_tps() {
    let exchange = MockExchange { meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }), ..Default::default() };
    let tps = vec![BracketLeg { price: 1.70, size: 60.0 }, BracketLeg { price: 2.00, size: 40.0 }];
    // planned 100, effective 100 → no scaling
    super::arm_bracket(&exchange, "PENDLE", Direction::Long, 100.0, 100.0, 1.25, &tps).await.unwrap();
    let triggers = exchange.triggers.lock().unwrap();
    assert_eq!(triggers.len(), 3);
    assert!(triggers.iter().all(|t| !t.is_buy)); // closing a long => sell
    let sl = triggers.iter().find(|t| !t.is_take_profit).unwrap();
    assert!((sl.size - 100.0).abs() < 1e-6);
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib telegram::tests::arm_bracket_places_sl_and_scaled_tps 2>&1 | tail -15`
Expected: FAIL (`arm_bracket` not found).

- [ ] **Step 3: Implement + refactor `execute_plan`**

Add the function (above `execute_plan`):

```rust
/// Places the reduce-only bracket: SL covering the full held size, and each TP
/// scaled by `effective_size/planned_size` (for partial fills). Closing side is
/// opposite `direction`. Shared by limit/market execution and the trigger monitor.
pub async fn arm_bracket<E: Exchange>(
    exchange: &E,
    coin: &str,
    direction: Direction,
    planned_size: f64,
    effective_size: f64,
    stop_loss: f64,
    take_profits: &[BracketLeg],
) -> anyhow::Result<()> {
    let scale = if planned_size > 0.0 { effective_size / planned_size } else { 0.0 };
    let close_is_buy = !matches!(direction, Direction::Long);
    exchange
        .place_trigger(&TriggerOrder {
            coin: coin.to_string(),
            is_buy: close_is_buy,
            size: effective_size,
            trigger_price: stop_loss,
            is_take_profit: false,
        })
        .await?;
    for take_profit in take_profits {
        exchange
            .place_trigger(&TriggerOrder {
                coin: coin.to_string(),
                is_buy: close_is_buy,
                size: take_profit.size * scale,
                trigger_price: take_profit.price,
                is_take_profit: true,
            })
            .await?;
    }
    Ok(())
}
```

In `execute_plan`, replace the inline SL + TP-loop block (the section that computes
`scale`, places the SL `place_trigger`, and loops `plan.take_profits`) with a single call:

```rust
    arm_bracket(
        exchange,
        &plan.coin,
        plan.direction,
        plan.size,
        effective_size,
        plan.stop_loss.price,
        &plan.take_profits,
    )
    .await?;
```

Keep the `reporter.report(ExecutionEvent::BracketArmed { .. })` call after it, unchanged.

- [ ] **Step 4: Run — expect PASS (incl. existing execute_plan tests)**

Run: `cargo test --lib telegram:: 2>&1 | tail -15`
Expected: PASS — `arm_bracket_*`, `execute_plan_sets_leverage_then_entry_then_brackets`, and `partial_fill_timeout_cancels_remainder_and_brackets_actual_size` all green.

- [ ] **Step 5: Commit**

```bash
git add src/telegram.rs
git commit -m "refactor(telegram): extract reusable arm_bracket from execute_plan"
```

---

### Task 5: Monitor pure helpers — `is_expired`, `matches_entry_fill`

**Files:**
- Modify: `src/monitor.rs` (add helpers + tests), and import `PendingTrigger` from `crate::trigger_store`

**Interfaces:**
- Consumes: `crate::hyperliquid::FillDetail`, `crate::trigger_store::PendingTrigger`.
- Produces:
  - `pub fn is_expired(now_secs: i64, expiry_at: i64) -> bool`
  - `pub fn matches_entry_fill(fill: &FillDetail, pending: &PendingTrigger) -> bool`

- [ ] **Step 1: Write the failing tests** — add to `src/monitor.rs` `mod tests`:

```rust
use crate::trigger_store::PendingTrigger;

fn pending(entry_oid: Option<u64>, created_at: i64) -> PendingTrigger {
    PendingTrigger {
        id: 1, coin: "SOL".into(), direction: "Long".into(), size: 0.5, trigger_px: 68.53,
        leverage: 3, stop_loss: 68.02, take_profits: vec![], entry_oid, chat_id: 1,
        created_at, expiry_at: created_at + 100, status: "active".into(),
    }
}

#[test]
fn is_expired_compares_now_to_expiry() {
    assert!(!is_expired(1099, 1100));
    assert!(is_expired(1101, 1100));
}

#[test]
fn matches_entry_fill_by_oid_and_opening_dir_and_time() {
    let p = pending(Some(7), 1000);
    let opening = detail_full("Open Long", 7, 1500, "SOL");
    assert!(matches_entry_fill(&opening, &p));
    // wrong oid
    assert!(!matches_entry_fill(&detail_full("Open Long", 8, 1500, "SOL"), &p));
    // closing dir
    assert!(!matches_entry_fill(&detail_full("Close Long", 7, 1500, "SOL"), &p));
    // older than created_at
    assert!(!matches_entry_fill(&detail_full("Open Long", 7, 999, "SOL"), &p));
    // pre-existing-position guard: an older opening fill on same coin must not match
    assert!(!matches_entry_fill(&detail_full("Open Long", 7, 500, "SOL"), &p));
}
```

Add a test helper (near the existing `detail` helper) that sets coin/oid/dir/time:

```rust
fn detail_full(dir: &str, oid: u64, time_ms: i64, coin: &str) -> FillDetail {
    FillDetail { coin: coin.into(), oid, dir: dir.into(), px: 68.53, sz: 0.5,
        closed_pnl: 0.0, fee: 0.0, time_ms, start_position: 0.0 }
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib monitor:: 2>&1 | tail -15`
Expected: FAIL (`is_expired`/`matches_entry_fill` not found).

- [ ] **Step 3: Implement** — add to `src/monitor.rs` (after the existing pure helpers):

```rust
use crate::trigger_store::PendingTrigger;

/// True when `now_secs` is past the trigger's expiry timestamp.
pub fn is_expired(now_secs: i64, expiry_at: i64) -> bool {
    now_secs > expiry_at
}

/// True when `fill` is the OPENING fill of `pending`'s entry order: an opening
/// `dir`, matching `entry_oid` (when known), and `time_ms >= created_at`. Matching
/// the specific order id keeps this correct even if a position already existed on
/// the coin (an older or closing fill never matches).
pub fn matches_entry_fill(fill: &FillDetail, pending: &PendingTrigger) -> bool {
    let is_opening = fill.dir.to_ascii_lowercase().contains("open");
    let oid_ok = match pending.entry_oid {
        Some(oid) => fill.oid == oid,
        None => fill.coin.eq_ignore_ascii_case(&pending.coin),
    };
    is_opening && oid_ok && fill.time_ms >= pending.created_at
}
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test --lib monitor:: 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/monitor.rs
git commit -m "feat(monitor): add trigger fill-match and expiry helpers"
```

---

### Task 6: `run_trigger_monitor` loop + spawn

**Files:**
- Modify: `src/monitor.rs` (loop), `src/telegram.rs` (spawn in `run`)

**Interfaces:**
- Consumes: Tasks 2–5; `arm_bracket` (Task 4); `Exchange::{fills_detailed, set_leverage, cancel_order}`; `Settings.trigger_expiry_secs`; `BracketLeg` (from `crate::sizing`); `Direction` (from `crate::parser`).
- Produces: `pub async fn run_trigger_monitor<E: Exchange + 'static>(bot: Bot, exchange: Arc<E>, triggers: Arc<crate::trigger_store::TriggerStore>, allowed_user_ids: Vec<i64>, poll_secs: u64)`

- [ ] **Step 1: Implement the loop** — add to `src/monitor.rs`:

```rust
use crate::trigger_store::{PendingLeg, TriggerStore};
use crate::sizing::BracketLeg;
use crate::parser::Direction;

/// Polls for trigger-entry fills (arming the bracket) and expiries (cancelling the
/// resting order). Never panics: all errors are logged and the loop continues.
pub async fn run_trigger_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    triggers: Arc<TriggerStore>,
    allowed_user_ids: Vec<i64>,
    poll_secs: u64,
) {
    let interval = Duration::from_secs(poll_secs.max(1));
    loop {
        tokio::time::sleep(interval).await;

        let active = match triggers.list_active() {
            Ok(active) => active,
            Err(error) => { tracing::warn!("trigger monitor list_active failed: {error}"); continue; }
        };
        if active.is_empty() { continue; }

        let fills = exchange.fills_detailed().await.unwrap_or_else(|error| {
            tracing::warn!("trigger monitor fills_detailed failed: {error}");
            Vec::new()
        });
        let now_secs = now_unix_secs();

        for pending in active {
            // 1. Filled? Match the entry order's opening fill.
            if let Some(fill) = fills.iter().find(|fill| matches_entry_fill(fill, &pending)) {
                let direction = if pending.direction.eq_ignore_ascii_case("short") {
                    Direction::Short
                } else {
                    Direction::Long
                };
                let take_profits: Vec<BracketLeg> = pending.take_profits.iter()
                    .map(|leg: &PendingLeg| BracketLeg {
                        price: leg.price,
                        size: pending.size * (leg.alloc_pct / 100.0),
                    })
                    .collect();
                match arm_bracket(exchange.as_ref(), &pending.coin, direction, pending.size, fill.sz, pending.stop_loss, &take_profits).await {
                    Ok(()) => {
                        let _ = triggers.mark_armed(pending.id);
                        notify_users(&bot, &allowed_user_ids, &format!("✅ Trigger {} kena — SL/TP terpasang.", pending.coin)).await;
                    }
                    Err(error) => {
                        tracing::warn!("arm_bracket failed for {}: {error}", pending.coin);
                        notify_users(&bot, &allowed_user_ids, &format!("⚠️ Posisi {} TERBUKA tapi GAGAL pasang SL — cek manual SEKARANG!", pending.coin)).await;
                        // leave active → retry next poll
                    }
                }
                continue;
            }
            // 2. Expired? Cancel the resting entry order.
            if is_expired(now_secs, pending.expiry_at) {
                if let Some(oid) = pending.entry_oid {
                    let _ = exchange.cancel_order(&pending.coin, oid).await;
                }
                let _ = triggers.mark_expired(pending.id);
                notify_users(&bot, &allowed_user_ids, &format!("⏱️ Trigger {} kadaluarsa — dibatalkan, tidak ada posisi.", pending.coin)).await;
            }
        }
    }
}

/// Sends `message` to every allowlisted user, logging (not propagating) send errors.
async fn notify_users(bot: &Bot, allowed_user_ids: &[i64], message: &str) {
    for user_id in allowed_user_ids {
        if let Err(error) = bot.send_message(ChatId(*user_id), message).await {
            tracing::warn!("trigger notification failed for {user_id}: {error}");
        }
    }
}

/// Current UNIX time in seconds (0 on clock error).
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

If `run_fill_monitor` already defines a `notify`-style helper or `now_unix_secs`, reuse it
instead of redefining (avoid duplicate-symbol errors); otherwise add as above.

- [ ] **Step 2: Spawn in `telegram::run`** — after the existing fill-monitor spawn block, add:

```rust
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let trigger_store = Arc::new(crate::trigger_store::TriggerStore::open(&journal_path)?);
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        let monitor_poll_secs = context.config.monitor_poll_secs;
        tokio::spawn(async move {
            crate::monitor::run_trigger_monitor(
                monitor_bot, monitor_exchange, trigger_store, monitor_user_ids, monitor_poll_secs,
            ).await;
        });
    }
```

- [ ] **Step 3: Build + full suite**

Run: `cargo build 2>&1 | tail -15` (clean) then `cargo test 2>&1 | tail -10` (green).

- [ ] **Step 4: Commit**

```bash
git add src/monitor.rs src/telegram.rs
git commit -m "feat(monitor): arm/expire trigger entries in background"
```

---

### Task 7: UX — `Confirm Trigger` button + confirm-handler trigger path

**Files:**
- Modify: `src/telegram.rs` (`CB_TRIGGER`, keyboard, `on_callback` trigger branch), uses `TriggerStore`

**Interfaces:**
- Consumes: `place_trigger_entry` (Task 2), `TriggerStore`/`PendingTrigger`/`PendingLeg` (Task 3), `Settings.trigger_expiry_secs` (Task 1). `context` must expose a `TriggerStore` — add `pub triggers: Arc<crate::trigger_store::TriggerStore>` to `BotContext` and build it in `run` (reuse the same `Arc` you spawn the monitor with).

- [ ] **Step 1: Add the constant + button + BotContext field**

Add constant near the others: `pub const CB_TRIGGER: &str = "confirm:trigger";`

In `summary_keyboard`'s confirm row, add a third button:

```rust
            InlineKeyboardButton::callback("✅ Confirm Limit", CB_LIMIT),
            InlineKeyboardButton::callback("⚡ Confirm Market", CB_MARKET),
            InlineKeyboardButton::callback("🎯 Confirm Trigger", CB_TRIGGER),
```

Add `pub triggers: Arc<crate::trigger_store::TriggerStore>,` to `struct BotContext`. In `run`,
create `let trigger_store = Arc::new(crate::trigger_store::TriggerStore::open(&journal_path)?);`
once, put it in `BotContext { .. triggers: trigger_store.clone(), .. }`, and pass
`trigger_store` to the `run_trigger_monitor` spawn (replacing the inline `open` from Task 6
Step 2 so there is a single shared store).

- [ ] **Step 2: Handle `CB_TRIGGER` in `on_callback`**

The existing dispatch is `let use_limit = match data.as_str() { CB_LIMIT => true, CB_MARKET => false, _ => return Ok(()) };`. Replace it with an enum so the trigger path is explicit:

```rust
    enum Confirm { Limit, Market, Trigger }
    let confirm = match data.as_str() {
        CB_LIMIT => Confirm::Limit,
        CB_MARKET => Confirm::Market,
        CB_TRIGGER => Confirm::Trigger,
        _ => return Ok(()),
    };
```

Keep the existing trade-take, daily-risk-cap check, and synchronous journal reservation
unchanged. For `Confirm::Limit | Confirm::Market`, keep the current spawn that calls
`execute_plan` (map to `use_limit = matches!(confirm, Confirm::Limit)`). For
`Confirm::Trigger`, instead of `execute_plan`, do:

```rust
        Confirm::Trigger => {
            let is_buy = matches!(trade.plan.direction, Direction::Long);
            let expiry_secs = context.settings.lock().unwrap().trigger_expiry_secs;
            // Set leverage now (account/coin level), then place the resting trigger entry.
            if let Err(error) = context.exchange.set_leverage(&trade.plan.coin, trade.plan.leverage).await {
                bot.send_message(chat_id, format!("❌ Gagal set leverage: {error}")).await.ok();
                return Ok(());
            }
            let placed = context.exchange.place_trigger_entry(&trade.plan.coin, is_buy, trade.plan.size, trade.plan.entry).await;
            match placed {
                Ok(result) => {
                    let take_profits: Vec<crate::trigger_store::PendingLeg> = trade.setup.take_profits.iter()
                        .map(|tp| crate::trigger_store::PendingLeg { price: tp.price, alloc_pct: tp.allocation_pct })
                        .collect();
                    let pending = crate::trigger_store::PendingTrigger {
                        id: 0,
                        coin: trade.plan.coin.clone(),
                        direction: format!("{:?}", trade.plan.direction),
                        size: trade.plan.size,
                        trigger_px: trade.plan.entry,
                        leverage: trade.plan.leverage,
                        stop_loss: trade.plan.stop_loss.price,
                        take_profits,
                        entry_oid: result.order_id,
                        chat_id: chat_id.0,
                        created_at: opened_at,
                        expiry_at: opened_at + expiry_secs as i64,
                        status: "active".into(),
                    };
                    let _ = context.triggers.insert(&pending);
                    bot.send_message(chat_id, format!("🎯 Trigger {} @ ${:.4} dipasang — nunggu harga menembus.", trade.plan.coin, trade.plan.entry)).await.ok();
                }
                Err(error) => { bot.send_message(chat_id, format!("❌ Gagal pasang trigger: {error}")).await.ok(); }
            }
            return Ok(());
        }
```

Place this branch BEFORE the `tokio::spawn` used by Limit/Market (the trigger path does not
spawn execution — the monitor handles arming). Keep the existing "Executing…" edit only for
the Limit/Market path; for the trigger path edit the message to `format!("Memasang trigger
{}…", trade.plan.coin)` instead (adjust the prelude so the edit text matches the path).

- [ ] **Step 3: Build + full suite**

Run: `cargo build 2>&1 | tail -15` (clean — watch for unused/borrow errors around the moved `trade`) then `cargo test 2>&1 | tail -10` (green).

- [ ] **Step 4: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): add Confirm Trigger entry path"
```

---

### Task 8: Trigger-side warning + docs

**Files:**
- Modify: `src/telegram.rs` (warning), `.env.example`, `README.md`

**Interfaces:**
- Produces: `pub fn trigger_fires_immediately(direction: Direction, trigger_px: f64, mark_px: f64) -> bool`

- [ ] **Step 1: Write the failing test** — add to `src/telegram.rs` `mod tests`:

```rust
#[test]
fn trigger_fires_immediately_detects_wrong_side() {
    use crate::parser::Direction;
    // Long: fires immediately if trigger <= mark (price already at/above trigger).
    assert!(super::trigger_fires_immediately(Direction::Long, 68.53, 69.00));
    assert!(!super::trigger_fires_immediately(Direction::Long, 68.53, 68.00));
    // Short: fires immediately if trigger >= mark.
    assert!(super::trigger_fires_immediately(Direction::Short, 68.53, 68.00));
    assert!(!super::trigger_fires_immediately(Direction::Short, 68.53, 69.00));
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib telegram::tests::trigger_fires_immediately_detects_wrong_side 2>&1 | tail -10`
Expected: FAIL.

- [ ] **Step 3: Implement + wire the warning**

Add the predicate:

```rust
/// True when a trigger entry would fire immediately because price is already past
/// the trigger (long with trigger ≤ mark, or short with trigger ≥ mark) — i.e. it
/// would behave like a market order rather than waiting for a breakout.
pub fn trigger_fires_immediately(direction: Direction, trigger_px: f64, mark_px: f64) -> bool {
    match direction {
        Direction::Long => trigger_px <= mark_px,
        Direction::Short => trigger_px >= mark_px,
    }
}
```

In the `Confirm::Trigger` branch, after a successful `place_trigger_entry`, fetch the mark
and append a warning when applicable. Reuse the SDK mark via the cheapest available path —
`context.exchange.positions()` does not help when flat, so use the asset mid from a light
info call already available in the codebase; if none is convenient, derive mark from
`trade.plan.entry` is NOT valid (that is the trigger itself). Use
`context.exchange` to read the current mark for the coin if such a method exists; otherwise
SKIP the live-mark check and instead emit the warning opportunistically only when the trade
plan already carries a mark. Concretely: if a mark price is available as `mark`, do:

```rust
                    if trigger_fires_immediately(trade.plan.direction, trade.plan.entry, mark) {
                        bot.send_message(chat_id, format!("⚠️ Harga sudah melewati trigger {} — order ini akan tereksekusi ~segera (seperti market).", trade.plan.coin)).await.ok();
                    }
```

If no mark-price accessor exists on `Exchange`, this step's wiring is deferred to a one-line
follow-up and the test for the pure predicate still ships. (The predicate is the verifiable
deliverable; the live-mark wiring depends on an accessor that may need adding — note it in
the task report rather than inventing an API.)

- [ ] **Step 4: Docs**

`.env.example` — after the `MONITOR_POLL_SECS=30` line:

```
# Seconds an unfilled trigger entry rests before auto-cancellation (default 14400 = 4h).
TRIGGER_EXPIRY_SECS=14400
```

`README.md` `## Usage` — append:

```markdown
**Confirm Trigger** places a stop entry that rests until price crosses the entry level
(buy-stop for longs, sell-stop for shorts), then market-fills and the bot arms the SL/TP
bracket. An unfilled trigger is cancelled after `TRIGGER_EXPIRY_SECS` (default 4h, editable
via `/set trigger_expiry_secs`).
```

- [ ] **Step 5: Run + commit**

Run: `cargo test 2>&1 | tail -10` (green).

```bash
git add src/telegram.rs .env.example README.md
git commit -m "feat(telegram): warn on immediate-fire trigger; document trigger entry"
```

---

## Self-Review Notes

- **Spec coverage:** UX button → Task 7. `place_trigger_entry` + tpsl → Task 2. Persistence → Task 3. `arm_bracket` refactor → Task 4. Fill-match/expiry helpers → Task 5. Monitor loop + spawn → Task 6. Settings `trigger_expiry_secs` → Task 1. Warning → Task 8. Docs → Task 8.
- **Pre-existing-position correctness:** Task 5 `matches_entry_fill` matches by `entry_oid` + opening dir + `time_ms >= created_at`, with an explicit test that an older opening fill on the same coin does NOT match — this is the spec's key requirement.
- **Open risk flagged for implementer:** Task 2 `entry_trigger_tpsl()` mapping (`"sl"`) should be confirmed on testnet before mainnet use; it is isolated to one function. Task 8 live-mark warning depends on a mark-price accessor — if absent, ship the pure predicate and note the wiring gap rather than inventing an API.
- **Type consistency:** `PendingTrigger`/`PendingLeg` fields, `arm_bracket` signature, `place_trigger_entry` signature, and `run_trigger_monitor` signature are used identically across Tasks 2–7.
- **DB safety:** `TriggerStore` uses its own connection on `journal_path` (like `SettingsStore`); the confirm handler and the monitor share one `Arc<TriggerStore>` (Task 7 Step 1) so they use the same connection rather than racing two.
