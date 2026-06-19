# Execution & Close Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Notify the Telegram user at each execution step and whenever a TP/SL closes a position (leg + realized PnL), via a polling background monitor.

**Architecture:** A `ProgressReporter` trait decouples `execute_plan` from Telegram so it can emit concise step events. A new `monitor` module polls `fills_detailed()` every N seconds, dedups via a SQLite `seen_fills` table, classifies each closing fill against the journaled bracket prices, and messages all allowlisted users. The journal gains a `tp_prices` column so closes can be labelled `SL`/`TP1`/`TP2`.

**Tech Stack:** Rust, tokio, teloxide (`Bot`), rusqlite, serde_json, async-trait, hyperliquid_rust_sdk.

## Global Constraints

- Naming: descriptive names, verbs for functions, nouns for values (per repo CLAUDE.md). No abbreviations.
- TDD: every behavioral change is preceded by a failing test. Pure logic must be unit-tested.
- `execute_plan` must remain decoupled from Telegram (testable with `MockExchange`).
- The monitor must never panic the bot: poll/send/DB errors are logged via `tracing` and swallowed.
- SQLite migrations are idempotent (errors ignored), matching the existing `MIGRATIONS` pattern.
- Indonesian user-facing copy, matching existing message style.
- Notifications target every `config.allowed_user_ids` as `teloxide::types::ChatId(user_id)`.

---

### Task 1: Add `MONITOR_POLL_SECS` config

**Files:**
- Modify: `src/config.rs` (struct `Config`, `from_env`, test struct literal `allowlist_membership_check`)

**Interfaces:**
- Produces: `Config.monitor_poll_secs: u64` (default `30`).

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs` `mod tests`:

```rust
#[test]
fn monitor_poll_secs_defaults_to_30_when_absent() {
    std::env::remove_var("MONITOR_POLL_SECS");
    assert_eq!(parse_env_or::<u64>("MONITOR_POLL_SECS", 30).unwrap(), 30);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib config:: 2>&1 | tail -20`
Expected: compile error or assertion — function/behavior present but confirms wiring. (If it passes trivially, proceed; the real wiring is asserted by compilation in Step 3.)

- [ ] **Step 3: Add the field and parsing**

In `struct Config` (after `journal_path`):

```rust
    /// Seconds between background polls of the fill history for close
    /// (TP/SL) notifications. Set via `MONITOR_POLL_SECS` (default 30).
    pub monitor_poll_secs: u64,
```

In `from_env()`'s returned `Config { .. }` (after `journal_path: ...`):

```rust
        monitor_poll_secs: parse_env_or("MONITOR_POLL_SECS", 30_u64)?,
```

In the `allowlist_membership_check` test's `Config { .. }` literal (after `journal_path: "trades.db".into(),`):

```rust
            monitor_poll_secs: 30,
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib config:: 2>&1 | tail -20`
Expected: PASS (all config tests).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add MONITOR_POLL_SECS for fill monitor"
```

---

### Task 2: Journal — persist TP prices and expose the latest bracket

**Files:**
- Modify: `src/journal.rs`

**Interfaces:**
- Consumes: `ExecutionPlan.take_profits: Vec<BracketLeg>`, `BracketLeg.price: f64`, `ExecutionPlan.stop_loss: BracketLeg`.
- Produces:
  - `pub struct Bracket { pub stop_loss: f64, pub take_profits: Vec<f64> }`
  - `Journal::latest_bracket_for_coin(&self, coin: &str) -> anyhow::Result<Option<Bracket>>`
  - `record()` now also writes a `tp_prices` JSON column (no signature change).

- [ ] **Step 1: Write the failing test**

Add to `src/journal.rs` `mod tests`:

```rust
#[test]
fn latest_bracket_for_coin_returns_newest_sl_and_tps() {
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
    journal.record(&plan, None, None, None, None, "Moderate", 1_700_000_000).unwrap();

    let bracket = journal.latest_bracket_for_coin("TAO").unwrap().expect("bracket present");
    assert_eq!(bracket.stop_loss, 280.0);
    assert_eq!(bracket.take_profits, vec![340.0, 380.0]);
    assert!(journal.latest_bracket_for_coin("ETH").unwrap().is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib journal::tests::latest_bracket_for_coin_returns_newest_sl_and_tps 2>&1 | tail -20`
Expected: FAIL — `latest_bracket_for_coin` / `Bracket` not found.

- [ ] **Step 3: Implement**

Add the migration to `MIGRATIONS` (append element):

```rust
    "ALTER TABLE trades ADD COLUMN tp_prices TEXT",
```

Add the struct near `TradeMeta`:

```rust
/// The bracket prices of a journaled trade, used to label which leg
/// (SL / TP1 / TP2) closed a position when a closing fill is observed.
#[derive(Debug, Clone, PartialEq)]
pub struct Bracket {
    pub stop_loss: f64,
    pub take_profits: Vec<f64>,
}
```

In `record()`, before the `connection.execute(..)` call, serialize TP prices:

```rust
        let tp_prices: Vec<f64> = plan.take_profits.iter().map(|leg| leg.price).collect();
        let tp_prices_json = serde_json::to_string(&tp_prices).unwrap_or_else(|_| "[]".to_string());
```

Change the INSERT to include the column and bind `tp_prices_json` as the final param:

```rust
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
```

Add the query method to `impl Journal`:

```rust
    /// Returns the SL + TP prices of the most recent trade for `coin`
    /// (case-insensitive), or `Ok(None)` if no trade was journaled for it.
    ///
    /// Used by the fill monitor to label which bracket leg closed a position.
    pub fn latest_bracket_for_coin(&self, coin: &str) -> anyhow::Result<Option<Bracket>> {
        let connection = self.connection.lock().unwrap();
        let row = connection.query_row(
            "SELECT stop_loss, tp_prices FROM trades
             WHERE coin = ?1 COLLATE NOCASE ORDER BY id DESC LIMIT 1",
            rusqlite::params![coin],
            |row| {
                let stop_loss: f64 = row.get(0)?;
                let tp_json: Option<String> = row.get(1)?;
                Ok((stop_loss, tp_json))
            },
        );
        match row {
            Ok((stop_loss, tp_json)) => {
                let take_profits = tp_json
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<Vec<f64>>(raw).ok())
                    .unwrap_or_default();
                Ok(Some(Bracket { stop_loss, take_profits }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib journal:: 2>&1 | tail -20`
Expected: PASS (new test plus existing journal tests).

- [ ] **Step 5: Commit**

```bash
git add src/journal.rs
git commit -m "feat(journal): persist tp_prices and expose latest_bracket_for_coin"
```

---

### Task 3: Journal — `seen_fills` dedup table

**Files:**
- Modify: `src/journal.rs`

**Interfaces:**
- Produces:
  - `Journal::is_fill_seen(&self, fill_key: &str) -> anyhow::Result<bool>`
  - `Journal::mark_fill_seen(&self, fill_key: &str) -> anyhow::Result<()>`
  - `Journal::seen_fills_empty(&self) -> anyhow::Result<bool>`

- [ ] **Step 1: Write the failing test**

Add to `src/journal.rs` `mod tests`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib journal::tests::seen_fills_dedup_lifecycle 2>&1 | tail -20`
Expected: FAIL — methods not found.

- [ ] **Step 3: Implement**

In `from_connection`, after `connection.execute(SCHEMA, [])?;` add the table creation:

```rust
        connection.execute(
            "CREATE TABLE IF NOT EXISTS seen_fills (fill_key TEXT PRIMARY KEY)",
            [],
        )?;
```

Add methods to `impl Journal`:

```rust
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib journal:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/journal.rs
git commit -m "feat(journal): add seen_fills dedup table for close notifications"
```

---

### Task 4: Monitor — pure helpers (`is_closing`, `fill_key`, `classify_close_fill`)

**Files:**
- Create: `src/monitor.rs`
- Modify: `src/main.rs` (add `mod monitor;`)

**Interfaces:**
- Consumes: `crate::hyperliquid::FillDetail` (fields `coin`, `oid`, `dir`, `px`, `sz`, `closed_pnl`, `time_ms`), `crate::journal::Bracket`.
- Produces:
  - `pub enum CloseLabel { StopLoss, TakeProfit(usize) }`
  - `pub fn is_closing(fill: &FillDetail) -> bool`
  - `pub fn fill_key(fill: &FillDetail) -> String`
  - `pub fn classify_close_fill(fill_price: f64, bracket: &Bracket) -> CloseLabel`

- [ ] **Step 1: Write the failing test**

Create `src/monitor.rs`:

```rust
//! Background fill monitor: polls Hyperliquid fill history and notifies the
//! Telegram user when a TP or SL trigger closes a position.

use crate::hyperliquid::FillDetail;
use crate::journal::Bracket;

/// Which bracket leg a closing fill corresponds to.
#[derive(Debug, Clone, PartialEq)]
pub enum CloseLabel {
    StopLoss,
    /// 1-based take-profit index (TP1, TP2, ...).
    TakeProfit(usize),
}

/// True when a fill closes/reduces a position rather than opening one.
pub fn is_closing(fill: &FillDetail) -> bool {
    fill.dir.to_ascii_lowercase().contains("close") || fill.closed_pnl != 0.0
}

/// Stable dedup key for a single fill: order id, time, price, size.
pub fn fill_key(fill: &FillDetail) -> String {
    format!("{}:{}:{}:{}", fill.oid, fill.time_ms, fill.px, fill.sz)
}

/// Labels a closing fill by matching its price to the nearest bracket leg.
///
/// Compares `fill_price` against the stop-loss price and each take-profit
/// price, returning the leg with the smallest absolute price distance. On a
/// tie the take-profit wins (profit-taking is the common case).
pub fn classify_close_fill(fill_price: f64, bracket: &Bracket) -> CloseLabel {
    let stop_distance = (fill_price - bracket.stop_loss).abs();
    let mut best_label = CloseLabel::StopLoss;
    let mut best_distance = stop_distance;
    for (index, take_profit_price) in bracket.take_profits.iter().enumerate() {
        let distance = (fill_price - take_profit_price).abs();
        if distance <= best_distance {
            best_distance = distance;
            best_label = CloseLabel::TakeProfit(index + 1);
        }
    }
    best_label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(dir: &str, closed_pnl: f64, oid: u64, px: f64, sz: f64, time_ms: i64) -> FillDetail {
        FillDetail {
            coin: "TAO".into(),
            oid,
            dir: dir.into(),
            px,
            sz,
            closed_pnl,
            fee: 0.0,
            time_ms,
            start_position: 0.0,
        }
    }

    #[test]
    fn is_closing_detects_close_dir_and_pnl() {
        assert!(is_closing(&detail("Close Long", 0.0, 1, 1.0, 1.0, 1)));
        assert!(is_closing(&detail("Open Long", 5.0, 1, 1.0, 1.0, 1)));
        assert!(!is_closing(&detail("Open Long", 0.0, 1, 1.0, 1.0, 1)));
    }

    #[test]
    fn fill_key_is_composite_and_stable() {
        let fill = detail("Close Long", 1.0, 42, 300.5, 10.0, 1700);
        assert_eq!(fill_key(&fill), "42:1700:300.5:10");
    }

    #[test]
    fn classify_matches_nearest_leg() {
        let bracket = Bracket { stop_loss: 280.0, take_profits: vec![340.0, 380.0] };
        assert_eq!(classify_close_fill(279.5, &bracket), CloseLabel::StopLoss);
        assert_eq!(classify_close_fill(341.0, &bracket), CloseLabel::TakeProfit(1));
        assert_eq!(classify_close_fill(379.0, &bracket), CloseLabel::TakeProfit(2));
    }

    #[test]
    fn classify_with_only_stop_loss() {
        let bracket = Bracket { stop_loss: 280.0, take_profits: vec![] };
        assert_eq!(classify_close_fill(999.0, &bracket), CloseLabel::StopLoss);
    }
}
```

Add to `src/main.rs` after `mod llm_parser;` (keep alphabetical-ish with siblings):

```rust
mod monitor;
```

- [ ] **Step 2: Run test to verify it passes (helpers are self-contained)**

Run: `cargo test --lib monitor::tests 2>&1 | tail -20`
Expected: PASS (these pure helpers compile and pass immediately; the failing-first discipline is covered by writing tests alongside in this single new file).

- [ ] **Step 3: (already implemented in Step 1)**

No-op — the implementation lives in the same new file as the tests.

- [ ] **Step 4: Verify full lib still builds**

Run: `cargo test --lib 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/monitor.rs src/main.rs
git commit -m "feat(monitor): add pure fill-classification helpers"
```

---

### Task 5: `ProgressReporter` + `ExecutionEvent`, wire into `execute_plan`

**Files:**
- Modify: `src/telegram.rs` (add types, change `execute_plan` signature + event calls, update the two existing `execute_plan` tests)

**Interfaces:**
- Produces:
  - `pub enum ExecutionEvent { EntrySubmitted { limit: bool, price: f64 }, Filled { size: f64, partial: bool }, FillTimeout { cancelled: bool }, BracketArmed { stop_loss: f64, take_profits: usize } }`
  - `pub trait ProgressReporter: Send + Sync { async fn report(&self, event: ExecutionEvent); }`
  - `pub fn format_execution_event(coin: &str, timeout_secs: u64, event: &ExecutionEvent) -> String`
  - `execute_plan(exchange, plan, use_limit, fill_timeout_secs, reporter: &dyn ProgressReporter)`

- [ ] **Step 1: Write the failing test**

Add to `src/telegram.rs` `mod tests` (near the existing `execute_plan_*` tests):

```rust
#[derive(Default)]
struct RecordingReporter {
    events: std::sync::Mutex<Vec<super::ExecutionEvent>>,
}

#[async_trait::async_trait]
impl super::ProgressReporter for RecordingReporter {
    async fn report(&self, event: super::ExecutionEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[tokio::test]
async fn execute_plan_reports_market_event_sequence() {
    let exchange = MockExchange {
        equity: 10_000.0,
        meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
        ..Default::default()
    };
    let plan = plan();
    let reporter = RecordingReporter::default();
    super::execute_plan(&exchange, &plan, false, 1, &reporter).await.unwrap();

    let events = reporter.events.lock().unwrap();
    assert!(matches!(events.first(), Some(super::ExecutionEvent::EntrySubmitted { limit: false, .. })));
    assert!(events.iter().any(|e| matches!(e, super::ExecutionEvent::Filled { partial: false, .. })));
    assert!(matches!(events.last(), Some(super::ExecutionEvent::BracketArmed { take_profits: 2, .. })));
}

#[test]
fn format_execution_event_renders_indonesian_copy() {
    let armed = super::ExecutionEvent::BracketArmed { stop_loss: 280.0, take_profits: 2 };
    let text = super::format_execution_event("TAO", 300, &armed);
    assert!(text.contains("TAO"));
    assert!(text.contains("SL/TP"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib telegram::tests::execute_plan_reports_market_event_sequence 2>&1 | tail -20`
Expected: FAIL — `ProgressReporter`/`ExecutionEvent` not found, `execute_plan` arity mismatch.

- [ ] **Step 3: Implement the types and formatter**

Add near the top of `src/telegram.rs` (after the `use` block, before `render_summary`):

```rust
/// A concise execution-progress event emitted by [`execute_plan`].
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionEvent {
    /// Entry order accepted by the exchange. `limit` distinguishes a resting
    /// limit order (awaiting fill) from a market order (filling now).
    EntrySubmitted { limit: bool, price: f64 },
    /// A fill was observed. `partial` marks a partial fill after a limit timeout.
    Filled { size: f64, partial: bool },
    /// A limit entry did not fill within the timeout. `cancelled` notes the
    /// best-effort cancel of the resting remainder.
    FillTimeout { cancelled: bool },
    /// The reduce-only SL + TP bracket was placed.
    BracketArmed { stop_loss: f64, take_profits: usize },
}

/// Receives [`ExecutionEvent`]s during [`execute_plan`]. Implementations must
/// be best-effort: a failure to deliver must not abort execution.
#[async_trait::async_trait]
pub trait ProgressReporter: Send + Sync {
    async fn report(&self, event: ExecutionEvent);
}

/// Formats an [`ExecutionEvent`] into the Indonesian message shown to the user.
pub fn format_execution_event(coin: &str, timeout_secs: u64, event: &ExecutionEvent) -> String {
    match event {
        ExecutionEvent::EntrySubmitted { limit: true, price } => {
            format!("⏳ Limit {coin} @ ${price:.4} dipasang, menunggu fill…")
        }
        ExecutionEvent::EntrySubmitted { limit: false, .. } => {
            format!("⏳ Entry {coin} dikirim…")
        }
        ExecutionEvent::Filled { size, partial: false } => {
            format!("✅ {coin} terisi (size {size}).")
        }
        ExecutionEvent::Filled { size, partial: true } => {
            format!("⚠️ Partial fill {coin}: bracket dipasang di size {size}.")
        }
        ExecutionEvent::FillTimeout { .. } => {
            format!("❌ Limit {coin} tak terisi dalam {timeout_secs}s — dibatalkan, tidak ada posisi.")
        }
        ExecutionEvent::BracketArmed { stop_loss, take_profits } => {
            format!("✅ SL/TP {coin} terpasang (SL ${stop_loss:.4}, {take_profits} TP).")
        }
    }
}
```

- [ ] **Step 4: Thread the reporter through `execute_plan`**

Change the signature:

```rust
pub async fn execute_plan<E: Exchange>(
    exchange: &E,
    plan: &ExecutionPlan,
    use_limit: bool,
    fill_timeout_secs: u64,
    reporter: &dyn ProgressReporter,
) -> anyhow::Result<()> {
```

After `let entry_result = exchange.place_entry(&entry).await?;` add:

```rust
    reporter
        .report(ExecutionEvent::EntrySubmitted { limit: use_limit, price: plan.entry })
        .await;
```

Inside the `if use_limit && !entry_result.filled` block: at the full-fill branch, before `break plan.size;` add:

```rust
                reporter.report(ExecutionEvent::Filled { size: plan.size, partial: false }).await;
```

In the timeout branch, replace the `if held <= 0.0 { anyhow::bail!(..) }` / partial-fill section with:

```rust
                let held = exchange.position_size(&plan.coin).await?;
                if held <= 0.0 {
                    reporter.report(ExecutionEvent::FillTimeout { cancelled: true }).await;
                    anyhow::bail!(
                        "entry limit order not filled within {fill_timeout_secs}s; \
                         order cancelled, no position opened"
                    );
                }
                reporter.report(ExecutionEvent::Filled { size: held, partial: true }).await;
                break held;
```

In the `else` branch (market / immediate fill), replace `plan.size` with:

```rust
    } else {
        reporter.report(ExecutionEvent::Filled { size: plan.size, partial: false }).await;
        plan.size
    };
```

After the TP loop, before `Ok(())`, add:

```rust
    reporter
        .report(ExecutionEvent::BracketArmed {
            stop_loss: plan.stop_loss.price,
            take_profits: plan.take_profits.len(),
        })
        .await;
```

- [ ] **Step 5: Update the two existing `execute_plan` tests to pass a reporter**

In `execute_plan_sets_leverage_then_entry_then_brackets`, change the call to:

```rust
        let reporter = RecordingReporter::default();
        super::execute_plan(&exchange, &plan, false, 1, &reporter).await.unwrap();
```

In `partial_fill_timeout_cancels_remainder_and_brackets_actual_size`, change the call to:

```rust
        let reporter = RecordingReporter::default();
        super::execute_plan(&exchange, &plan, true, 0, &reporter).await.unwrap();
```

- [ ] **Step 6: Run tests**

Run: `cargo test --lib telegram:: 2>&1 | tail -30`
Expected: PASS (new + both updated existing tests).

- [ ] **Step 7: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): emit execution-step events via ProgressReporter"
```

---

### Task 6: Wire `TelegramReporter` into the confirm handler

**Files:**
- Modify: `src/telegram.rs` (confirm handler around lines 866–935)

**Interfaces:**
- Consumes: `ExecutionEvent`, `ProgressReporter`, `format_execution_event`, `execute_plan` (Task 5).
- Produces: `struct TelegramReporter { bot: Bot, chat_id: ChatId, coin: String, timeout_secs: u64 }` implementing `ProgressReporter`.

- [ ] **Step 1: Add the production reporter**

Add to `src/telegram.rs` (below `format_execution_event`):

```rust
/// Production [`ProgressReporter`] that forwards each event to a Telegram chat.
/// Send failures are logged and swallowed so execution never aborts mid-bracket.
struct TelegramReporter {
    bot: Bot,
    chat_id: teloxide::types::ChatId,
    coin: String,
    timeout_secs: u64,
}

#[async_trait::async_trait]
impl ProgressReporter for TelegramReporter {
    async fn report(&self, event: ExecutionEvent) {
        let text = format_execution_event(&self.coin, self.timeout_secs, &event);
        if let Err(error) = self.bot.send_message(self.chat_id, text).await {
            tracing::warn!("progress notification failed: {error}");
        }
    }
}
```

- [ ] **Step 2: Use the reporter in the confirm handler**

Replace the `match execute_plan(..).await { Ok(()) => { .. } Err(error) => { .. } }` block (lines ~887–934) so it constructs a reporter, passes it, drops the now-redundant `✅ Executed …` success message (the `BracketArmed` event is the success message), and keeps journalling + the error message:

```rust
    let reporter = TelegramReporter {
        bot: bot.clone(),
        chat_id: message.chat.id,
        coin: trade.plan.coin.clone(),
        timeout_secs: context.config.entry_fill_timeout_secs,
    };

    match execute_plan(
        context.exchange.as_ref(),
        &trade.plan,
        use_limit,
        context.config.entry_fill_timeout_secs,
        &reporter,
    )
    .await
    {
        Ok(()) => {
            // Journal every attempt so a position that was opened is always
            // auditable. The user-facing success message was already sent by
            // the reporter's BracketArmed event.
            let _ = context.journal.record(
                &trade.plan,
                None,
                trade_confidence,
                trade_timeframe.as_deref(),
                trade_risk_reward,
                &trade_profile,
                opened_at,
            );
        }
        Err(error) => {
            // Journal on failure too: a partial fill may have opened a position
            // even when execute_plan returns Err.
            let _ = context.journal.record(
                &trade.plan,
                None,
                trade_confidence,
                trade_timeframe.as_deref(),
                trade_risk_reward,
                &trade_profile,
                opened_at,
            );
            bot.send_message(
                message.chat.id,
                format!("❌ Execution failed: {error}"),
            )
            .await?;
        }
    }
    Ok(())
```

- [ ] **Step 3: Build and run the full suite**

Run: `cargo test 2>&1 | tail -30`
Expected: PASS (no behavioral test regressions; handler compiles with the new reporter).

- [ ] **Step 4: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): drive confirm-flow notifications through TelegramReporter"
```

---

### Task 7: Fill-monitor loop + spawn in `run`

**Files:**
- Modify: `src/monitor.rs` (add the loop + message formatter), `src/telegram.rs` (`run`: spawn the monitor)

**Interfaces:**
- Consumes: `is_closing`, `fill_key`, `classify_close_fill`, `CloseLabel` (Task 4); `Journal::{latest_bracket_for_coin, is_fill_seen, mark_fill_seen, seen_fills_empty}` (Tasks 2–3); `Exchange::fills_detailed` (existing).
- Produces:
  - `pub fn format_close_message(coin: &str, label: Option<CloseLabel>, closed_pnl: f64) -> String`
  - `pub async fn run_fill_monitor<E: Exchange + 'static>(bot: Bot, exchange: Arc<E>, journal: Arc<Journal>, allowed_user_ids: Vec<i64>, poll_secs: u64)`

- [ ] **Step 1: Write the failing test for the formatter**

Add to `src/monitor.rs` `mod tests`:

```rust
#[test]
fn format_close_message_labels_leg_and_pnl() {
    let tp = format_close_message("TAO", Some(CloseLabel::TakeProfit(1)), 12.30);
    assert!(tp.contains("TP1"));
    assert!(tp.contains("TAO"));
    assert!(tp.contains("+$12.30"));

    let sl = format_close_message("TAO", Some(CloseLabel::StopLoss), -8.10);
    assert!(sl.contains("SL"));
    assert!(sl.contains("-$8.10"));

    let generic = format_close_message("TAO", None, 5.0);
    assert!(generic.contains("ditutup"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib monitor::tests::format_close_message_labels_leg_and_pnl 2>&1 | tail -20`
Expected: FAIL — `format_close_message` not found.

- [ ] **Step 3: Implement the formatter and loop**

Add to `src/monitor.rs` (after the helpers, before `#[cfg(test)]`):

```rust
use crate::hyperliquid::Exchange;
use crate::journal::Journal;
use std::sync::Arc;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::ChatId;

/// Formats a signed USD PnL as `+$12.30` / `-$8.10`.
fn format_pnl(closed_pnl: f64) -> String {
    let sign = if closed_pnl < 0.0 { "-" } else { "+" };
    format!("{sign}${:.2}", closed_pnl.abs())
}

/// Builds the Telegram message for a closed position.
pub fn format_close_message(coin: &str, label: Option<CloseLabel>, closed_pnl: f64) -> String {
    let pnl = format_pnl(closed_pnl);
    match label {
        Some(CloseLabel::TakeProfit(index)) => format!("🎯 TP{index} kena — {coin} {pnl}"),
        Some(CloseLabel::StopLoss) => format!("🛑 SL kena — {coin} {pnl}"),
        None => format!("📕 Posisi {coin} ditutup — {pnl}"),
    }
}

/// Polls fill history every `poll_secs` and notifies all `allowed_user_ids`
/// when a TP/SL closing fill is observed. Never panics: poll/send/DB errors are
/// logged and the loop continues.
///
/// On a brand-new database (`seen_fills` empty) the first pass baselines all
/// historical closing fills silently so the user is not spammed at first boot.
pub async fn run_fill_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    journal: Arc<Journal>,
    allowed_user_ids: Vec<i64>,
    poll_secs: u64,
) {
    // Baseline: silence pre-existing fills on a fresh database.
    if journal.seen_fills_empty().unwrap_or(false) {
        if let Ok(fills) = exchange.fills_detailed().await {
            for fill in fills.iter().filter(|fill| is_closing(fill)) {
                let _ = journal.mark_fill_seen(&fill_key(fill));
            }
        }
        tracing::info!("fill monitor baselined historical fills");
    }

    let interval = Duration::from_secs(poll_secs.max(1));
    loop {
        tokio::time::sleep(interval).await;

        let fills = match exchange.fills_detailed().await {
            Ok(fills) => fills,
            Err(error) => {
                tracing::warn!("fill monitor poll failed: {error}");
                continue;
            }
        };

        for fill in fills.iter().filter(|fill| is_closing(fill)) {
            let key = fill_key(fill);
            if journal.is_fill_seen(&key).unwrap_or(false) {
                continue;
            }
            let label = journal
                .latest_bracket_for_coin(&fill.coin)
                .ok()
                .flatten()
                .map(|bracket| classify_close_fill(fill.px, &bracket));
            let message = format_close_message(&fill.coin, label, fill.closed_pnl);
            for user_id in &allowed_user_ids {
                if let Err(error) = bot.send_message(ChatId(*user_id), &message).await {
                    tracing::warn!("close notification failed for {user_id}: {error}");
                }
            }
            let _ = journal.mark_fill_seen(&key);
        }
    }
}
```

- [ ] **Step 4: Spawn the monitor in `telegram::run`**

In `src/telegram.rs` `run`, after the `context` is built (after line ~959) and before `let handler = ...`, add:

```rust
    // Background close (TP/SL) notifications. Uses its own Journal connection
    // (separate SQLite handle) so it never contends with the bot's writes.
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let monitor_journal = Arc::new(Journal::open(&journal_path)?);
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        let monitor_poll_secs = context.config.monitor_poll_secs;
        tokio::spawn(async move {
            crate::monitor::run_fill_monitor(
                monitor_bot,
                monitor_exchange,
                monitor_journal,
                monitor_user_ids,
                monitor_poll_secs,
            )
            .await;
        });
    }
```

- [ ] **Step 5: Run the full suite + build**

Run: `cargo test 2>&1 | tail -30`
Expected: PASS.

Run: `cargo build 2>&1 | tail -10`
Expected: builds clean (warnings acceptable).

- [ ] **Step 6: Commit**

```bash
git add src/monitor.rs src/telegram.rs
git commit -m "feat(monitor): poll fills and send TP/SL close notifications"
```

---

### Task 8: Document the new env var and notifications

**Files:**
- Modify: `.env.example`, `README.md`

- [ ] **Step 1: Add `MONITOR_POLL_SECS` to `.env.example`**

After the `ENTRY_FILL_TIMEOUT_SECS=300` line:

```
# Seconds between background polls for TP/SL close notifications (default 30).
MONITOR_POLL_SECS=30
```

- [ ] **Step 2: Document notifications in `README.md`**

In the `## Usage` section, append a paragraph:

```markdown
While a trade executes the bot reports each step (entry submitted, fill, bracket
armed; partial-fill and timeout are flagged). A background monitor polls fill
history every `MONITOR_POLL_SECS` (default 30s) and messages you when a TP or SL
closes a position, naming the leg (TP1/TP2/SL) and the realized PnL.
```

- [ ] **Step 3: Commit**

```bash
git add .env.example README.md
git commit -m "docs: document MONITOR_POLL_SECS and trade notifications"
```

---

### Task 9: Make `entry_fill_timeout_secs` editable from Settings (`/set`)

**Files:**
- Modify: `src/settings.rs` (struct `Settings`, `from_config`, `apply_setting`, `VALID_KEYS`, `persist`, `load`, tests)
- Modify: `src/telegram.rs` (`render_settings`, confirm handler — read timeout from settings instead of config)

**Interfaces:**
- Produces: `Settings.entry_fill_timeout_secs: u64`; new `/set` key `entry_fill_timeout_secs` (whole number ≥ 1).
- Supersedes Task 6: the confirm handler reads the timeout from the live `Settings`, not `context.config.entry_fill_timeout_secs`.

- [ ] **Step 1: Write the failing tests**

Add to `src/settings.rs` `mod tests` and update `sample()`:

```rust
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
```

Update the `sample()` helper to include the field:

```rust
    fn sample() -> Settings {
        Settings {
            entry_mode: EntryMode::RiskBased,
            risk_pct: 1.0,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            max_daily_risk_pct: Some(5.0),
            leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
            entry_fill_timeout_secs: 300,
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib settings:: 2>&1 | tail -20`
Expected: FAIL — field/key missing, `sample()` literal mismatch.

- [ ] **Step 3: Implement**

Add the field to `struct Settings` (after `leverage`):

```rust
    /// Seconds to wait for a limit entry to fill before cancelling.
    pub entry_fill_timeout_secs: u64,
```

In `Settings::from_config` (after `leverage: config.leverage,`):

```rust
            entry_fill_timeout_secs: config.entry_fill_timeout_secs,
```

Add a parser (near `parse_leverage`):

```rust
/// Parses a fill-timeout in seconds; must be a whole number of at least 1.
fn parse_timeout_secs(value: &str) -> Result<u64, String> {
    let parsed: u64 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
    if parsed < 1 {
        return Err("entry_fill_timeout_secs must be >= 1".to_string());
    }
    Ok(parsed)
}
```

Add the match arm in `apply_setting` (before the `_ =>` arm):

```rust
        "entry_fill_timeout_secs" => next.entry_fill_timeout_secs = parse_timeout_secs(value)?,
```

Extend `VALID_KEYS`:

```rust
const VALID_KEYS: &str = "entry_mode, risk_pct, entry_pct, entry_fixed_usd, \
max_daily_risk_pct, entry_fill_timeout_secs, leverage_conservative, leverage_moderate, leverage_aggressive";
```

In `SettingsStore::persist` (after the leverage puts):

```rust
        self.put("entry_fill_timeout_secs", &settings.entry_fill_timeout_secs.to_string())?;
```

In `SettingsStore::load` (before the final `self.persist(&resolved)?;`):

```rust
        if let Some(raw) = self.get("entry_fill_timeout_secs")? {
            match raw.parse() {
                Ok(value) => resolved.entry_fill_timeout_secs = value,
                Err(_) => tracing::warn!(key = "entry_fill_timeout_secs", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
```

- [ ] **Step 4: Show it in `render_settings` (`src/telegram.rs`)**

Add a line to the format string (after the `leverage_aggressive: {}x\n` line) and a matching arg:

```rust
         leverage_aggressive: {}x\n\
         entry_fill_timeout_secs: {}s\n\n\
```

Add `settings.entry_fill_timeout_secs,` as the final format argument (after `settings.leverage.aggressive,`).

- [ ] **Step 5: Read the timeout from Settings in the confirm handler**

In the confirm handler (Task 6's block), before constructing the reporter, read the live value (copy it out of the lock — never hold a lock across `await`):

```rust
    let fill_timeout_secs = context.settings.lock().unwrap().entry_fill_timeout_secs;
```

Then use `fill_timeout_secs` in both the `TelegramReporter { .. timeout_secs: fill_timeout_secs }` and the `execute_plan(.., fill_timeout_secs, &reporter)` call, replacing `context.config.entry_fill_timeout_secs`.

- [ ] **Step 6: Run the full suite**

Run: `cargo test 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/settings.rs src/telegram.rs
git commit -m "feat(settings): make entry_fill_timeout_secs editable via /set"
```

---

### Task 10: Non-blocking parallel execution

**Files:**
- Modify: `src/telegram.rs` (confirm handler — wrap execution in `tokio::spawn`)
- Modify: `README.md` (note parallel execution)

**Interfaces:**
- Consumes: `TelegramReporter` (Task 6), `execute_plan` (Task 5), settings timeout read (Task 9).
- Depends on Tasks 5, 6, 9 being complete.

**Context:** The confirm handler currently `.await`s `execute_plan` inline. teloxide
processes updates from the same chat sequentially, so a limit order's fill-wait (up to
`entry_fill_timeout_secs`) blocks any further setup from the same user until it
finishes. Wrapping execution in `tokio::spawn` lets the handler return immediately so
multiple entries run concurrently; the async progress notifications (Tasks 5–7) carry
all status back to the user.

**Verification note:** the confirm handler is not unit-tested (it requires a live
`teloxide::Bot` and network), consistent with the existing codebase. Task 10 is glue
around the already-tested `execute_plan`; verification is `cargo build`, the full
existing suite staying green, and a manual smoke check. No fake unit test is added.

- [ ] **Step 1: Spawn the execution body**

In the confirm handler, keep the synchronous prelude (the daily-risk-cap check,
`answer_callback_query`, and the `edit_message_text("Executing {coin}…")`) exactly as
is. Then replace the metadata capture + reporter + `match execute_plan { .. }` block
(the section added/modified in Tasks 6 and 9) with a spawned task:

```rust
    // Read the live timeout before spawning (never hold the lock across await).
    let fill_timeout_secs = context.settings.lock().unwrap().entry_fill_timeout_secs;

    // Execute off the handler so the chat is not blocked during a limit
    // order's fill-wait — multiple entries can run concurrently. All status
    // is delivered asynchronously via TelegramReporter.
    let task_context = context.clone();
    let task_bot = bot.clone();
    let chat_id = message.chat.id;
    tokio::spawn(async move {
        let opened_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        let trade_confidence = trade.setup.confidence;
        let trade_timeframe = trade.setup.timeframe.clone();
        let trade_risk_reward = trade.setup.risk_reward;
        let trade_profile = format!("{:?}", trade.profile);

        let reporter = TelegramReporter {
            bot: task_bot.clone(),
            chat_id,
            coin: trade.plan.coin.clone(),
            timeout_secs: fill_timeout_secs,
        };

        let outcome = execute_plan(
            task_context.exchange.as_ref(),
            &trade.plan,
            use_limit,
            fill_timeout_secs,
            &reporter,
        )
        .await;

        // Journal every attempt (a partial fill may open a position even on Err).
        let _ = task_context.journal.record(
            &trade.plan,
            None,
            trade_confidence,
            trade_timeframe.as_deref(),
            trade_risk_reward,
            &trade_profile,
            opened_at,
        );

        if let Err(error) = outcome {
            if let Err(send_error) = task_bot
                .send_message(chat_id, format!("❌ Execution failed: {error}"))
                .await
            {
                tracing::warn!("failed to send execution error: {send_error}");
            }
        }
    });
    Ok(())
```

Ensure any now-unused bindings from the prior inline version (e.g. a pre-spawn
`opened_at`) are removed so there are no unused-variable warnings.

- [ ] **Step 2: Build and run the full suite**

Run: `cargo build 2>&1 | tail -15`
Expected: builds clean (no unused-variable/borrow errors).

Run: `cargo test 2>&1 | tail -20`
Expected: PASS (existing suite unaffected).

- [ ] **Step 3: Note parallel execution in `README.md`**

In `## Usage`, append:

```markdown
Executions run in the background, so you can submit and confirm multiple setups
without waiting for a prior limit order to fill.
```

- [ ] **Step 4: Commit**

```bash
git add src/telegram.rs README.md
git commit -m "feat(telegram): run execution off-handler for parallel opens"
```

**Known minor (roll-up):** two confirms racing can both pass the daily-risk-cap check
before either journals its `risk_amount`, slightly overshooting the cap. Acceptable for
single-user use; revisit with a serialized cap-reservation if it matters.

---

## Self-Review Notes

- **Spec coverage:** Component 1 (ProgressReporter) → Tasks 5–6. Component 2 (poll monitor, dedup, baseline, target chat, failure isolation) → Tasks 3, 4, 7. Component 3 (tp_prices, latest_bracket_for_coin, classify, messages) → Tasks 2, 4, 7. Config `MONITOR_POLL_SECS` → Task 1. Docs → Task 8.
- **Type consistency:** `Bracket { stop_loss, take_profits }`, `CloseLabel { StopLoss, TakeProfit(usize) }`, `ExecutionEvent` variants, and `execute_plan`'s 5-arg signature are used identically across Tasks 2/4/5/6/7.
- **FillDetail fields** (`coin/oid/dir/px/sz/closed_pnl/fee/time_ms/start_position`) verified against `src/hyperliquid/mod.rs`.
- **No call-site change** to `journal.record` — TP prices are derived from `plan.take_profits` inside `record`.
- **Task 9 (added post-spec)** makes `entry_fill_timeout_secs` runtime-editable via `/set`, superseding Task 6's `context.config.entry_fill_timeout_secs` read with a live `Settings` lookup. Every `Settings` struct literal (incl. `settings.rs` `sample()` and any other test) must add the `entry_fill_timeout_secs` field or it won't compile.
```
