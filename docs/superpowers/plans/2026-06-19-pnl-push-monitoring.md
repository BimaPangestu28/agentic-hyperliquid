# P&L Push Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Periodically push a running-PnL summary (total + per-position unrealized PnL) to all allowlisted users while positions are open, and add a total uPnL line to `/account`.

**Architecture:** A spawned `run_pnl_monitor` task polls `positions()` + `equity()` every `pnl_push_secs` (0 disables) and sends `render_pnl_summary` when not flat. The summary builds from `exchange.positions()`, so pre-existing positions of any origin are included automatically.

**Tech Stack:** Rust, tokio, teloxide, rusqlite.

## Global Constraints

- Descriptive names; verbs for functions, nouns for values.
- TDD: pure formatting/validation logic unit-tested first.
- Monitor never panics: poll/send errors logged via `tracing` and swallowed.
- Notifications target every `config.allowed_user_ids` as `teloxide::types::ChatId(user_id)`.
- `pnl_push_secs == 0` is a valid, disabling value (not an error); skip when flat (no spam).
- Implement AFTER the trigger-entry plan (independent; shares `Settings`/`monitor.rs` patterns).

---

### Task 1: Config + Settings — `pnl_push_secs`

**Files:**
- Modify: `src/config.rs`, `src/settings.rs`, `src/telegram.rs` (`render_settings` + `Settings { .. }` test literals)

**Interfaces:**
- Produces: `Config.pnl_push_secs: u64` (default `900`); `Settings.pnl_push_secs: u64`; `/set` key `pnl_push_secs` (any `u64`; `0` disables).

- [ ] **Step 1: Write failing settings tests** — add to `src/settings.rs` `mod tests`, update `sample()`:

```rust
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
```

Add `pnl_push_secs: 900,` to `sample()`.

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib settings:: 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 3: Implement**

`src/config.rs` — add to `struct Config`:

```rust
    /// Seconds between background P&L push updates while positions are open.
    /// Set via `PNL_PUSH_SECS` (default 900 = 15min; 0 disables).
    pub pnl_push_secs: u64,
```

`from_env()`: `pnl_push_secs: parse_env_or("PNL_PUSH_SECS", 900_u64)?,`
Add `pnl_push_secs: 900,` to the `allowlist_membership_check` test `Config { .. }`.

`src/settings.rs` — add field to `struct Settings`:

```rust
    /// Seconds between background P&L push updates (0 disables).
    pub pnl_push_secs: u64,
```

`Settings::from_config`: `pnl_push_secs: config.pnl_push_secs,`

`apply_setting` arm (before `_ =>`) — note: `0` is allowed, so parse without a lower bound:

```rust
        "pnl_push_secs" => {
            next.pnl_push_secs = value.parse::<u64>().map_err(|_| format!("'{value}' is not a whole number"))?;
        }
```

Extend `VALID_KEYS` by appending `, pnl_push_secs`.

`persist`: `self.put("pnl_push_secs", &settings.pnl_push_secs.to_string())?;`

`load` (mirror the timeout block):

```rust
        if let Some(raw) = self.get("pnl_push_secs")? {
            match raw.parse() {
                Ok(value) => resolved.pnl_push_secs = value,
                Err(_) => tracing::warn!(key = "pnl_push_secs", value = %raw, "failed to parse stored setting; keeping seed value"),
            }
        }
```

`src/telegram.rs` `render_settings` — add a line after `trigger_expiry_secs` (or after
`entry_fill_timeout_secs` if the trigger plan was not applied):

```rust
         pnl_push_secs: {}s\n\n\
```

and append `settings.pnl_push_secs,` as the final format arg. Add `pnl_push_secs: 900,` to
every `Settings { .. }` literal in `src/telegram.rs` tests.

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test --lib 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/settings.rs src/telegram.rs
git commit -m "feat(config): add pnl_push_secs setting"
```

---

### Task 2: `render_pnl_summary` + total line in `/account`

**Files:**
- Modify: `src/telegram.rs` (`render_pnl_summary`, `render_account`)

**Interfaces:**
- Consumes: `crate::hyperliquid::OpenPosition` (fields `coin, direction, size, entry_px, mark_px, unrealized_pnl, leverage`).
- Produces: `pub fn render_pnl_summary(equity: f64, positions: &[OpenPosition]) -> String`; `render_account` shows a `Total uPnL` line when positions exist.

- [ ] **Step 1: Write the failing tests** — add to `src/telegram.rs` `mod tests`:

```rust
fn two_positions() -> Vec<OpenPosition> {
    vec![
        OpenPosition { coin: "SOL".into(), direction: "long".into(), size: 0.5, entry_px: 68.5, mark_px: 69.1, unrealized_pnl: 0.30, leverage: 3.0, notional: 34.5 },
        OpenPosition { coin: "ETH".into(), direction: "short".into(), size: 0.1, entry_px: 2000.0, mark_px: 1990.0, unrealized_pnl: 1.00, leverage: 2.0, notional: 199.0 },
    ]
}

#[test]
fn render_pnl_summary_totals_unrealized() {
    let text = super::render_pnl_summary(107.44, &two_positions());
    assert!(text.contains("Total uPnL: $+1.30")); // 0.30 + 1.00
    assert!(text.contains("SOL"));
    assert!(text.contains("ETH"));
}

#[test]
fn account_card_shows_total_upnl_when_positions_present() {
    let text = super::render_account(107.44, &two_positions(), 0.0, Some(10.0));
    assert!(text.contains("Total uPnL: $+1.30"));
}
```

(If a `sample_positions()` helper already exists in the test module, reuse it instead of
`two_positions()` and adjust the expected total accordingly.)

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test --lib telegram::tests::render_pnl_summary_totals_unrealized 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add a shared per-position line helper + the summary, and a total helper used by both:

```rust
/// Sum of unrealized PnL across positions.
fn total_unrealized_pnl(positions: &[OpenPosition]) -> f64 {
    positions.iter().map(|position| position.unrealized_pnl).sum()
}

/// One position row, shared by /account and the P&L push.
fn position_line(position: &OpenPosition) -> String {
    format!(
        "  {:<6} {:<5} {} @ ${:.2}  mark ${:.2}  uPnL ${:+.2}  {:.0}x\n",
        position.coin,
        position.direction.to_uppercase(),
        position.size,
        position.entry_px,
        position.mark_px,
        position.unrealized_pnl,
        position.leverage,
    )
}

/// Running-P&L push: total + per-position unrealized PnL.
pub fn render_pnl_summary(equity: f64, positions: &[OpenPosition]) -> String {
    let mut out = String::from("📊 Running P&L\n");
    out.push_str(&format!("Equity: ${:.2}\n", equity));
    out.push_str(&format!("Total uPnL: ${:+.2}\n", total_unrealized_pnl(positions)));
    for position in positions {
        out.push_str(&position_line(position));
    }
    out
}
```

Refactor `render_account` to use `position_line` for each row and to print
`Total uPnL: $±X.XX` right after the `Open positions (N):` header line:

```rust
    out.push_str(&format!("\nOpen positions ({}):\n", positions.len()));
    out.push_str(&format!("Total uPnL: ${:+.2}\n", total_unrealized_pnl(positions)));
    for position in positions {
        out.push_str(&position_line(position));
    }
    out
```

- [ ] **Step 4: Run — expect PASS (incl. existing account tests)**

Run: `cargo test --lib telegram:: 2>&1 | tail -15`
Expected: PASS — new tests plus the existing `account_card_*` tests.

- [ ] **Step 5: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): add render_pnl_summary and /account total uPnL"
```

---

### Task 3: `run_pnl_monitor` loop + spawn

**Files:**
- Modify: `src/monitor.rs` (loop), `src/telegram.rs` (spawn in `run`)

**Interfaces:**
- Consumes: `render_pnl_summary` (Task 2); `Exchange::{positions, equity}`; `Settings.pnl_push_secs` (Task 1).
- Produces: `pub async fn run_pnl_monitor<E: Exchange + 'static>(bot: Bot, exchange: Arc<E>, settings: Arc<std::sync::Mutex<crate::settings::Settings>>, allowed_user_ids: Vec<i64>)`

- [ ] **Step 1: Implement the loop** — add to `src/monitor.rs`:

```rust
use crate::settings::Settings;
use std::sync::Mutex;

/// Periodically pushes a running-P&L summary while positions are open. Reads
/// `pnl_push_secs` from live settings each tick (0 disables → idle). Never panics.
pub async fn run_pnl_monitor<E: Exchange + 'static>(
    bot: Bot,
    exchange: Arc<E>,
    settings: Arc<Mutex<Settings>>,
    allowed_user_ids: Vec<i64>,
) {
    loop {
        let push_secs = settings.lock().unwrap().pnl_push_secs;
        if push_secs == 0 {
            // Disabled: idle at a fixed cadence so re-enabling via /set takes effect.
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        tokio::time::sleep(Duration::from_secs(push_secs)).await;

        let positions = match exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => { tracing::warn!("pnl monitor positions() failed: {error}"); continue; }
        };
        if positions.is_empty() { continue; }

        let equity = exchange.equity().await.unwrap_or(0.0);
        let message = crate::telegram::render_pnl_summary(equity, &positions);
        for user_id in &allowed_user_ids {
            if let Err(error) = bot.send_message(ChatId(*user_id), &message).await {
                tracing::warn!("pnl push failed for {user_id}: {error}");
            }
        }
    }
}
```

(`render_pnl_summary` is `pub` in `telegram.rs`; reference it via `crate::telegram::`.)

- [ ] **Step 2: Spawn in `telegram::run`** — after the other monitor spawns:

```rust
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let monitor_settings = context.settings.clone();
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        tokio::spawn(async move {
            crate::monitor::run_pnl_monitor(
                monitor_bot, monitor_exchange, monitor_settings, monitor_user_ids,
            ).await;
        });
    }
```

- [ ] **Step 3: Build + full suite**

Run: `cargo build 2>&1 | tail -15` (clean) then `cargo test 2>&1 | tail -10` (green).

- [ ] **Step 4: Commit**

```bash
git add src/monitor.rs src/telegram.rs
git commit -m "feat(monitor): push periodic running-P&L summary"
```

---

### Task 4: Docs

**Files:**
- Modify: `.env.example`, `README.md`

- [ ] **Step 1: `.env.example`** — after `MONITOR_POLL_SECS=30` (and `TRIGGER_EXPIRY_SECS` if present):

```
# Seconds between background running-P&L push updates while positions are open
# (default 900 = 15min; 0 disables).
PNL_PUSH_SECS=900
```

- [ ] **Step 2: `README.md` `## Usage`** — append:

```markdown
While any position is open the bot pushes a running-P&L summary (total + per-position
unrealized PnL) every `PNL_PUSH_SECS` (default 15min; `0` disables; editable via
`/set pnl_push_secs`). `/account` also shows a total uPnL line.
```

- [ ] **Step 3: Commit**

```bash
git add .env.example README.md
git commit -m "docs: document PNL_PUSH_SECS and running-P&L push"
```

---

## Self-Review Notes

- **Spec coverage:** `render_pnl_summary` + `/account` total → Task 2. `run_pnl_monitor` (skip-when-flat, disabled-when-zero, all users) → Task 3. Setting `pnl_push_secs` → Task 1. Docs → Task 4.
- **Pre-existing positions:** built from `exchange.positions()` (all open positions) — no special handling needed; noted in spec.
- **Type consistency:** `render_pnl_summary(equity, positions)` and `run_pnl_monitor` signatures match across Tasks 2–3. `OpenPosition` field names match `src/hyperliquid/mod.rs`.
- **Disabled cadence:** when `pnl_push_secs == 0` the loop idles at 60s so re-enabling via `/set` is picked up without a restart.
