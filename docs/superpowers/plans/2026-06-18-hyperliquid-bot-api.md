# Hyperliquid Bot Read-Only API Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a small read-only HTTP API to `agent-hyperliquid` exposing account balance, open positions, closed trades (with strategy metadata), and USDC deposit/withdrawal flows, so `portfolio-tracker` can pull this data instead of reading Hyperliquid on-chain.

**Architecture:** An `axum` HTTP server is spawned alongside the existing `teloxide` dispatcher in `main.rs`. All routes sit behind a constant-time bearer-token check. Handlers read existing state only — the SDK `Exchange` (equity, positions, fills, ledger) and the local `Journal` (strategy metadata). No trading logic changes. Fill→round-trip-trade assembly is a pure, unit-tested function; positions come from a new `Exchange` trait method over `user_state.asset_positions`.

**Tech Stack:** Rust, `axum` 0.7 + `tower-http`, `tokio`, `serde`/`serde_json`, `async-trait`, existing `hyperliquid_rust_sdk` + `rusqlite` journal.

## Global Constraints

- The HTTP API is **read-only**. No handler may place/cancel orders or mutate the journal.
- All routes require `Authorization: Bearer <token>` matched against `PORTFOLIO_API_TOKEN` in **constant time**; missing/invalid → `401`.
- Money values serialize as JSON numbers in USD; timestamps as epoch milliseconds (matching the SDK's `time_ms`) unless noted.
- The existing `Fill` struct and everything consuming it (`stats.rs`, `process_signal`) must remain unchanged — richer fill data uses a **new** type.
- `Journal` uses `rusqlite` (`Connection` is `!Sync`): HTTP handlers open a fresh `Journal::open(&db_path)` per request; never share one `Connection` across handlers.
- Bind address from `HTTP_BIND_ADDR` (default `127.0.0.1:8088`). Journal path is the same one `telegram::run` uses (currently `trades.db`); read it from config/env, do not hardcode in two places.
- Tests run from the repo root with `cargo test`. Pure functions are tested without network; handlers are tested with the in-memory mock `Exchange` and `Journal::open_in_memory()`.
- Run `cargo build` after wiring changes; do not run `cargo fmt`.

---

## Phase 1 — HTTP server skeleton + auth

### Task 1: Add deps and the `api` module with bearer-auth middleware

**Files:**
- Modify: `Cargo.toml` (add `axum`, `tower-http`)
- Create: `src/api/mod.rs`
- Create: `src/api/auth.rs`
- Modify: `src/main.rs` (add `mod api;`)

**Interfaces:**
- Produces: `api::auth::token_matches(provided: &str, expected: &str) -> bool` (constant-time); `api::ApiState { exchange: Arc<dyn Exchange>, db_path: String, token: String }`; `api::router(state: ApiState) -> axum::Router`.

- [ ] **Step 1: Add dependencies** — in `Cargo.toml` under `[dependencies]`, after the `serde_json` line:

```toml
axum = "0.7"
tower-http = { version = "0.6", features = ["trace"] }
```

- [ ] **Step 2: Write the failing auth test** — create `src/api/auth.rs`:

```rust
//! Constant-time bearer-token check for the read-only API.

/// True when `provided` equals `expected` without short-circuiting on the first
/// differing byte (avoids a timing side-channel on the shared secret).
pub fn token_matches(provided: &str, expected: &str) -> bool {
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_on_exact_equality() {
        assert!(token_matches("secret-123", "secret-123"));
        assert!(!token_matches("secret-123", "secret-124"));
        assert!(!token_matches("secret", "secret-123"));
        assert!(!token_matches("", "secret"));
    }
}
```

- [ ] **Step 3: Run the test to confirm it passes**

Run: `cargo test token_matches`
Expected: PASS (1 test).

- [ ] **Step 4: Create the module + router skeleton** — create `src/api/mod.rs`:

```rust
//! Read-only HTTP API consumed by portfolio-tracker. Trading logic lives
//! elsewhere; nothing here mutates exchange or journal state.

pub mod auth;

use crate::hyperliquid::Exchange;
use axum::{
    extract::State,
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct ApiState {
    pub exchange: Arc<dyn Exchange>,
    pub db_path: String,
    pub token: String,
}

/// Reject any request whose `Authorization: Bearer <token>` does not match the
/// configured secret.
async fn require_bearer(
    State(state): State<ApiState>,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let provided = header.strip_prefix("Bearer ").unwrap_or("");
    if auth::token_matches(provided, &state.token) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(state.clone(), require_bearer))
        .with_state(state)
}
```

Add `mod api;` to `src/main.rs` alongside the other `mod` lines.

- [ ] **Step 5: Verify build**

Run: `cargo build`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/api/mod.rs src/api/auth.rs src/main.rs
git commit -m "feat(api): read-only HTTP module with bearer auth"
```

---

### Task 2: `/balance` handler

**Files:**
- Modify: `src/api/mod.rs` (add the `balance` handler + route)

**Interfaces:**
- Consumes: `Exchange::equity()`, `ApiState`.
- Produces: `GET /balance -> { "equity_usd": f64, "as_of_ms": i64 }`.

- [ ] **Step 1: Write the failing test** — add a `tests` module at the bottom of `src/api/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::hyperliquid::testing::MockExchange;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // oneshot

    fn state_with(equity: f64) -> ApiState {
        ApiState {
            exchange: Arc::new(MockExchange::with_equity(equity)),
            db_path: ":memory:".into(),
            token: "t".into(),
        }
    }

    #[tokio::test]
    async fn balance_requires_auth() {
        let app = router(state_with(1000.0));
        let resp = app
            .oneshot(Request::builder().uri("/balance").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn balance_returns_equity() {
        let app = router(state_with(1234.5));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/balance")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["equity_usd"].as_f64().unwrap(), 1234.5);
    }
}
```

This requires a constructor on the existing mock. Add to `src/hyperliquid/mod.rs` inside the `#[cfg(test)]` mock module (or its `testing` submodule) a helper:

```rust
impl MockExchange {
    pub fn with_equity(equity: f64) -> Self {
        // Mirror the existing Default/new construction, setting `equity`.
        Self { equity, ..Self::new_for_test() }
    }
}
```

(Match the real mock's field set and existing constructor name; if the mock has no public constructor, add `with_equity` building it directly. The mock must be reachable as `crate::hyperliquid::testing::MockExchange` — if it is currently `#[cfg(test)]`-only and private, expose it under a `pub mod testing` gated on `#[cfg(any(test, feature = "test-util"))]` so the `api` tests can use it.)

- [ ] **Step 2: Add `tower` dev-dependency** — in `Cargo.toml` under `[dev-dependencies]`:

```toml
tower = { version = "0.5", features = ["util"] }
```

- [ ] **Step 3: Run the test to confirm it fails**

Run: `cargo test --lib api::tests`
Expected: FAIL — `balance` route/handler not defined.

- [ ] **Step 4: Implement the handler** — in `src/api/mod.rs`, add the handler and register it:

```rust
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
struct BalanceResponse {
    equity_usd: f64,
    as_of_ms: i64,
}

async fn balance(State(state): State<ApiState>) -> Result<Json<BalanceResponse>, StatusCode> {
    let equity = state
        .exchange
        .equity()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(BalanceResponse { equity_usd: equity, as_of_ms: now_ms() }))
}

/// Epoch milliseconds. Isolated so handlers stay deterministic under test if
/// later stubbed.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
```

Add the route inside `router`, before the `.layer(...)`:

```rust
        .route("/balance", get(balance))
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib api::tests`
Expected: PASS (auth reject + equity return).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/api/mod.rs src/hyperliquid/mod.rs
git commit -m "feat(api): GET /balance returns account equity"
```

---

## Phase 2 — Open positions

### Task 3: `Exchange::positions()` + `/positions`

**Files:**
- Modify: `src/hyperliquid/mod.rs` (add `OpenPosition` type, `positions()` trait method, real impl, mock impl)
- Modify: `src/api/mod.rs` (add the `positions` handler + route)

**Interfaces:**
- Produces: `hyperliquid::OpenPosition { coin: String, direction: String, size: f64, entry_px: f64, mark_px: f64, unrealized_pnl: f64, leverage: f64, notional: f64 }`; `Exchange::positions(&self) -> anyhow::Result<Vec<OpenPosition>>`; `GET /positions -> OpenPosition[]`.

- [ ] **Step 1: Add the `OpenPosition` type** — in `src/hyperliquid/mod.rs`, after the `Fill` struct:

```rust
/// A single open perp position, derived from `user_state.asset_positions`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OpenPosition {
    pub coin: String,
    pub direction: String, // "long" | "short"
    pub size: f64,         // absolute size
    pub entry_px: f64,
    pub mark_px: f64,
    pub unrealized_pnl: f64,
    pub leverage: f64,
    pub notional: f64,     // position value in USD
}
```

- [ ] **Step 2: Add the trait method** — in the `Exchange` trait, after `user_fills`:

```rust
    /// Open perp positions with mark price and unrealized PnL.
    async fn positions(&self) -> anyhow::Result<Vec<OpenPosition>>;
```

- [ ] **Step 3: Write the failing test** — in the mock module of `src/hyperliquid/mod.rs`, add a test asserting the mock returns seeded positions:

```rust
    #[tokio::test]
    async fn mock_returns_seeded_positions() {
        let mock = MockExchange::new_for_test();
        let p = super::OpenPosition {
            coin: "ETH".into(), direction: "long".into(), size: 1.0,
            entry_px: 2000.0, mark_px: 2100.0, unrealized_pnl: 100.0,
            leverage: 5.0, notional: 2100.0,
        };
        mock.set_positions(vec![p.clone()]);
        assert_eq!(mock.positions().await.unwrap(), vec![p]);
    }
```

- [ ] **Step 4: Implement the mock method** — add a `positions: Mutex<Vec<OpenPosition>>` field to `MockExchange`, a `set_positions` setter, and:

```rust
        async fn positions(&self) -> anyhow::Result<Vec<super::OpenPosition>> {
            Ok(self.positions.lock().unwrap().clone())
        }
```

(Initialize the new field to `Mutex::new(Vec::new())` in every mock constructor.)

- [ ] **Step 5: Implement the real method** — in `impl Exchange for HyperliquidExchange`, mirror the existing `position_size`/`equity` use of `self.info.user_state(self.address)`:

```rust
    async fn positions(&self) -> anyhow::Result<Vec<OpenPosition>> {
        let state = self.info.user_state(self.address).await?;
        let mut out = Vec::new();
        for ap in state.asset_positions.iter() {
            let p = &ap.position;
            let szi: f64 = p.szi.parse().unwrap_or(0.0);
            if szi == 0.0 {
                continue;
            }
            let entry_px = p.entry_px.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let notional: f64 = p.position_value.parse().unwrap_or(0.0);
            let unrealized_pnl: f64 = p.unrealized_pnl.parse().unwrap_or(0.0);
            let leverage: f64 = p.leverage.value as f64;
            let size = szi.abs();
            let mark_px = if size > 0.0 { notional / size } else { 0.0 };
            out.push(OpenPosition {
                coin: p.coin.clone(),
                direction: if szi >= 0.0 { "long".into() } else { "short".into() },
                size,
                entry_px,
                mark_px,
                unrealized_pnl,
                leverage,
                notional,
            });
        }
        Ok(out)
    }
```

(Field names on the SDK position struct — `szi`, `entry_px`, `position_value`, `unrealized_pnl`, `leverage.value` — must be confirmed against `hyperliquid_rust_sdk` 0.6's `AssetPosition`/`Position` types; adjust accessors to match, keep the output shape.)

- [ ] **Step 6: Run the mock test**

Run: `cargo test mock_returns_seeded_positions`
Expected: PASS.

- [ ] **Step 7: Add the handler + route** — in `src/api/mod.rs`:

```rust
async fn positions(State(state): State<ApiState>) -> Result<Json<Vec<crate::hyperliquid::OpenPosition>>, StatusCode> {
    let positions = state
        .exchange
        .positions()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(positions))
}
```

Register before the `.layer(...)`:

```rust
        .route("/positions", get(positions))
```

- [ ] **Step 8: Build + commit**

Run: `cargo build`
Expected: clean.

```bash
git add src/hyperliquid/mod.rs src/api/mod.rs
git commit -m "feat(api): GET /positions with unrealized PnL"
```

---

## Phase 3 — Closed trades with strategy metadata

### Task 4: Detailed fills + round-trip assembly

**Files:**
- Modify: `src/hyperliquid/mod.rs` (add `FillDetail` type + `fills_detailed()` trait method + impls)
- Create: `src/api/trades.rs` (pure `assemble_trades` + `ClosedTrade`)
- Modify: `src/api/mod.rs` (add `pub mod trades;`)

**Interfaces:**
- Produces:
  - `hyperliquid::FillDetail { coin, oid: u64, dir: String, px: f64, sz: f64, closed_pnl: f64, fee: f64, time_ms: i64, start_position: f64 }`
  - `Exchange::fills_detailed(&self) -> anyhow::Result<Vec<FillDetail>>`
  - `api::trades::ClosedTrade { external_id, coin, direction, size, entry_px, exit_px, realized_pnl, fee, opened_at_ms, closed_at_ms, entry_oid: Option<u64> }`
  - `api::trades::assemble_trades(fills: &[FillDetail]) -> Vec<ClosedTrade>`

- [ ] **Step 1: Add `FillDetail` + trait method** — in `src/hyperliquid/mod.rs`, after `Fill`:

```rust
/// A richer fill row for the API: enough to reconstruct round-trip trades.
/// Distinct from `Fill` so existing stats code is unaffected.
#[derive(Debug, Clone, PartialEq)]
pub struct FillDetail {
    pub coin: String,
    pub oid: u64,
    pub dir: String, // SDK `dir`, e.g. "Open Long" / "Close Long"
    pub px: f64,
    pub sz: f64,
    pub closed_pnl: f64,
    pub fee: f64,
    pub time_ms: i64,
    pub start_position: f64,
}
```

In the `Exchange` trait, after `positions`:

```rust
    /// All fills with price/size/order-id detail, oldest first.
    async fn fills_detailed(&self) -> anyhow::Result<Vec<FillDetail>>;
```

- [ ] **Step 2: Write the failing assembly test** — create `src/api/trades.rs`:

```rust
//! Pure reconstruction of round-trip trades from a chronological fill stream,
//! plus the API response shape. No network, no journal — testable in isolation.

use crate::hyperliquid::FillDetail;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClosedTrade {
    pub external_id: String,
    pub coin: String,
    pub direction: String, // "long" | "short"
    pub size: f64,
    pub entry_px: f64,
    pub exit_px: f64,
    pub realized_pnl: f64,
    pub fee: f64,
    pub opened_at_ms: i64,
    pub closed_at_ms: i64,
    pub entry_oid: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(coin: &str, oid: u64, dir: &str, px: f64, sz: f64, pnl: f64, fee: f64, t: i64) -> FillDetail {
        FillDetail {
            coin: coin.into(), oid, dir: dir.into(), px, sz,
            closed_pnl: pnl, fee, time_ms: t, start_position: 0.0,
        }
    }

    #[test]
    fn assembles_one_long_round_trip() {
        let fills = vec![
            fill("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            fill("ETH", 2, "Close Long", 2100.0, 1.0, 100.0, 1.0, 2000),
        ];
        let trades = assemble_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.coin, "ETH");
        assert_eq!(t.direction, "long");
        assert_eq!(t.size, 1.0);
        assert_eq!(t.entry_px, 2000.0);
        assert_eq!(t.exit_px, 2100.0);
        assert_eq!(t.realized_pnl, 100.0);
        assert_eq!(t.fee, 2.0); // entry + exit fee
        assert_eq!(t.opened_at_ms, 1000);
        assert_eq!(t.closed_at_ms, 2000);
        assert_eq!(t.entry_oid, Some(1));
        assert_eq!(t.external_id, "ETH:1:2000"); // coin:entry_oid:closed_at_ms
    }

    #[test]
    fn open_position_without_close_is_not_a_trade() {
        let fills = vec![fill("BTC", 9, "Open Short", 50000.0, 0.1, 0.0, 0.5, 1000)];
        assert!(assemble_trades(&fills).is_empty());
    }
}
```

- [ ] **Step 3: Run to confirm failure**

Run: `cargo test --lib api::trades`
Expected: FAIL — `assemble_trades` not defined.

- [ ] **Step 4: Implement `assemble_trades`** — prepend to `src/api/trades.rs` (above the test module):

```rust
/// Group a chronological fill stream into completed round-trip trades, one per
/// coin position life-cycle. A trade opens on the first fill that moves the
/// position away from flat and closes on the fill that returns it to flat
/// (identified by a non-zero `closed_pnl`). Still-open positions emit nothing.
pub fn assemble_trades(fills: &[FillDetail]) -> Vec<ClosedTrade> {
    use std::collections::HashMap;

    struct Open {
        entry_oid: u64,
        direction: String,
        size: f64,
        entry_px: f64,
        opened_at_ms: i64,
        fee: f64,
    }

    let mut open: HashMap<String, Open> = HashMap::new();
    let mut out = Vec::new();
    let mut ordered = fills.to_vec();
    ordered.sort_by_key(|f| f.time_ms);

    for f in &ordered {
        let is_close = f.closed_pnl != 0.0 || f.dir.contains("Close");
        if !is_close {
            // Opening (or adding to) a position: record/extend the open leg.
            let entry = open.entry(f.coin.clone()).or_insert(Open {
                entry_oid: f.oid,
                direction: if f.dir.contains("Long") { "long".into() } else { "short".into() },
                size: 0.0,
                entry_px: f.px,
                opened_at_ms: f.time_ms,
                fee: 0.0,
            });
            entry.size += f.sz;
            entry.fee += f.fee;
        } else if let Some(o) = open.remove(&f.coin) {
            out.push(ClosedTrade {
                external_id: format!("{}:{}:{}", f.coin, o.entry_oid, f.time_ms),
                coin: f.coin.clone(),
                direction: o.direction,
                size: o.size,
                entry_px: o.entry_px,
                exit_px: f.px,
                realized_pnl: f.closed_pnl,
                fee: o.fee + f.fee,
                opened_at_ms: o.opened_at_ms,
                closed_at_ms: f.time_ms,
                entry_oid: Some(o.entry_oid),
            });
        }
    }
    out
}
```

Add `pub mod trades;` to `src/api/mod.rs`.

- [ ] **Step 5: Run the test**

Run: `cargo test --lib api::trades`
Expected: PASS (2 tests).

- [ ] **Step 6: Implement `fills_detailed` for the mock + real exchange** — mock: add a `fills_detailed: Mutex<Vec<FillDetail>>` field + `set_fills_detailed` setter returning the clone. Real: mirror the existing `user_fills` body (which already calls the SDK fills endpoint) but map into `FillDetail`, reading `px`, `sz`, `oid`, `start_position`, `dir`, `closed_pnl`, `fee`, `time` from the SDK fill (confirm field names against the SDK type used in `user_fills`).

- [ ] **Step 7: Build + commit**

Run: `cargo build && cargo test --lib api::trades`
Expected: clean + PASS.

```bash
git add src/hyperliquid/mod.rs src/api/trades.rs src/api/mod.rs
git commit -m "feat(api): detailed fills + round-trip trade assembly"
```

---

### Task 5: Journal metadata read + `/trades` handler

**Files:**
- Modify: `src/journal.rs` (add `metadata_by_order_id`)
- Modify: `src/api/mod.rs` (add the `trades` handler + route)

**Interfaces:**
- Produces:
  - `journal::TradeMeta { confidence: Option<u8>, timeframe: Option<String>, profile: Option<String>, leverage: i64 }`
  - `Journal::metadata_by_order_id(&self) -> anyhow::Result<HashMap<u64, TradeMeta>>`
  - `GET /trades?since=<ms> -> TradeResponse[]` where each row is a `ClosedTrade` plus optional `{ confidence, timeframe, profile, leverage }`.

- [ ] **Step 1: Write the failing journal test** — add to the `tests` module in `src/journal.rs`:

```rust
    #[test]
    fn metadata_by_order_id_maps_entry_orders() {
        let journal = Journal::open_in_memory().unwrap();
        journal
            .record(/* coin */ "ETH", /* direction */ "long", /* size */ 1.0,
                    /* entry */ 2000.0, /* leverage */ 5, /* stop_loss */ 1900.0,
                    /* entry_order_id */ Some(42))
            .unwrap();
        let map = journal.metadata_by_order_id().unwrap();
        let meta = map.get(&42).expect("entry 42 present");
        assert_eq!(meta.leverage, 5);
    }
```

(Match `record`'s real parameter list and the signal-metadata args it takes; the point is that an entry with `entry_order_id = 42` becomes a map entry keyed by `42`.)

- [ ] **Step 2: Implement the read** — add to `src/journal.rs`:

```rust
use std::collections::HashMap;

/// Strategy metadata for one journaled entry, keyed elsewhere by its
/// exchange order id.
#[derive(Debug, Clone)]
pub struct TradeMeta {
    pub confidence: Option<u8>,
    pub timeframe: Option<String>,
    pub profile: Option<String>,
    pub leverage: i64,
}

impl Journal {
    /// Map of `entry_order_id -> TradeMeta` for every journaled entry that has a
    /// non-null order id. Used to enrich exchange-derived round-trip trades.
    pub fn metadata_by_order_id(&self) -> anyhow::Result<HashMap<u64, TradeMeta>> {
        let mut stmt = self.connection.prepare(
            "SELECT entry_order_id, confidence, timeframe, profile, leverage
             FROM trades WHERE entry_order_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let oid: i64 = row.get(0)?;
            Ok((
                oid as u64,
                TradeMeta {
                    confidence: row.get::<_, Option<i64>>(1)?.map(|v| v as u8),
                    timeframe: row.get(2)?,
                    profile: row.get(3)?,
                    leverage: row.get(4)?,
                },
            ))
        })?;
        let mut map = HashMap::new();
        for r in rows {
            let (oid, meta) = r?;
            map.insert(oid, meta);
        }
        Ok(map)
    }
}
```

(`self.connection` — match the real field name on `Journal`.)

- [ ] **Step 3: Run the journal test**

Run: `cargo test metadata_by_order_id_maps_entry_orders`
Expected: PASS.

- [ ] **Step 4: Add the `/trades` handler** — in `src/api/mod.rs`:

```rust
use crate::api::trades::{assemble_trades, ClosedTrade};
use crate::journal::Journal;
use axum::extract::Query;
use serde::Deserialize;

#[derive(Deserialize)]
struct TradesQuery {
    since: Option<i64>, // epoch ms; omit for all
}

#[derive(Serialize)]
struct TradeResponse {
    #[serde(flatten)]
    trade: ClosedTrade,
    confidence: Option<u8>,
    timeframe: Option<String>,
    profile: Option<String>,
    leverage: Option<i64>,
}

async fn trades(
    State(state): State<ApiState>,
    Query(q): Query<TradesQuery>,
) -> Result<Json<Vec<TradeResponse>>, StatusCode> {
    let fills = state
        .exchange
        .fills_detailed()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let mut assembled = assemble_trades(&fills);
    if let Some(since) = q.since {
        assembled.retain(|t| t.closed_at_ms >= since);
    }
    // Journal metadata is best-effort: a read failure must not 500 the trades.
    let meta = Journal::open(&state.db_path)
        .and_then(|j| j.metadata_by_order_id())
        .unwrap_or_default();
    let out = assembled
        .into_iter()
        .map(|trade| {
            let m = trade.entry_oid.and_then(|oid| meta.get(&oid));
            TradeResponse {
                confidence: m.and_then(|m| m.confidence),
                timeframe: m.and_then(|m| m.timeframe.clone()),
                profile: m.and_then(|m| m.profile.clone()),
                leverage: m.map(|m| m.leverage),
                trade,
            }
        })
        .collect();
    Ok(Json(out))
}
```

Register before `.layer(...)`:

```rust
        .route("/trades", get(trades))
```

- [ ] **Step 5: Write a handler test** — add to `api::tests`: seed the mock with two detailed fills forming one round-trip, call `/trades` with auth, assert one row with the expected `coin`/`realized_pnl`. (Use `MockExchange::new_for_test()` + `set_fills_detailed`, `db_path: ":memory:"`; metadata map is empty so the meta fields are `null` — that is fine.)

- [ ] **Step 6: Run tests + build**

Run: `cargo test --lib api && cargo build`
Expected: PASS + clean.

- [ ] **Step 7: Commit**

```bash
git add src/journal.rs src/api/mod.rs
git commit -m "feat(api): GET /trades with journal strategy metadata"
```

---

## Phase 4 — USDC flows

### Task 6: `/flows` handler

**Files:**
- Modify: `src/hyperliquid/mod.rs` (add `LedgerFlow` type + `usdc_flows()` trait method + impls)
- Modify: `src/api/mod.rs` (add the `flows` handler + route)

**Interfaces:**
- Produces: `hyperliquid::LedgerFlow { external_id: String, kind: String, usdc: f64, time_ms: i64 }` (`kind` = `"deposit"|"withdrawal"`); `Exchange::usdc_flows(&self) -> anyhow::Result<Vec<LedgerFlow>>`; `GET /flows?since=<ms> -> LedgerFlow[]`.

- [ ] **Step 1: Add the type + trait method** — in `src/hyperliquid/mod.rs`:

```rust
/// A USDC deposit/withdrawal from the non-funding ledger.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LedgerFlow {
    pub external_id: String, // "<hash>:<kind>"
    pub kind: String,        // "deposit" | "withdrawal"
    pub usdc: f64,
    pub time_ms: i64,
}
```

Trait, after `fills_detailed`:

```rust
    /// USDC deposits/withdrawals from the non-funding ledger, oldest first.
    async fn usdc_flows(&self) -> anyhow::Result<Vec<LedgerFlow>>;
```

- [ ] **Step 2: Implement mock + real** — mock: `flows: Mutex<Vec<LedgerFlow>>` + `set_flows` + clone-returning method (init `Mutex::new(Vec::new())` in all constructors). Real: POST `{"type":"userNonFundingLedgerUpdates","user":<addr>}` to the SDK info base URL (reuse the existing `reqwest::Client`/base-URL pattern already in this file), then map rows where `delta.type` is `deposit`/`withdraw` into `LedgerFlow` (`kind` = `deposit`/`withdrawal`, `usdc` from `delta.usdc`, `external_id` = `format!("{hash}:{kind}")`).

- [ ] **Step 3: Write the failing handler test** — in `api::tests`, seed the mock with one deposit + one withdrawal via `set_flows`, GET `/flows` with auth, assert two rows and the `kind` values.

- [ ] **Step 4: Add the handler + route** — in `src/api/mod.rs`:

```rust
#[derive(Deserialize)]
struct FlowsQuery {
    since: Option<i64>,
}

async fn flows(
    State(state): State<ApiState>,
    Query(q): Query<FlowsQuery>,
) -> Result<Json<Vec<crate::hyperliquid::LedgerFlow>>, StatusCode> {
    let mut flows = state
        .exchange
        .usdc_flows()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if let Some(since) = q.since {
        flows.retain(|f| f.time_ms >= since);
    }
    Ok(Json(flows))
}
```

Register before `.layer(...)`:

```rust
        .route("/flows", get(flows))
```

- [ ] **Step 5: Run tests + build**

Run: `cargo test --lib api && cargo build`
Expected: PASS + clean.

- [ ] **Step 6: Commit**

```bash
git add src/hyperliquid/mod.rs src/api/mod.rs
git commit -m "feat(api): GET /flows USDC deposits and withdrawals"
```

---

## Phase 5 — Server startup

### Task 7: Spawn the HTTP server alongside the dispatcher

**Files:**
- Modify: `src/config.rs` (add `http_bind_addr`, `api_token`, `journal_path` to `Config` + `from_env`)
- Modify: `src/main.rs` (spawn the server before `telegram::run`)

**Interfaces:**
- Consumes: `api::{ApiState, router}`, `Config`.
- Produces: an HTTP server bound to `config.http_bind_addr` serving the API, running concurrently with the Telegram dispatcher.

- [ ] **Step 1: Add config fields** — in `src/config.rs`, add to `Config`:

```rust
    pub http_bind_addr: String,
    pub api_token: Option<String>,
    pub journal_path: String,
```

and in `from_env` (matching the existing `std::env::var` style):

```rust
        http_bind_addr: std::env::var("HTTP_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8088".to_string()),
        api_token: std::env::var("PORTFOLIO_API_TOKEN").ok().filter(|s| !s.is_empty()),
        journal_path: std::env::var("JOURNAL_DB_PATH").unwrap_or_else(|_| "trades.db".to_string()),
```

(If `telegram::run` currently hardcodes the journal path, change it to read `config.journal_path` so both paths agree — single source of truth.)

- [ ] **Step 2: Spawn the server in `main`** — in `src/main.rs`, after building `exchange` and before `telegram::run`:

```rust
    let exchange = Arc::new(exchange);

    if let Some(token) = config.api_token.clone() {
        let state = api::ApiState {
            exchange: exchange.clone(),
            db_path: config.journal_path.clone(),
            token,
        };
        let bind = config.http_bind_addr.clone();
        tokio::spawn(async move {
            match tokio::net::TcpListener::bind(&bind).await {
                Ok(listener) => {
                    tracing::info!("portfolio API listening on {bind}");
                    if let Err(e) = axum::serve(listener, api::router(state)).await {
                        tracing::error!("portfolio API server stopped: {e}");
                    }
                }
                Err(e) => tracing::error!("portfolio API failed to bind {bind}: {e}"),
            }
        });
    } else {
        tracing::info!("PORTFOLIO_API_TOKEN unset — portfolio API disabled");
    }

    telegram::run(config, exchange).await
```

(`telegram::run` takes `Arc<E>`; pass the same `exchange` clone. Adjust the `exchange` binding so it is built once and shared — the server gets a clone, the dispatcher gets the original.)

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: clean.

- [ ] **Step 4: Manual smoke test** (optional, requires real creds) — set `PORTFOLIO_API_TOKEN=test` and run; in another shell:

```bash
curl -s -H "authorization: Bearer test" http://127.0.0.1:8088/balance
```

Expected: a JSON `{ "equity_usd": ..., "as_of_ms": ... }`. Without the header → `401`.

- [ ] **Step 5: Full suite + commit**

Run: `cargo test`
Expected: all pass.

```bash
git add src/config.rs src/main.rs src/telegram.rs
git commit -m "feat(api): serve read-only API alongside the Telegram dispatcher"
```

---

## Self-Review

**Spec coverage (bot-side sections of the design):**
- Bot read-only HTTP API + bearer auth → Tasks 1–2, 7. ✓
- `GET /balance` from `equity()` → Task 2. ✓
- `GET /positions` (open + unrealized PnL) → Task 3. ✓
- `GET /trades` (round-trip + realized PnL + journal metadata via `entry_order_id`) → Tasks 4–5. ✓
- `GET /flows` (USDC deposits/withdrawals) → Task 6. ✓
- New `FillDetail` type leaves existing `Fill`/stats untouched → Task 4. ✓
- Server runs alongside teloxide; read-only throughout → Task 7. ✓
- Config (`PORTFOLIO_API_TOKEN`, `HTTP_BIND_ADDR`, journal path) → Task 7. ✓

**Type consistency:** `OpenPosition`, `FillDetail`, `LedgerFlow`, `ClosedTrade`, `TradeMeta`, `ApiState` defined once and reused; `Exchange` gains `positions`/`fills_detailed`/`usdc_flows`, each implemented for both the real exchange and the mock (every mock constructor must initialize the new `Mutex` fields — the build catches omissions).

**Notes for the implementer (verify against live code, keep the test):**
- `hyperliquid_rust_sdk` 0.6 field names on the position type (`szi`, `entry_px`, `position_value`, `unrealized_pnl`, `leverage.value`) and on the fill type — confirm and adjust accessors; output shapes stay as defined.
- `MockExchange`'s real constructor name (`new_for_test` is assumed) and field set — match it; expose the mock to the `api` tests via `pub mod testing` if it is currently private.
- `Journal`'s connection field name and `record(...)` parameter list — match the real signatures in the journal test.
