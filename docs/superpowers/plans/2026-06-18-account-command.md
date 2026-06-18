# `/account` Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/account` Telegram command that shows current equity, open perp positions, and daily-risk-used on demand.

**Architecture:** A new `OpenPosition` domain type and an `Exchange::open_positions()` method read `user_state().asset_positions` (uniform across unified/non-unified). A pure `build_open_position` mapper does the field parsing so it is unit-testable without the network. A plain-text `render_account` renderer and a `/account` interception in `on_message` (mirroring the existing `/stats` handler) assemble equity + positions + daily-cap into one message.

**Tech Stack:** Rust 2021, teloxide 0.13, hyperliquid_rust_sdk 0.6, async-trait, anyhow.

## Global Constraints

- `/account` output is PLAIN text — send WITHOUT `.parse_mode(...)` (matches `render_stats`/`render_settings`).
- Positions come from `info.user_state(address).asset_positions`; the same path works in unified and non-unified modes (no mode branching for positions). Only `equity()` differs by mode, and it already exists.
- The "Daily risk used" line appears ONLY when `max_daily_risk_pct` is `Some`; omitted when the cap is disabled.
- Never hold a `std::sync::Mutex` guard across an `.await`: copy `max_daily_risk_pct` (an `Option<f64>`, `Copy`) out of the lock in one statement.
- SDK field names (verified): `PositionData { coin: String, szi: String, entry_px: Option<String>, unrealized_pnl: String, leverage: Leverage, liquidation_px: Option<String>, .. }`; `Leverage { value: u32, .. }`.
- Daily-risk-used = `journal.risk_used_since(crate::risk::start_of_utc_day(now_secs))`, mirroring the existing daily-cap check.

---

### Task 1: `OpenPosition` type + `open_positions()` Exchange method

**Files:**
- Modify: `src/hyperliquid/mod.rs`

**Interfaces:**
- Produces: `pub struct OpenPosition { coin: String, is_long: bool, size: f64, entry_px: f64, unrealized_pnl: f64, leverage: u32, liquidation_px: Option<f64> }` (derives `Debug, Clone, PartialEq`); `Exchange::open_positions(&self) -> anyhow::Result<Vec<OpenPosition>>`; pure `build_open_position(coin: &str, szi: &str, entry_px: Option<&str>, unrealized_pnl: &str, leverage: u32, liquidation_px: Option<&str>) -> anyhow::Result<Option<OpenPosition>>`.

- [ ] **Step 1: Write failing tests for the pure mapper**

Add to the `#[cfg(test)] mod tests` block in `src/hyperliquid/mod.rs` (it already exists near the bottom; if no `tests` module exists, create one with `use super::*;`):

```rust
    #[test]
    fn build_open_position_maps_long() {
        let op = super::build_open_position("BTC", "0.05", Some("61200.0"), "45.2", 3, Some("52100.0"))
            .unwrap()
            .unwrap();
        assert_eq!(op, super::OpenPosition {
            coin: "BTC".into(), is_long: true, size: 0.05,
            entry_px: 61200.0, unrealized_pnl: 45.2, leverage: 3,
            liquidation_px: Some(52100.0),
        });
    }

    #[test]
    fn build_open_position_maps_short_from_negative_szi() {
        let op = super::build_open_position("ETH", "-1.2", Some("3410.0"), "-12.8", 2, None)
            .unwrap()
            .unwrap();
        assert!(!op.is_long);
        assert_eq!(op.size, 1.2);
        assert_eq!(op.liquidation_px, None);
    }

    #[test]
    fn build_open_position_skips_zero_size() {
        assert_eq!(super::build_open_position("BTC", "0", None, "0", 1, None).unwrap(), None);
    }

    #[test]
    fn build_open_position_defaults_missing_entry_and_bad_pnl() {
        let op = super::build_open_position("SOL", "10", None, "not-a-number", 5, None)
            .unwrap()
            .unwrap();
        assert_eq!(op.entry_px, 0.0);
        assert_eq!(op.unrealized_pnl, 0.0);
    }

    #[test]
    fn build_open_position_errors_on_bad_szi() {
        assert!(super::build_open_position("BTC", "xyz", None, "0", 1, None).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib hyperliquid::tests::build_open_position 2>&1 | tail -20`
Expected: FAIL — `build_open_position` / `OpenPosition` not found.

- [ ] **Step 3: Add the `OpenPosition` struct + pure mapper**

In `src/hyperliquid/mod.rs`, after the `Fill` struct (in the "Public domain types" area), add:

```rust
// ── Open position (live account state) ───────────────────────────────────────

/// A currently-open perp position, read from `user_state().asset_positions`.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPosition {
    pub coin: String,
    pub is_long: bool,
    pub size: f64,            // absolute size in coin units
    pub entry_px: f64,        // 0.0 when the exchange reports none
    pub unrealized_pnl: f64,
    pub leverage: u32,
    pub liquidation_px: Option<f64>,
}

/// Maps raw position fields (as strings from the SDK) into an `OpenPosition`.
/// Returns `Ok(None)` for a zero-size position (caller skips it). Errors only
/// when `szi` — the field that defines the position — is malformed; the
/// display-only fields fall back (`entry_px`/`unrealized_pnl` → 0.0,
/// `liquidation_px` → None) rather than failing the whole report.
pub(crate) fn build_open_position(
    coin: &str,
    szi: &str,
    entry_px: Option<&str>,
    unrealized_pnl: &str,
    leverage: u32,
    liquidation_px: Option<&str>,
) -> anyhow::Result<Option<OpenPosition>> {
    let signed = szi
        .parse::<f64>()
        .map_err(|e| anyhow::anyhow!("cannot parse position szi {szi:?}: {e}"))?;
    if signed == 0.0 {
        return Ok(None);
    }
    Ok(Some(OpenPosition {
        coin: coin.to_string(),
        is_long: signed > 0.0,
        size: signed.abs(),
        entry_px: entry_px.and_then(|s| s.parse().ok()).unwrap_or(0.0),
        unrealized_pnl: unrealized_pnl.parse().unwrap_or(0.0),
        leverage,
        liquidation_px: liquidation_px.and_then(|s| s.parse().ok()),
    }))
}
```

- [ ] **Step 4: Run mapper tests to verify they pass**

Run: `cargo test --lib hyperliquid::tests::build_open_position 2>&1 | tail -20`
Expected: PASS (5 tests).

- [ ] **Step 5: Add the trait method + real + mock impls**

In `src/hyperliquid/mod.rs`:

(a) Add to the `Exchange` trait (after `open_order_count`):

```rust
    /// Returns all currently-open perp positions (zero-size entries skipped).
    /// Reads `user_state().asset_positions`; identical in unified/non-unified modes.
    async fn open_positions(&self) -> anyhow::Result<Vec<OpenPosition>>;
```

(b) Add a field to `MockExchange` (in the `#[cfg(test)] pub mod mock` block, alongside the other `Mutex` fields — the struct derives `Default`, and `Mutex<Vec<_>>` defaults to empty):

```rust
        /// Positions returned verbatim by `open_positions`.
        pub positions: Mutex<Vec<super::OpenPosition>>,
```

(c) Add the mock impl method (inside `impl Exchange for MockExchange`):

```rust
        async fn open_positions(&self) -> anyhow::Result<Vec<super::OpenPosition>> {
            Ok(self.positions.lock().unwrap().clone())
        }
```

(d) Add the real impl (inside `impl Exchange for HyperliquidExchange`, near `position_size`):

```rust
    async fn open_positions(&self) -> anyhow::Result<Vec<OpenPosition>> {
        let state = self.info.user_state(self.address).await?;
        let mut positions = Vec::new();
        for asset_position in &state.asset_positions {
            let position = &asset_position.position;
            if let Some(open_position) = build_open_position(
                &position.coin,
                &position.szi,
                position.entry_px.as_deref(),
                &position.unrealized_pnl,
                position.leverage.value,
                position.liquidation_px.as_deref(),
            )? {
                positions.push(open_position);
            }
        }
        Ok(positions)
    }
```

- [ ] **Step 6: Add a mock round-trip test**

Add to the `#[cfg(test)] mod tests` block:

```rust
    #[tokio::test]
    async fn mock_returns_preset_positions() {
        use crate::hyperliquid::mock::MockExchange;
        let exchange = MockExchange::default();
        exchange.positions.lock().unwrap().push(super::OpenPosition {
            coin: "BTC".into(), is_long: true, size: 0.05, entry_px: 61200.0,
            unrealized_pnl: 45.2, leverage: 3, liquidation_px: Some(52100.0),
        });
        let positions = super::Exchange::open_positions(&exchange).await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].coin, "BTC");
    }
```

- [ ] **Step 7: Run the full suite**

Run: `cargo test 2>&1 | tail -25`
Expected: PASS (existing tests + the 6 new ones).

- [ ] **Step 8: Commit**

Stage ONLY `src/hyperliquid/mod.rs` (do not `git add .`; leave untracked files alone):

```bash
git add src/hyperliquid/mod.rs
git commit -m "feat(hyperliquid): add OpenPosition type and open_positions()"
```

---

### Task 2: `render_account` + `/account` handler

**Files:**
- Modify: `src/telegram.rs`

**Interfaces:**
- Consumes: `crate::hyperliquid::OpenPosition`, `Exchange::open_positions`, `Exchange::equity`, `crate::risk::start_of_utc_day`, `journal.risk_used_since`, `context.settings.max_daily_risk_pct`.
- Produces: `pub fn render_account(equity: f64, positions: &[OpenPosition], used_today: f64, cap_pct: Option<f64>) -> String`.

- [ ] **Step 1: Write failing tests for `render_account`**

Add to the `#[cfg(test)] mod tests` block in `src/telegram.rs`:

```rust
    fn sample_positions() -> Vec<crate::hyperliquid::OpenPosition> {
        vec![
            crate::hyperliquid::OpenPosition {
                coin: "BTC".into(), is_long: true, size: 0.05, entry_px: 61200.0,
                unrealized_pnl: 45.2, leverage: 3, liquidation_px: Some(52100.0),
            },
            crate::hyperliquid::OpenPosition {
                coin: "ETH".into(), is_long: false, size: 1.2, entry_px: 3410.0,
                unrealized_pnl: -12.8, leverage: 2, liquidation_px: None,
            },
        ]
    }

    #[test]
    fn account_card_lists_positions_and_cap() {
        let text = super::render_account(1234.56, &sample_positions(), 12.3, Some(5.0));
        assert!(text.contains("Equity: $1234.56"));
        assert!(text.contains("Daily risk used:"));
        assert!(text.contains("Open positions (2):"));
        assert!(text.contains("BTC"));
        assert!(text.contains("LONG"));
        assert!(text.contains("SHORT"));
        assert!(text.contains("uPnL $+45.20")); // positive sign
        assert!(text.contains("uPnL $-12.80")); // negative sign
        assert!(text.contains("liq $52100.00"));
        assert!(text.contains("liq -")); // ETH has no liquidation price
    }

    #[test]
    fn account_card_flat_when_no_positions() {
        let text = super::render_account(1000.0, &[], 0.0, Some(5.0));
        assert!(text.contains("Flat"));
        assert!(!text.contains("Open positions"));
    }

    #[test]
    fn account_card_omits_daily_risk_when_cap_disabled() {
        let text = super::render_account(1000.0, &sample_positions(), 0.0, None);
        assert!(!text.contains("Daily risk used"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib telegram::tests::account_card 2>&1 | tail -20`
Expected: FAIL — `render_account` not found.

- [ ] **Step 3: Implement `render_account`**

In `src/telegram.rs`, add near `render_settings` (and add `use crate::hyperliquid::OpenPosition;` to the imports if `OpenPosition` is not already in scope):

```rust
/// Plain-text account card (no parse_mode). The daily-risk line is omitted
/// when the cap is disabled (`cap_pct` is `None`).
pub fn render_account(
    equity: f64,
    positions: &[OpenPosition],
    used_today: f64,
    cap_pct: Option<f64>,
) -> String {
    let mut out = String::new();
    out.push_str("💰 Account\n");
    out.push_str(&format!("Equity: ${:.2}\n", equity));
    if let Some(cap) = cap_pct {
        let cap_amount = equity * cap / 100.0;
        out.push_str(&format!(
            "Daily risk used: ${:.2} / ${:.2} ({}%)\n",
            used_today, cap_amount, cap
        ));
    }
    if positions.is_empty() {
        out.push_str("\nFlat — no open positions.\n");
        return out;
    }
    out.push_str(&format!("\nOpen positions ({}):\n", positions.len()));
    for position in positions {
        let direction = if position.is_long { "LONG" } else { "SHORT" };
        let liquidation = match position.liquidation_px {
            Some(price) => format!("${:.2}", price),
            None => "-".to_string(),
        };
        out.push_str(&format!(
            "  {:<6} {:<5} {} @ ${:.2}  uPnL ${:+.2}  {}x  liq {}\n",
            position.coin,
            direction,
            position.size,
            position.entry_px,
            position.unrealized_pnl,
            position.leverage,
            liquidation,
        ));
    }
    out
}
```

- [ ] **Step 4: Run render tests to verify they pass**

Run: `cargo test --lib telegram::tests::account_card 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 5: Add the `/account` interception in `on_message`**

In `src/telegram.rs`, immediately AFTER the existing `/stats` interception block (the `if text.split_whitespace()...eq_ignore_ascii_case("/stats")` block) and BEFORE the `/settings` block, insert:

```rust
    // /account — live equity + open positions + daily-risk-used. Like /stats,
    // it needs the async exchange + journal + settings, so it is handled here.
    if text
        .split_whitespace()
        .next()
        .map(|w| w.split('@').next().unwrap_or("").eq_ignore_ascii_case("/account"))
        .unwrap_or(false)
    {
        let equity = match context.exchange.equity().await {
            Ok(value) => value,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not fetch account state: {error}")).await?;
                return Ok(());
            }
        };
        let positions = match context.exchange.open_positions().await {
            Ok(value) => value,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not fetch account state: {error}")).await?;
                return Ok(());
            }
        };
        let cap_pct = context.settings.lock().unwrap().max_daily_risk_pct;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let used_today = context
            .journal
            .risk_used_since(crate::risk::start_of_utc_day(now_secs))
            .unwrap_or(0.0);
        bot.send_message(message.chat.id, render_account(equity, &positions, used_today, cap_pct))
            .await?;
        return Ok(());
    }
```

- [ ] **Step 6: Add `/account` to the help text**

In `src/telegram.rs`, find the `Commands:` line in the `WELCOME_TEXT` constant (it currently ends with `…/settings, /set <key> <value>`). Add `/account`:

```
Commands: /start, /help, /stats, /account, /settings, /set <key> <value>
```

- [ ] **Step 7: Run the full suite**

Run: `cargo test 2>&1 | tail -25`
Expected: PASS (all tests including the 3 new render tests).

- [ ] **Step 8: Commit**

Stage ONLY `src/telegram.rs`:

```bash
git add src/telegram.rs
git commit -m "feat(telegram): add /account command for live balance and positions"
```

---

## Self-Review Notes

- **Spec coverage:** `OpenPosition` + `open_positions()` (uniform across modes) → Task 1. Pure mapper for testability → Task 1 (`build_open_position`). `render_account` plain text + daily-risk-line-only-when-cap → Task 2. `/account` handler (equity + positions + used-today, error handling) → Task 2 Step 5. Help text → Task 2 Step 6. Mock support for positions → Task 1 Step 5b/5c.
- **Manual verification (after Task 2):** `cargo run`, then in Telegram `/account` on a flat account → "Flat — no open positions."; open a position via a signal card, `/account` → shows the row with LONG/SHORT, entry, uPnL, leverage, liq.
- Each task touches one file and ends green and independently reviewable.
