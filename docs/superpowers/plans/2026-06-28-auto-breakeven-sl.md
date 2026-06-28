# Auto-Breakeven Stop-Loss Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When an open position's profit reaches a configurable R-multiple of its initial risk, automatically move its stop-loss to breakeven (entry + a small fee buffer), reading the live stop from the exchange.

**Architecture:** A new background loop `run_breakeven_monitor` polls `positions()` and, per position, reads the resting stop-loss via a new `open_orders(coin)` exchange query. A pure decision function (`decide_breakeven`) decides whether to move; the loop places the new stop, cancels the old one, and notifies. No journal/schema changes; "already moved" is detected from the stop already sitting on the profitable side of entry.

**Tech Stack:** Rust, tokio, rusqlite (settings only), teloxide (Bot), hyperliquid SDK, async-trait.

## Global Constraints

- Feature is **disabled by default** (`breakeven_enabled = false`) — behaviour unchanged until opted in.
- Settings defaults: `breakeven_trigger_r = 1.0`, `breakeven_buffer_pct = 0.1`.
- `Direction` is the existing `crate::parser::Direction` enum (`Long` / `Short`).
- Monitors never panic: poll/place/cancel/send errors are logged with `tracing` and the loop continues.
- Place the new stop **before** cancelling the old one (position never unprotected).
- Follow existing naming: descriptive names, verbs for functions, `cargo test` is the test runner.

---

### Task 1: Pure breakeven core

**Files:**
- Create: `src/breakeven.rs`
- Modify: `src/main.rs` (add `mod breakeven;` alongside the other `mod` declarations)
- Test: inline `#[cfg(test)]` module in `src/breakeven.rs`

**Interfaces:**
- Consumes: `crate::parser::Direction`, `crate::hyperliquid::{OpenPosition, OpenOrder}` (the `OpenOrder` type is created in Task 3; Task 1 only needs `OpenPosition` and the three scalar helpers — `decide_breakeven` is added in Task 4 once `OpenOrder` exists).
- Produces:
  - `reached_breakeven_threshold(direction: Direction, entry: f64, stop_loss: f64, mark: f64, trigger_r: f64) -> bool`
  - `breakeven_price(direction: Direction, entry: f64, buffer_pct: f64) -> f64`
  - `already_at_breakeven(direction: Direction, entry: f64, stop: f64) -> bool`

- [ ] **Step 1: Add the module declaration**

In `src/main.rs`, find the block of `mod ...;` declarations and add (keeping alphabetical order if the file uses it):

```rust
mod breakeven;
```

- [ ] **Step 2: Write the failing tests**

Create `src/breakeven.rs` with:

```rust
//! Pure helpers for the auto-breakeven stop-loss feature. No I/O — every
//! function is a deterministic computation over prices, so the decision logic
//! is unit-tested without an exchange or a clock.

use crate::parser::Direction;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_met_for_long_at_and_above_one_r() {
        // entry 100, stop 95 -> risk 5. 1R target = mark 105.
        assert!(!reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 104.9, 1.0));
        assert!(reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 105.0, 1.0));
        assert!(reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 110.0, 1.0));
    }

    #[test]
    fn threshold_met_for_short_at_and_above_one_r() {
        // entry 100, stop 105 -> risk 5. 1R target = mark 95.
        assert!(!reached_breakeven_threshold(Direction::Short, 100.0, 105.0, 95.1, 1.0));
        assert!(reached_breakeven_threshold(Direction::Short, 100.0, 105.0, 95.0, 1.0));
        assert!(reached_breakeven_threshold(Direction::Short, 100.0, 105.0, 90.0, 1.0));
    }

    #[test]
    fn threshold_respects_trigger_r_other_than_one() {
        // entry 100, stop 95 -> risk 5. 2R target = mark 110.
        assert!(!reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 109.0, 2.0));
        assert!(reached_breakeven_threshold(Direction::Long, 100.0, 95.0, 110.0, 2.0));
    }

    #[test]
    fn threshold_false_when_risk_distance_zero() {
        // Degenerate setup: entry == stop. Never fires (guards divide-by-zero).
        assert!(!reached_breakeven_threshold(Direction::Long, 100.0, 100.0, 200.0, 1.0));
    }

    #[test]
    fn breakeven_price_nudges_past_entry() {
        // Long: above entry by buffer_pct%.
        assert!((breakeven_price(Direction::Long, 100.0, 0.1) - 100.1).abs() < 1e-9);
        // Short: below entry by buffer_pct%.
        assert!((breakeven_price(Direction::Short, 100.0, 0.1) - 99.9).abs() < 1e-9);
    }

    #[test]
    fn already_moved_when_stop_on_profitable_side_of_entry() {
        // Long: stop at/above entry means already moved.
        assert!(!already_at_breakeven(Direction::Long, 100.0, 95.0));
        assert!(already_at_breakeven(Direction::Long, 100.0, 100.0));
        assert!(already_at_breakeven(Direction::Long, 100.0, 100.1));
        // Short: stop at/below entry means already moved.
        assert!(!already_at_breakeven(Direction::Short, 100.0, 105.0));
        assert!(already_at_breakeven(Direction::Short, 100.0, 100.0));
        assert!(already_at_breakeven(Direction::Short, 100.0, 99.9));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test breakeven::`
Expected: FAIL — `cannot find function reached_breakeven_threshold` (etc.).

- [ ] **Step 4: Write the minimal implementation**

Insert above the `#[cfg(test)]` module in `src/breakeven.rs`:

```rust
/// True when profit has reached `trigger_r` times the initial risk distance.
/// `risk = |entry - stop_loss|`; profit is `mark - entry` for a long and
/// `entry - mark` for a short. Returns false when risk is ~zero (degenerate setup).
pub fn reached_breakeven_threshold(
    direction: Direction,
    entry: f64,
    stop_loss: f64,
    mark: f64,
    trigger_r: f64,
) -> bool {
    let risk_distance = (entry - stop_loss).abs();
    if risk_distance < f64::EPSILON {
        return false;
    }
    let profit_distance = match direction {
        Direction::Long => mark - entry,
        Direction::Short => entry - mark,
    };
    profit_distance >= trigger_r * risk_distance
}

/// Breakeven stop price: `entry` nudged past itself by `buffer_pct`% to cover fees,
/// so a stop-out closes ~flat after round-trip fees. Long → above entry; short → below.
pub fn breakeven_price(direction: Direction, entry: f64, buffer_pct: f64) -> f64 {
    let buffer = entry * (buffer_pct / 100.0);
    match direction {
        Direction::Long => entry + buffer,
        Direction::Short => entry - buffer,
    }
}

/// True when the resting `stop` is already on the profitable side of `entry`
/// (long: `stop >= entry`; short: `stop <= entry`) — i.e. it has already been
/// moved to breakeven and must not be moved again.
pub fn already_at_breakeven(direction: Direction, entry: f64, stop: f64) -> bool {
    match direction {
        Direction::Long => stop >= entry,
        Direction::Short => stop <= entry,
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test breakeven::`
Expected: PASS (6 tests).

- [ ] **Step 6: Commit**

```bash
git add src/breakeven.rs src/main.rs
git commit -m "feat(breakeven): pure R-multiple threshold + breakeven-price helpers"
```

---

### Task 2: Settings — three breakeven keys

**Files:**
- Modify: `src/settings.rs` (struct, `from_config`, `sample`, parse helpers, `apply_setting`, `persist`, `load`, `VALID_KEYS`)
- Modify: `src/telegram.rs` (the two `Settings { .. }` test literals near lines 1789 and 1811, and `render_settings`)
- Test: inline `#[cfg(test)]` module in `src/settings.rs`

**Interfaces:**
- Produces on `Settings`: `breakeven_enabled: bool`, `breakeven_trigger_r: f64`, `breakeven_buffer_pct: f64`.

- [ ] **Step 1: Write the failing tests**

In `src/settings.rs` `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn sets_breakeven_trigger_r_and_rejects_non_positive() {
        assert_eq!(apply_setting(&sample(), "breakeven_trigger_r", "1.5").unwrap().breakeven_trigger_r, 1.5);
        assert!(apply_setting(&sample(), "breakeven_trigger_r", "0").is_err());
        assert!(apply_setting(&sample(), "breakeven_trigger_r", "-1").is_err());
        assert!(apply_setting(&sample(), "breakeven_trigger_r", "abc").is_err());
    }

    #[test]
    fn sets_breakeven_buffer_pct_and_rejects_negative() {
        assert_eq!(apply_setting(&sample(), "breakeven_buffer_pct", "0.2").unwrap().breakeven_buffer_pct, 0.2);
        assert_eq!(apply_setting(&sample(), "breakeven_buffer_pct", "0").unwrap().breakeven_buffer_pct, 0.0);
        assert!(apply_setting(&sample(), "breakeven_buffer_pct", "-0.1").is_err());
    }

    #[test]
    fn toggles_breakeven_enabled() {
        let on = apply_setting(&sample(), "breakeven_enabled", "on").unwrap();
        assert!(on.breakeven_enabled);
        let off = apply_setting(&on, "breakeven_enabled", "off").unwrap();
        assert!(!off.breakeven_enabled);
    }

    #[test]
    fn persist_load_roundtrips_breakeven_settings() {
        let store = SettingsStore::open_in_memory().unwrap();
        let mut settings = sample();
        settings.breakeven_enabled = true;
        settings.breakeven_trigger_r = 1.5;
        settings.breakeven_buffer_pct = 0.2;
        store.persist(&settings).unwrap();
        let loaded = store.load(sample()).unwrap();
        assert!(loaded.breakeven_enabled);
        assert_eq!(loaded.breakeven_trigger_r, 1.5);
        assert_eq!(loaded.breakeven_buffer_pct, 0.2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test settings::`
Expected: FAIL — `no field breakeven_trigger_r on type Settings`.

- [ ] **Step 3: Add the struct fields**

In `src/settings.rs`, in `pub struct Settings`, after `pub max_open_positions: u32,` add:

```rust
    /// Master toggle for auto-breakeven stop-loss (default false).
    pub breakeven_enabled: bool,
    /// Profit (in R-multiples of initial risk) at which the stop is moved to
    /// breakeven. Must be > 0.
    pub breakeven_trigger_r: f64,
    /// Fee buffer past entry (percent) for the breakeven stop. Must be >= 0.
    pub breakeven_buffer_pct: f64,
```

In `from_config`, after `max_open_positions: 5,` add:

```rust
            breakeven_enabled: false,
            breakeven_trigger_r: 1.0,
            breakeven_buffer_pct: 0.1,
```

In `sample()` (test helper), after `max_open_positions: 5,` add the same three lines:

```rust
        breakeven_enabled: false,
        breakeven_trigger_r: 1.0,
        breakeven_buffer_pct: 0.1,
```

- [ ] **Step 4: Add parse helpers**

In `src/settings.rs`, after `fn parse_min_rr`, add:

```rust
/// Parses the breakeven R-multiple trigger; must be a number strictly > 0.
fn parse_breakeven_trigger_r(value: &str) -> Result<f64, String> {
    let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
    if parsed <= 0.0 {
        return Err(format!("breakeven_trigger_r must be > 0 (got {parsed})"));
    }
    Ok(parsed)
}

/// Parses the breakeven fee buffer percent; must be a number of at least 0.
fn parse_breakeven_buffer_pct(value: &str) -> Result<f64, String> {
    let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
    if parsed < 0.0 {
        return Err(format!("breakeven_buffer_pct must be >= 0 (got {parsed})"));
    }
    Ok(parsed)
}
```

- [ ] **Step 5: Wire `apply_setting`, `VALID_KEYS`, `persist`, `load`**

In `apply_setting`, before the final `_ =>` arm, add:

```rust
        "breakeven_enabled" => {
            next.breakeven_enabled = match value {
                "true" | "on" | "1" => true,
                "false" | "off" | "0" => false,
                _ => return Err("breakeven_enabled must be true or false".to_string()),
            };
        }
        "breakeven_trigger_r" => next.breakeven_trigger_r = parse_breakeven_trigger_r(value)?,
        "breakeven_buffer_pct" => next.breakeven_buffer_pct = parse_breakeven_buffer_pct(value)?,
```

Append to the `VALID_KEYS` string (inside the existing literal, after `coin_blacklist`):

```
, breakeven_enabled, breakeven_trigger_r, breakeven_buffer_pct
```

In `persist`, after the `coin_blacklist` put, add:

```rust
        self.put("breakeven_enabled", &settings.breakeven_enabled.to_string())?;
        self.put("breakeven_trigger_r", &settings.breakeven_trigger_r.to_string())?;
        self.put("breakeven_buffer_pct", &settings.breakeven_buffer_pct.to_string())?;
```

In `load`, after the `coin_blacklist` block, add:

```rust
        if let Some(raw) = self.get("breakeven_enabled")? {
            resolved.breakeven_enabled = matches!(raw.as_str(), "true" | "on" | "1");
        }
        if let Some(raw) = self.get("breakeven_trigger_r")? {
            match raw.parse() {
                Ok(value) => resolved.breakeven_trigger_r = value,
                Err(_) => tracing::warn!(key = "breakeven_trigger_r", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
        if let Some(raw) = self.get("breakeven_buffer_pct")? {
            match raw.parse() {
                Ok(value) => resolved.breakeven_buffer_pct = value,
                Err(_) => tracing::warn!(key = "breakeven_buffer_pct", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
```

- [ ] **Step 6: Update the two `Settings { .. }` literals in telegram.rs**

In `src/telegram.rs`, both test literals (near lines 1789 and 1811) end with `min_rr: 0.0,` and `coin_blacklist: Vec::new(),`. After those, in each, add:

```rust
            breakeven_enabled: false,
            breakeven_trigger_r: 1.0,
            breakeven_buffer_pct: 0.1,
```

- [ ] **Step 7: Surface in `render_settings`**

In `src/telegram.rs` `render_settings`, after the `coin_blacklist: {}\n` line in the format string and its `blacklist` argument, add a new line `breakeven: {}\n` and build the value before the `format!`:

```rust
    let breakeven = if settings.breakeven_enabled {
        format!("ON · {:.2}R · buf {:.2}%", settings.breakeven_trigger_r, settings.breakeven_buffer_pct)
    } else {
        "OFF".to_string()
    };
```

Add `breakeven: {}\n` to the format string (e.g. after the `coin_blacklist` line) and pass `breakeven,` as the matching argument. Also extend the `/set` example line to include:

```
 ·  /set breakeven_enabled on  ·  /set breakeven_trigger_r 1.0
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test settings:: && cargo test telegram::tests::summary && cargo test telegram::tests::disabled_cap_renders`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add src/settings.rs src/telegram.rs
git commit -m "feat(settings): breakeven_enabled / trigger_r / buffer_pct keys"
```

---

### Task 3: `OpenOrder` type + `open_orders` exchange query

**Files:**
- Modify: `src/hyperliquid/mod.rs` (add `OpenOrder` struct, trait method, mock impl + seed setter, real impl)
- Test: inline mock test in `src/hyperliquid/mod.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct OpenOrder { coin: String, oid: u64, trigger_price: f64, is_trigger: bool, reduce_only: bool, is_take_profit: bool }`
  - Trait method `async fn open_orders(&self, coin: &str) -> anyhow::Result<Vec<OpenOrder>>`
  - Mock seed: `MockExchange::set_open_orders_detail(Vec<OpenOrder>)`

- [ ] **Step 1: Add the `OpenOrder` struct**

In `src/hyperliquid/mod.rs`, after the `OpenPosition` struct, add:

```rust
// ── OpenOrder (resting order snapshot, for stop/TP inspection) ─────────────────

/// A resting order on the book, with enough detail to identify a reduce-only
/// stop-loss trigger and read/cancel it. Derived from `frontendOpenOrders`.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenOrder {
    pub coin: String,
    pub oid: u64,
    /// Trigger price for trigger orders; `0.0` for plain limit orders.
    pub trigger_price: f64,
    pub is_trigger: bool,
    pub reduce_only: bool,
    /// True for take-profit triggers, false for stop-loss triggers
    /// (from Hyperliquid `orderType`: "Take Profit *" → true, "Stop *" → false).
    pub is_take_profit: bool,
}
```

- [ ] **Step 2: Add the trait method**

In `pub trait Exchange`, after the `positions` method, add:

```rust
    /// Resting orders for `coin`, including trigger price and reduce-only/TP flags,
    /// so callers can locate the live stop-loss. Read-only.
    async fn open_orders(&self, coin: &str) -> anyhow::Result<Vec<OpenOrder>>;
```

- [ ] **Step 3: Write the failing mock test**

In `src/hyperliquid/mod.rs` `#[cfg(test)] mod tests`, add:

```rust
    #[tokio::test]
    async fn mock_open_orders_filters_by_coin() {
        let mock = MockExchange::new_for_test();
        mock.set_open_orders_detail(vec![
            OpenOrder { coin: "BTC".into(), oid: 11, trigger_price: 95.0, is_trigger: true, reduce_only: true, is_take_profit: false },
            OpenOrder { coin: "ETH".into(), oid: 22, trigger_price: 0.0, is_trigger: false, reduce_only: false, is_take_profit: false },
        ]);
        let btc = mock.open_orders("btc").await.unwrap();
        assert_eq!(btc.len(), 1);
        assert_eq!(btc[0].oid, 11);
    }
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test mock_open_orders_filters_by_coin`
Expected: FAIL — `no method set_open_orders_detail` / trait not implemented.

- [ ] **Step 5: Add the mock field, setter, and impl**

In `MockExchange` struct, add a field:

```rust
        /// Pre-loaded resting orders returned by `open_orders()`.
        pub open_orders_detail: Mutex<Vec<super::OpenOrder>>,
```

In `impl MockExchange`, add a setter:

```rust
        /// Seeds the resting orders returned by `open_orders()`.
        pub fn set_open_orders_detail(&self, orders: Vec<super::OpenOrder>) {
            *self.open_orders_detail.lock().unwrap() = orders;
        }
```

In `impl Exchange for MockExchange`, add the method:

```rust
        async fn open_orders(&self, coin: &str) -> anyhow::Result<Vec<super::OpenOrder>> {
            Ok(self
                .open_orders_detail
                .lock()
                .unwrap()
                .iter()
                .filter(|order| order.coin.eq_ignore_ascii_case(coin))
                .cloned()
                .collect())
        }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test mock_open_orders_filters_by_coin`
Expected: PASS.

- [ ] **Step 7: Add the real implementation**

In `src/hyperliquid/mod.rs`, in `impl Exchange for` the real client (the same block as `usdc_flows`), add an `open_orders` method that POSTs `frontendOpenOrders` (mirror the `usdc_flows` raw-POST pattern):

```rust
    /// Resting orders for `coin` via the `frontendOpenOrders` info endpoint, which
    /// (unlike `openOrders`) includes `triggerPx`, `isTrigger`, `reduceOnly`, and
    /// `orderType` — enough to identify the reduce-only stop-loss trigger.
    async fn open_orders(&self, coin: &str) -> anyhow::Result<Vec<OpenOrder>> {
        let address_hex = format!("{:#x}", self.address);
        let body = serde_json::json!({
            "type": "frontendOpenOrders",
            "user": address_hex,
        })
        .to_string();

        let base_url = &self.info.http_client.base_url;
        let response_text = self
            .info
            .http_client
            .client
            .post(format!("{base_url}/info"))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("open_orders request failed: {e}"))?
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("open_orders response read failed: {e}"))?;

        let raw_rows: Vec<serde_json::Value> = serde_json::from_str(&response_text)
            .map_err(|e| anyhow::anyhow!("open_orders JSON parse failed: {e}"))?;

        let orders = raw_rows
            .into_iter()
            .filter_map(|row| {
                let row_coin = row.get("coin")?.as_str()?.to_string();
                if !row_coin.eq_ignore_ascii_case(coin) {
                    return None;
                }
                let oid = row.get("oid")?.as_u64()?;
                let is_trigger = row.get("isTrigger").and_then(|v| v.as_bool()).unwrap_or(false);
                let reduce_only = row.get("reduceOnly").and_then(|v| v.as_bool()).unwrap_or(false);
                let trigger_price = row
                    .get("triggerPx")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let order_type = row.get("orderType").and_then(|v| v.as_str()).unwrap_or("");
                let is_take_profit = order_type.to_ascii_lowercase().contains("take profit");
                Some(OpenOrder { coin: row_coin, oid, trigger_price, is_trigger, reduce_only, is_take_profit })
            })
            .collect();
        Ok(orders)
    }
```

- [ ] **Step 8: Build to verify it compiles**

Run: `cargo build`
Expected: compiles clean (no behaviour change — new method is unused so far).

- [ ] **Step 9: Commit**

```bash
git add src/hyperliquid/mod.rs
git commit -m "feat(exchange): open_orders(coin) query with trigger/reduce-only detail"
```

---

### Task 4: Breakeven decision + apply, wired into a monitor loop

**Files:**
- Modify: `src/breakeven.rs` (add `BreakevenAction` + `decide_breakeven`)
- Modify: `src/monitor.rs` (add `apply_breakeven` + `run_breakeven_monitor`)
- Test: inline tests in both files

**Interfaces:**
- Consumes: `reached_breakeven_threshold`, `breakeven_price`, `already_at_breakeven` (Task 1); `OpenOrder`, `OpenPosition`, `TriggerOrder`, `Exchange` (Task 3 / existing).
- Produces:
  - `pub struct BreakevenAction { pub be_price: f64, pub size: f64, pub close_is_buy: bool, pub old_oid: u64 }`
  - `pub fn decide_breakeven(position: &OpenPosition, stop: Option<&OpenOrder>, trigger_r: f64, buffer_pct: f64) -> Option<BreakevenAction>`
  - `pub async fn apply_breakeven<E: Exchange + ?Sized>(exchange: &E, coin: &str, action: &BreakevenAction) -> anyhow::Result<()>`
  - `pub async fn run_breakeven_monitor<E: Exchange + 'static>(bot: Bot, exchange: Arc<E>, settings: Arc<Mutex<Settings>>, allowed_user_ids: Vec<i64>, poll_secs: u64)`

- [ ] **Step 1: Write the failing tests for `decide_breakeven`**

In `src/breakeven.rs` `#[cfg(test)] mod tests`, add a helper and tests:

```rust
    use crate::hyperliquid::{OpenOrder, OpenPosition};

    fn long_position(entry: f64, mark: f64) -> OpenPosition {
        OpenPosition {
            coin: "BTC".into(), direction: "long".into(), size: 0.5,
            entry_px: entry, mark_px: mark, unrealized_pnl: 0.0, leverage: 10.0, notional: 0.0,
        }
    }

    fn stop_order(oid: u64, trigger: f64) -> OpenOrder {
        OpenOrder { coin: "BTC".into(), oid, trigger_price: trigger, is_trigger: true, reduce_only: true, is_take_profit: false }
    }

    #[test]
    fn decide_moves_long_past_threshold() {
        // entry 100 stop 95 risk 5; mark 105 = 1R -> move. BE = 100.1, close side = sell (false).
        let action = decide_breakeven(&long_position(100.0, 105.0), Some(&stop_order(7, 95.0)), 1.0, 0.1).unwrap();
        assert!((action.be_price - 100.1).abs() < 1e-9);
        assert_eq!(action.old_oid, 7);
        assert!(!action.close_is_buy); // long closes with a sell
        assert!((action.size - 0.5).abs() < 1e-9);
    }

    #[test]
    fn decide_skips_below_threshold() {
        assert!(decide_breakeven(&long_position(100.0, 104.0), Some(&stop_order(7, 95.0)), 1.0, 0.1).is_none());
    }

    #[test]
    fn decide_skips_when_already_at_breakeven() {
        // stop already at/above entry -> already moved.
        assert!(decide_breakeven(&long_position(100.0, 110.0), Some(&stop_order(7, 100.0)), 1.0, 0.1).is_none());
    }

    #[test]
    fn decide_skips_when_no_stop_order() {
        assert!(decide_breakeven(&long_position(100.0, 110.0), None, 1.0, 0.1).is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test breakeven::`
Expected: FAIL — `cannot find function decide_breakeven`.

- [ ] **Step 3: Implement `BreakevenAction` + `decide_breakeven`**

In `src/breakeven.rs`, after the three helpers and above the test module, add:

```rust
use crate::hyperliquid::{OpenOrder, OpenPosition};

/// The concrete order change to apply when a stop should be moved to breakeven.
#[derive(Debug, Clone, PartialEq)]
pub struct BreakevenAction {
    pub be_price: f64,
    pub size: f64,
    /// Side of the replacement stop (a long position closes with a sell → false).
    pub close_is_buy: bool,
    /// The order id of the stop to cancel after the new one is placed.
    pub old_oid: u64,
}

/// Decides whether `position`'s stop should be moved to breakeven, given the
/// resting stop-loss order (`stop`) and the configured thresholds. Returns `None`
/// to skip: no stop found, already moved, degenerate risk, profit below threshold,
/// or the computed breakeven price is not a valid stop relative to the mark.
pub fn decide_breakeven(
    position: &OpenPosition,
    stop: Option<&OpenOrder>,
    trigger_r: f64,
    buffer_pct: f64,
) -> Option<BreakevenAction> {
    let stop = stop?;
    let direction = if position.direction.eq_ignore_ascii_case("long") {
        Direction::Long
    } else {
        Direction::Short
    };
    let entry = position.entry_px;
    let stop_price = stop.trigger_price;

    if already_at_breakeven(direction, entry, stop_price) {
        return None;
    }
    if !reached_breakeven_threshold(direction, entry, stop_price, position.mark_px, trigger_r) {
        return None;
    }
    let be_price = breakeven_price(direction, entry, buffer_pct);
    // Guard: the new stop must sit on the correct side of the mark (below for a
    // long, above for a short). Only violated when R*risk < buffer (rare).
    let valid = match direction {
        Direction::Long => be_price < position.mark_px,
        Direction::Short => be_price > position.mark_px,
    };
    if !valid {
        return None;
    }
    let close_is_buy = matches!(direction, Direction::Short);
    Some(BreakevenAction { be_price, size: position.size, close_is_buy, old_oid: stop.oid })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test breakeven::`
Expected: PASS (10 tests total).

- [ ] **Step 5: Commit**

```bash
git add src/breakeven.rs
git commit -m "feat(breakeven): decide_breakeven action from position + resting stop"
```

- [ ] **Step 6: Write the failing test for `apply_breakeven`**

In `src/monitor.rs` `#[cfg(test)] mod tests`, add:

```rust
    #[tokio::test]
    async fn apply_breakeven_places_new_stop_then_cancels_old() {
        use crate::hyperliquid::testing::MockExchange;
        use crate::breakeven::BreakevenAction;
        let mock = MockExchange::new_for_test();
        let action = BreakevenAction { be_price: 100.1, size: 0.5, close_is_buy: false, old_oid: 7 };
        crate::breakeven::apply_breakeven(&mock, "BTC", &action).await.unwrap();
        // New reduce-only stop placed at the breakeven price.
        let triggers = mock.triggers.lock().unwrap();
        assert_eq!(triggers.len(), 1);
        assert!(!triggers[0].is_take_profit);
        assert!((triggers[0].trigger_price - 100.1).abs() < 1e-9);
        // Old stop cancelled by oid.
        let cancels = mock.cancels.lock().unwrap();
        assert_eq!(*cancels, vec![("BTC".to_string(), 7u64)]);
    }
```

- [ ] **Step 7: Run test to verify it fails**

Run: `cargo test apply_breakeven_places_new_stop_then_cancels_old`
Expected: FAIL — `cannot find function apply_breakeven`.

- [ ] **Step 8: Implement `apply_breakeven` in breakeven.rs**

In `src/breakeven.rs`, add (this introduces the exchange dependency; keep it below the pure helpers):

```rust
use crate::hyperliquid::{Exchange, TriggerOrder};

/// Applies a breakeven move: places the new stop FIRST (position never
/// unprotected), then cancels the old stop by id.
pub async fn apply_breakeven<E: Exchange + ?Sized>(
    exchange: &E,
    coin: &str,
    action: &BreakevenAction,
) -> anyhow::Result<()> {
    exchange
        .place_trigger(&TriggerOrder {
            coin: coin.to_string(),
            is_buy: action.close_is_buy,
            size: action.size,
            trigger_price: action.be_price,
            is_take_profit: false,
        })
        .await?;
    exchange.cancel_order(coin, action.old_oid).await?;
    Ok(())
}
```

- [ ] **Step 9: Run test to verify it passes**

Run: `cargo test apply_breakeven_places_new_stop_then_cancels_old`
Expected: PASS.

- [ ] **Step 10: Implement `run_breakeven_monitor` in monitor.rs**

In `src/monitor.rs`, after `run_pnl_monitor`, add:

```rust
/// Polls open positions and moves each stop-loss to breakeven once profit reaches
/// `breakeven_trigger_r` R. Reads settings live each tick; disabled (idle) when
/// `breakeven_enabled` is false. Never panics: all errors are logged.
pub async fn run_breakeven_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    settings: Arc<Mutex<Settings>>,
    allowed_user_ids: Vec<i64>,
    poll_secs: u64,
) {
    let interval = Duration::from_secs(poll_secs.max(1));
    loop {
        tokio::time::sleep(interval).await;

        let (enabled, trigger_r, buffer_pct) = {
            let guard = settings.lock().unwrap();
            (guard.breakeven_enabled, guard.breakeven_trigger_r, guard.breakeven_buffer_pct)
        };
        if !enabled {
            continue;
        }

        let positions = match exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => { tracing::warn!("breakeven monitor positions() failed: {error}"); continue; }
        };

        for position in &positions {
            let orders = match exchange.open_orders(&position.coin).await {
                Ok(orders) => orders,
                Err(error) => { tracing::warn!("breakeven monitor open_orders({}) failed: {error}", position.coin); continue; }
            };
            // The reduce-only stop-loss trigger for this position (not a take-profit).
            let stop = orders.iter().find(|o| o.is_trigger && o.reduce_only && !o.is_take_profit);
            let action = match crate::breakeven::decide_breakeven(position, stop, trigger_r, buffer_pct) {
                Some(action) => action,
                None => continue,
            };
            match crate::breakeven::apply_breakeven(exchange.as_ref(), &position.coin, &action).await {
                Ok(()) => {
                    notify_users(&bot, &allowed_user_ids,
                        &format!("🛡️ SL {} digeser ke breakeven (${:.4}).", position.coin, action.be_price)).await;
                }
                Err(error) => {
                    tracing::warn!("breakeven move for {} failed: {error}", position.coin);
                }
            }
        }
    }
}
```

If `Settings` is not already imported in `monitor.rs`, it is (the file already uses `use crate::settings::Settings;`). `Arc`, `Mutex`, `Duration`, `Bot` are already imported there.

- [ ] **Step 11: Run the monitor + apply tests**

Run: `cargo test breakeven:: && cargo test monitor::`
Expected: PASS.

- [ ] **Step 12: Commit**

```bash
git add src/breakeven.rs src/monitor.rs
git commit -m "feat(breakeven): apply_breakeven + run_breakeven_monitor loop"
```

---

### Task 5: Spawn the breakeven monitor at startup

**Files:**
- Modify: `src/telegram.rs` (the monitor-spawn block near lines 1661–1695)

**Interfaces:**
- Consumes: `run_breakeven_monitor` (Task 4), `context.exchange`, `context.settings`, `context.config.allowed_user_ids`, `context.config.monitor_poll_secs`.

- [ ] **Step 1: Add the spawn block**

In `src/telegram.rs`, after the `run_trigger_monitor` spawn block (around line 1695) and before/after the P&L monitor block, add:

```rust
    // Background breakeven monitor: moves each stop-loss to breakeven once profit
    // reaches breakeven_trigger_r. Reads settings live (disabled = idle). Shares the
    // bot's settings Arc so /set takes effect without restart.
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let monitor_settings = context.settings.clone();
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        let monitor_poll_secs = context.config.monitor_poll_secs;
        tokio::spawn(async move {
            crate::monitor::run_breakeven_monitor(
                monitor_bot, monitor_exchange, monitor_settings, monitor_user_ids, monitor_poll_secs,
            ).await;
        });
    }
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build`
Expected: compiles clean.

- [ ] **Step 3: Run the full suite**

Run: `cargo test`
Expected: PASS (all prior tests + the new breakeven/settings/exchange tests).

- [ ] **Step 4: Lint**

Run: `cargo clippy --all-targets`
Expected: no NEW warnings from the added code (pre-existing test-helper "too many arguments" warnings are acceptable).

- [ ] **Step 5: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(breakeven): spawn run_breakeven_monitor at bot startup"
```

---

## Self-Review

**Spec coverage:**
- R-multiple trigger, all positions → Task 1 (`reached_breakeven_threshold`) + Task 4 (`decide_breakeven`/loop). ✓
- Entry+fee-buffer placement → Task 1 (`breakeven_price`). ✓
- Default off + tunable settings → Task 2. ✓
- Exchange-truth mechanism (open_orders, no journal) → Task 3. ✓
- "Already moved" via stop on profitable side → Task 1 (`already_at_breakeven`) + Task 4. ✓
- Place-new-before-cancel-old ordering → Task 4 (`apply_breakeven`). ✓
- Notification → Task 4 (loop). ✓
- Spawn/wiring → Task 5. ✓
- Testing matrix (unit + MockExchange integration) → Tasks 1–4. ✓

**Type consistency:** `decide_breakeven` returns `Option<BreakevenAction>`; `apply_breakeven` consumes `&BreakevenAction`; field names (`be_price`, `size`, `close_is_buy`, `old_oid`) match across Task 4 definition, tests, and the loop. `OpenOrder` fields match between Task 3 definition, the real/mock impls, and Task 4 usage. Settings field names match between Task 2 struct, `apply_setting`, `persist`, `load`, and the Task 4 loop read.

**Placeholders:** none — every code step shows complete code.
