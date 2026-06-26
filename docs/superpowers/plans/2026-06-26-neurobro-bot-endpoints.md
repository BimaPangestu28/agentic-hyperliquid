# Neurobro Auto-Scalping — Bot Subsystem Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Rust-side pieces the Neurobro scraper depends on: a coin watchlist + safety flags in settings, a `GET /watchlist` read endpoint, and a `POST /execute` endpoint that auto-opens a position (size + market entry + SL/TP bracket) and sends a Telegram notification — no confirmation step.

**Architecture:** Extend `Settings` with `watchlist`, `auto_scalp_enabled`, `max_open_positions` (persisted in the existing SQLite `SettingsStore`). Add `/watch` Telegram commands. Extend the axum `ApiState` so the API can read current settings from the DB and send Telegram notifications, then add two endpoints. `POST /execute` reuses the EXISTING `build_plan` (sizing) and `execute_plan` (entry+bracket) — no new execution logic.

**Tech Stack:** Rust, `axum` (HTTP API), `teloxide` (Telegram), `rusqlite` (settings store), `tokio`, `anyhow`, `serde`.

## Global Constraints

- Auto-execution uses the **Aggressive** risk profile (leverage = `settings.leverage.aggressive`, currently 20x). Market entry, reduce-only SL + a single TP.
- `POST /execute` is gated three ways: `auto_scalp_enabled == true`, `confidence >= 7`, open positions `< max_open_positions`. A failed gate returns a 4xx and does NOT trade.
- `auto_scalp_enabled` defaults to **false** (master kill-switch off until the user turns it on).
- Reuse existing `build_plan` (`src/sizing.rs:140`) and `execute_plan` (`src/telegram.rs:534`) — do NOT write new sizing/order/bracket logic.
- The API reads CURRENT settings from SQLite per call (`SettingsStore::open(db_path)?.load(seed)`) — it does not share the live settings mutex.
- User-facing Telegram copy is Indonesian; notifications are plain text (no parse_mode).
- All new `Settings` fields must be handled in `from_config`, `persist`, and `load` (key/value rows), mirroring the existing fields.
- Run `cargo test` after each task; tests are colocated `#[cfg(test)]` modules. Settings tests use `SettingsStore::open_in_memory()`.

## Confirmed types (from the codebase)

```rust
// src/parser.rs
pub enum Direction { Long, Short }
pub struct TakeProfit { pub price: f64, pub allocation_pct: f64 }
pub struct TradeSetup { pub coin: String, pub direction: Direction, pub timeframe: Option<String>,
    pub risk_reward: Option<f64>, pub confidence: Option<u8>, pub entry: f64, pub stop_loss: f64,
    pub take_profits: Vec<TakeProfit> }
// src/sizing.rs
pub fn build_plan(input: &SizingInput) -> Result<ExecutionPlan, SizingError>;
pub struct SizingInput<'a> { pub setup: &'a TradeSetup, pub equity: f64, pub risk_pct: f64,
    pub entry_mode: EntryMode, pub entry_pct: f64, pub entry_fixed_usd: f64,
    pub profile: RiskProfile, pub leverage: &'a LeverageMap, pub asset_meta: &'a AssetMeta }
// src/telegram.rs
pub async fn execute_plan<E: Exchange>(exchange: &E, plan: &ExecutionPlan, use_limit: bool,
    fill_timeout_secs: u64, cancel: Arc<tokio::sync::Notify>, reporter: &dyn ProgressReporter) -> anyhow::Result<()>;
// src/api/mod.rs
pub struct ApiState { pub exchange: Arc<dyn Exchange>, pub db_path: String, pub token: String }
```

---

### Task 1: Settings — watchlist + safety flags + persistence

**Files:**
- Modify: `src/settings.rs` — `Settings` struct (`~line 14`), `from_config` (`~line 31`), `VALID_KEYS` (`~line 47`), `apply_setting` (`~line 92`), `persist` (`~line 174`), `load` (`~line 196`), tests (`~line 330+`).

**Interfaces:**
- Produces on `Settings`: `pub watchlist: Vec<String>`, `pub auto_scalp_enabled: bool`, `pub max_open_positions: u32`.
- Produces a shared test seed: `#[cfg(test)] pub fn sample() -> Settings` at module scope (promoted from the existing private `sample()` in `mod tests`), reused by the telegram and api test modules.

> NOTE on the existing `sample()`: `src/settings.rs` currently has a private `fn sample() -> Settings` inside `#[cfg(test)] mod tests` (line ~277) that builds a `Settings` struct literal. Adding fields to `Settings` will break this literal (missing fields). This task MUST update it AND promote it so other modules' tests can reuse one seed (DRY).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/settings.rs` (they call `sample()`, available via `use super::*`):

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib settings::tests::apply_setting_toggles_auto_scalp_and_cap settings::tests::persist_load_roundtrips_watchlist_and_flags`
Expected: FAIL — no field `auto_scalp_enabled` / `watchlist` / `max_open_positions`.

- [ ] **Step 3: Add the fields to `Settings` and `from_config`**

In `struct Settings` (after `pnl_push_secs`, line ~26):

```rust
    /// Coins the auto-scalp scraper scans, upper-cased.
    pub watchlist: Vec<String>,
    /// Master kill-switch for auto-scalp execution (default false).
    pub auto_scalp_enabled: bool,
    /// Max concurrent open positions the auto-scalp loop may hold.
    pub max_open_positions: u32,
```

In `from_config` (after `pnl_push_secs: config.pnl_push_secs,`, line ~41):

```rust
            watchlist: Vec::new(),
            auto_scalp_enabled: false,
            max_open_positions: 5,
```

- [ ] **Step 4: Add `apply_setting` keys + extend `VALID_KEYS`**

Append to `VALID_KEYS` (line ~47): `, auto_scalp_enabled, max_open_positions`.

In `apply_setting`'s `match key` (before the catch-all error arm):

```rust
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
```

> `watchlist` is NOT settable via `/set` (it is mutated by `/watch` in Task 2), so it gets no `apply_setting` arm.

- [ ] **Step 5: Persist + load the new fields**

In `persist` (before `Ok(())`, line ~189):

```rust
        self.put("watchlist", &settings.watchlist.join(","))?;
        self.put("auto_scalp_enabled", &settings.auto_scalp_enabled.to_string())?;
        self.put("max_open_positions", &settings.max_open_positions.to_string())?;
```

In `load` (before the final `Ok(resolved)` — find where `pnl_push_secs` is loaded and add after it):

```rust
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
```

- [ ] **Step 5b: Promote + update the `sample()` test helper**

Move the existing `fn sample() -> Settings` OUT of `#[cfg(test)] mod tests` to module scope as `#[cfg(test)] pub fn sample() -> Settings`, and add the three new fields so it compiles and is reusable by other modules' tests:

```rust
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
    }
}
```

Remove the old private `fn sample()` from `mod tests` (the `use super::*;` there picks up the promoted one). Add `use crate::config::LeverageMap;` / `EntryMode` imports at module scope if the promoted fn needs them (they are already imported at the top of the file).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib settings::`
Expected: PASS (all settings tests, including the 2 new).

- [ ] **Step 7: Commit**

```bash
git add src/settings.rs
git commit -m "feat(settings): watchlist + auto_scalp_enabled + max_open_positions"
```

---

### Task 2: `/watch` Telegram commands

**Files:**
- Modify: `src/telegram.rs` — pure helpers near `render_settings` (`~line 279`), command handler in `on_message` (after the `/settings` block), tests (`~line 1490+`).

**Interfaces:**
- Consumes: `Settings.watchlist` (Task 1); `context.settings` (`Arc<Mutex<Settings>>`), `context.settings_store` (persist), `first_command_word` (`src/telegram.rs:534` area).
- Produces: `fn add_coin(list: &[String], coin: &str) -> Vec<String>`, `fn remove_coin(list: &[String], coin: &str) -> Vec<String>`, `fn render_watch(settings: &Settings) -> String`.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn add_coin_normalizes_and_dedupes() {
        let list = vec!["BTC".to_string()];
        assert_eq!(super::add_coin(&list, "eth"), vec!["BTC".to_string(), "ETH".to_string()]);
        assert_eq!(super::add_coin(&list, "btc"), vec!["BTC".to_string()]); // dedupe, case-insensitive
    }

    #[test]
    fn remove_coin_is_case_insensitive() {
        let list = vec!["BTC".to_string(), "ETH".to_string()];
        assert_eq!(super::remove_coin(&list, "btc"), vec!["ETH".to_string()]);
        assert_eq!(super::remove_coin(&list, "sol"), list); // absent → unchanged
    }

    #[test]
    fn render_watch_shows_coins_and_switch() {
        let mut s = crate::settings::sample();
        s.watchlist = vec!["BTC".into(), "ETH".into()];
        s.auto_scalp_enabled = true;
        let text = super::render_watch(&s);
        assert!(text.contains("BTC") && text.contains("ETH"));
        assert!(text.to_lowercase().contains("auto"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib add_coin_normalizes remove_coin_is_case render_watch_shows`
Expected: FAIL — unknown `add_coin` / `remove_coin` / `render_watch`.

- [ ] **Step 3: Implement the pure helpers**

Near `render_settings` (`~line 279`):

```rust
/// Returns a new list with `coin` (upper-cased) appended if absent.
pub fn add_coin(list: &[String], coin: &str) -> Vec<String> {
    let coin = coin.trim().to_uppercase();
    let mut next = list.to_vec();
    if !coin.is_empty() && !next.iter().any(|c| c.eq_ignore_ascii_case(&coin)) {
        next.push(coin);
    }
    next
}

/// Returns a new list with `coin` removed (case-insensitive).
pub fn remove_coin(list: &[String], coin: &str) -> Vec<String> {
    let coin = coin.trim();
    list.iter().filter(|c| !c.eq_ignore_ascii_case(coin)).cloned().collect()
}

/// Renders the watchlist + auto-scalp status for the `/watch` command.
pub fn render_watch(settings: &Settings) -> String {
    let coins = if settings.watchlist.is_empty() {
        "(kosong)".to_string()
    } else {
        settings.watchlist.join(", ")
    };
    let status = if settings.auto_scalp_enabled { "ON 🟢" } else { "OFF 🔴" };
    format!(
        "👁️ Watchlist auto-scalp\n\nCoin: {coins}\nAuto-scalp: {status}\nMax posisi: {}\n\n\
         /watch add <COIN>  ·  /watch remove <COIN>\n\
         /set auto_scalp_enabled on|off  ·  /set max_open_positions <n>",
        settings.max_open_positions
    )
}
```

- [ ] **Step 4: Add the `/watch` command handler in `on_message`**

After the `/settings` handler block (search `first_command_word(text) == "/settings"`):

```rust
    // /watch [add|remove <COIN>] — manage the auto-scalp watchlist.
    if first_command_word(text) == "/watch" {
        let mut parts = text.split_whitespace();
        let _cmd = parts.next();
        let action = parts.next().map(|a| a.to_lowercase());
        let coin = parts.next();
        match (action.as_deref(), coin) {
            (Some("add"), Some(coin)) => {
                let next = {
                    let mut guard = context.settings.lock().unwrap();
                    guard.watchlist = add_coin(&guard.watchlist, coin);
                    guard.clone()
                };
                if let Err(error) = context.settings_store.persist(&next) {
                    tracing::warn!(%error, "failed to persist watchlist add");
                }
                bot.send_message(message.chat.id, render_watch(&next)).await?;
            }
            (Some("remove"), Some(coin)) => {
                let next = {
                    let mut guard = context.settings.lock().unwrap();
                    guard.watchlist = remove_coin(&guard.watchlist, coin);
                    guard.clone()
                };
                if let Err(error) = context.settings_store.persist(&next) {
                    tracing::warn!(%error, "failed to persist watchlist remove");
                }
                bot.send_message(message.chat.id, render_watch(&next)).await?;
            }
            (None, _) => {
                let settings = context.settings.lock().unwrap().clone();
                bot.send_message(message.chat.id, render_watch(&settings)).await?;
            }
            _ => {
                bot.send_message(message.chat.id, "Pakai: /watch  ·  /watch add <COIN>  ·  /watch remove <COIN>").await?;
            }
        }
        return Ok(());
    }
```

- [ ] **Step 5: Run tests + build**

Run: `cargo test --lib add_coin remove_coin render_watch && cargo build`
Expected: 3 tests PASS; build clean.

- [ ] **Step 6: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): /watch commands to manage the auto-scalp watchlist"
```

---

### Task 3: Extend `ApiState` + wire it in `main.rs`

**Files:**
- Modify: `src/api/mod.rs` — `ApiState` struct (`~line 19`), the `state_with` test helper (`~line 203`).
- Modify: `src/main.rs` — `ApiState { .. }` construction (`~line 31`).

**Interfaces:**
- Produces on `ApiState`: `pub settings_seed: Settings`, `pub telegram_bot_token: String`, `pub allowed_user_ids: Vec<i64>` (in addition to the existing `exchange`, `db_path`, `token`).

- [ ] **Step 1: Add the fields to `ApiState`**

In `src/api/mod.rs` (`struct ApiState`, line ~19):

```rust
pub struct ApiState {
    pub exchange: Arc<dyn Exchange>,
    pub db_path: String,
    pub token: String,
    /// Default settings used to seed `SettingsStore::load` when reading current
    /// settings from the DB per request.
    pub settings_seed: crate::settings::Settings,
    /// Telegram bot token used to construct a `Bot` for auto-open notifications.
    pub telegram_bot_token: String,
    /// Chat ids that receive auto-open notifications.
    pub allowed_user_ids: Vec<i64>,
}
```

> Add `use crate::settings::Settings;` if not already imported.

- [ ] **Step 2: Update the `state_with` test helper**

In the API test module (`~line 203`), extend the `ApiState { .. }` it builds with the three new fields:

```rust
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
```

- [ ] **Step 3: Wire the fields in `main.rs`**

In `src/main.rs`, the `api::ApiState { .. }` block (`~line 31`):

```rust
        let api_state = api::ApiState {
            exchange: api_exchange,
            db_path: config.journal_path.clone(),
            token: api_token,
            settings_seed: settings::Settings::from_config(&config),
            telegram_bot_token: config.telegram_token.clone(),
            allowed_user_ids: config.allowed_user_ids.clone(),
        };
```

> The config field is `config.telegram_token` (confirmed, `src/config.rs:20`) — the ApiState field is named `telegram_bot_token` but is sourced from `config.telegram_token`. `mod settings;` is already declared in `main.rs`.

- [ ] **Step 4: Verify build + existing API tests pass**

Run: `cargo build && cargo test --lib api::`
Expected: build clean; existing API tests pass with the extended state.

- [ ] **Step 5: Commit**

```bash
git add src/api/mod.rs src/main.rs
git commit -m "feat(api): extend ApiState with settings seed, bot token, allowed users"
```

---

### Task 4: `GET /watchlist` endpoint

**Files:**
- Modify: `src/api/mod.rs` — route (`~line 45`), handler, tests.

**Interfaces:**
- Consumes: `ApiState` (Task 3), `SettingsStore::open(&db_path)?.load(seed)` (`src/settings.rs:146,196`).
- Produces: `GET /watchlist` → `{ "coins": [...], "auto_scalp_enabled": bool, "max_open_positions": u32 }`.

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn watchlist_requires_auth_and_returns_settings() {
        let app = router(state_with(1000.0));
        // no bearer → 401
        let res = app.clone().oneshot(
            Request::builder().uri("/watchlist").body(axum::body::Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // with bearer → 200 + JSON
        let res = app.oneshot(
            Request::builder().uri("/watchlist").header("authorization", "Bearer test-token")
                .body(axum::body::Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
```

> Confirm the exact bearer the `state_with` helper sets as `token` and match it in the header (the existing `balance_requires_auth` test shows the pattern).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib watchlist_requires_auth_and_returns_settings`
Expected: FAIL — no route `/watchlist` (404 instead of 200).

- [ ] **Step 3: Add the response type, handler, and route**

Add near the other response structs:

```rust
#[derive(serde::Serialize)]
struct WatchlistResponse {
    coins: Vec<String>,
    auto_scalp_enabled: bool,
    max_open_positions: u32,
}

async fn watchlist(State(state): State<ApiState>) -> Result<Json<WatchlistResponse>, StatusCode> {
    let store = crate::settings::SettingsStore::open(&state.db_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let settings = store.load(state.settings_seed.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(WatchlistResponse {
        coins: settings.watchlist,
        auto_scalp_enabled: settings.auto_scalp_enabled,
        max_open_positions: settings.max_open_positions,
    }))
}
```

In `router`, add after the `/positions` route:

```rust
        .route("/watchlist", get(watchlist))
```

- [ ] **Step 4: Run test + build**

Run: `cargo test --lib watchlist_requires_auth_and_returns_settings && cargo build`
Expected: PASS; build clean.

- [ ] **Step 5: Commit**

```bash
git add src/api/mod.rs
git commit -m "feat(api): GET /watchlist endpoint"
```

---

### Task 5: `POST /execute` — gated auto-open + notification

**Files:**
- Modify: `src/telegram.rs` — relax `execute_plan` to `<E: Exchange + ?Sized>` (`~line 534`); add a public no-op reporter.
- Modify: `src/api/mod.rs` — request type, handler, route, tests.

**Interfaces:**
- Consumes: `build_plan` (`src/sizing.rs:140`), `execute_plan` (`src/telegram.rs:534`), `ApiState` (Task 3); `TradeSetup`/`TakeProfit`/`Direction` (`src/parser.rs`).
- Produces: `POST /execute` (bearer) accepting `ExecuteRequest`, returning `200 {ok:true, order_id?}` or a gated 4xx.

- [ ] **Step 1: Relax `execute_plan` to accept `&dyn Exchange` and add a no-op reporter**

In `src/telegram.rs`, change the signature (line 534) from `pub async fn execute_plan<E: Exchange>` to:

```rust
pub async fn execute_plan<E: Exchange + ?Sized>(
```

Add a public no-op reporter near `ProgressReporter` (search the trait definition):

```rust
/// A `ProgressReporter` that drops every event — used by the API auto-execute
/// path, which reports via a Telegram notification instead of progress events.
pub struct NoopReporter;

#[async_trait::async_trait]
impl ProgressReporter for NoopReporter {
    async fn report(&self, _event: ExecutionEvent) {}
}
```

> Match the exact `ProgressReporter` trait signature (it is `async fn report(&self, event: ExecutionEvent)` — confirm the method name/shape and mirror it).

- [ ] **Step 2: Write the failing tests**

In the API test module:

```rust
    fn execute_body(confidence: u8) -> String {
        format!(r#"{{"coin":"AVAX","direction":"long","entry":6.12,"stop_loss":5.99,"take_profit":6.32,"confidence":{confidence},"thesis":"t"}}"#)
    }

    #[tokio::test]
    async fn execute_rejected_when_auto_scalp_disabled() {
        // state_with leaves auto_scalp_enabled at the seed default (false)
        let app = router(state_with(1000.0));
        let res = app.oneshot(Request::builder().method("POST").uri("/execute")
            .header("authorization", "Bearer test-token").header("content-type","application/json")
            .body(axum::body::Body::from(execute_body(8))).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT); // 409 kill-switch
    }

    #[tokio::test]
    async fn execute_rejected_when_confidence_below_gate() {
        let mut state = state_with(1000.0);
        state.settings_seed.auto_scalp_enabled = true;
        let app = router(state);
        let res = app.oneshot(Request::builder().method("POST").uri("/execute")
            .header("authorization", "Bearer test-token").header("content-type","application/json")
            .body(axum::body::Body::from(execute_body(6))).unwrap()).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY); // 422 conf<7
    }
```

> These two gate tests need no live exchange/Telegram (they fail before execution). A success-path test is NOT included here because it would place orders + send Telegram; the success path is covered by the existing `execute_plan` tests against `MockExchange` and validated end-to-end during scraper integration. Note this explicitly in your report.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib execute_rejected_when_auto_scalp_disabled execute_rejected_when_confidence_below_gate`
Expected: FAIL — no route `/execute` (404).

- [ ] **Step 4: Implement the request type, handler, and route**

In `src/api/mod.rs`:

```rust
#[derive(serde::Deserialize)]
struct ExecuteRequest {
    coin: String,
    direction: String, // "long" | "short"
    entry: f64,
    stop_loss: f64,
    take_profit: f64,
    confidence: u8,
    #[serde(default)]
    thesis: String,
}

async fn execute(
    State(state): State<ApiState>,
    Json(req): Json<ExecuteRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Load current settings from the DB.
    let store = crate::settings::SettingsStore::open(&state.db_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let settings = store.load(state.settings_seed.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Gate 1: master kill-switch.
    if !settings.auto_scalp_enabled { return Err(StatusCode::CONFLICT); }
    // Gate 2: confidence.
    if req.confidence < 7 { return Err(StatusCode::UNPROCESSABLE_ENTITY); }
    // Gate 3: position cap.
    let open = state.exchange.positions().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if open.len() as u32 >= settings.max_open_positions { return Err(StatusCode::CONFLICT); }

    // Build the TradeSetup (single TP, 100%).
    let direction = match req.direction.to_lowercase().as_str() {
        "long" => crate::parser::Direction::Long,
        "short" => crate::parser::Direction::Short,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let setup = crate::parser::TradeSetup {
        coin: req.coin.to_uppercase(),
        direction,
        timeframe: Some("scalp".to_string()),
        risk_reward: None,
        confidence: Some(req.confidence),
        entry: req.entry,
        stop_loss: req.stop_loss,
        take_profits: vec![crate::parser::TakeProfit { price: req.take_profit, allocation_pct: 100.0 }],
    };

    // Size with the Aggressive profile (20x), market entry.
    let equity = state.exchange.equity().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let asset_meta = state.exchange.asset_meta(&setup.coin).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let plan = crate::sizing::build_plan(&crate::sizing::SizingInput {
        setup: &setup,
        equity,
        risk_pct: settings.risk_pct,
        entry_mode: settings.entry_mode,
        entry_pct: settings.entry_pct,
        entry_fixed_usd: settings.entry_fixed_usd,
        profile: crate::sizing::RiskProfile::Aggressive,
        leverage: &settings.leverage,
        asset_meta: &asset_meta,
    }).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;

    // Execute: market entry + SL/TP bracket, no confirm.
    crate::telegram::execute_plan(
        state.exchange.as_ref(), &plan, false, settings.entry_fill_timeout_secs,
        std::sync::Arc::new(tokio::sync::Notify::new()), &crate::telegram::NoopReporter,
    ).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Notify (best-effort).
    let bot = teloxide::Bot::new(&state.telegram_bot_token);
    let msg = format!(
        "🤖 Auto-buka {} {} {} @ {} · SL {} · TP {} · conf {}/10",
        setup.coin,
        if matches!(setup.direction, crate::parser::Direction::Long) { "LONG" } else { "SHORT" },
        plan.size, plan.entry, plan.stop_loss.price, req.take_profit, req.confidence
    );
    for user_id in &state.allowed_user_ids {
        use teloxide::prelude::Requester;
        let _ = bot.send_message(teloxide::types::ChatId(*user_id), &msg).await;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}
```

In `router`, add:

```rust
        .route("/execute", axum::routing::post(execute))
```

> `RiskProfile::Aggressive` and `Direction` paths: confirm exact enum paths (`crate::sizing::RiskProfile`, `crate::parser::Direction`). `teloxide` is already a dependency. Add `use axum::routing::post;` to the import block if you prefer the shorter form.

- [ ] **Step 5: Run tests + build**

Run: `cargo test --lib execute_rejected && cargo build`
Expected: 2 gate tests PASS; build clean.

- [ ] **Step 6: Run the full suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/telegram.rs src/api/mod.rs
git commit -m "feat(api): POST /execute — gated auto-open with Telegram notification"
```

---

## Self-Review Notes

- **Spec coverage:** watchlist in settings + add/remove (Task 1-2); `auto_scalp_enabled` kill-switch + `max_open_positions` cap (Task 1, enforced Task 5); `GET /watchlist` (Task 4); `POST /execute` gated (conf≥7, kill-switch, cap) + size/order/bracket via existing `build_plan`/`execute_plan` + Telegram notif (Task 5); Aggressive/20x profile (Task 5). All bot-side spec items mapped. The scraper itself is a separate plan (built next, once this is live and the Neurobro DOM is available).
- **Reuse/DRY:** no new sizing/execution logic — `build_plan` and `execute_plan` are reused; `execute_plan` is only relaxed to `?Sized` so the `Arc<dyn Exchange>` API state can call it.
- **Safety:** `auto_scalp_enabled` defaults false; three independent gates in `/execute`; fail-closed on bad direction/asset.
- **Type consistency:** `ExecuteRequest` fields ↔ `TradeSetup`/`TakeProfit` mapping; `WatchlistResponse` ↔ `/watch` settings; `add_coin`/`remove_coin`/`render_watch` consistent across Task 2.
- **Deferred:** success-path `/execute` test (places real orders + Telegram) is intentionally not unit-tested; covered by existing `execute_plan` mock tests + scraper integration. Flagged in Task 5.
```
