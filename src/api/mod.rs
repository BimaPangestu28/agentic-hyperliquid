//! Read-only HTTP API consumed by portfolio-tracker. Trading logic lives
//! elsewhere; nothing here mutates exchange or journal state.

pub mod auth;
pub mod trades;

use crate::hyperliquid::Exchange;
use axum::{
    extract::State,
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::Arc;

#[derive(Clone)]
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
        .route("/balance", get(balance))
        .route("/positions", get(positions))
        .route("/watchlist", get(watchlist))
        .route("/manual-scans", get(manual_scans))
        .route("/trades", get(trades))
        .route("/flows", get(flows))
        .route("/execute", axum::routing::post(execute))
        .layer(middleware::from_fn_with_state(state.clone(), require_bearer))
        .with_state(state)
}

// ── Handlers ─────────────────────────────────────────────────────────────────

use axum::extract::Query;
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct BalanceResponse {
    equity_usd: f64,
    as_of_ms: i64,
}

#[derive(Serialize)]
struct WatchlistResponse {
    coins: Vec<String>,
    auto_scalp_enabled: bool,
    max_open_positions: u32,
}

#[derive(Serialize)]
struct ManualScansResponse {
    coins: Vec<String>,
}

async fn balance(State(state): State<ApiState>) -> Result<Json<BalanceResponse>, StatusCode> {
    let equity = state
        .exchange
        .equity()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(BalanceResponse { equity_usd: equity, as_of_ms: now_ms() }))
}

async fn positions(
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::hyperliquid::OpenPosition>>, StatusCode> {
    let open_positions = state
        .exchange
        .positions()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(open_positions))
}

/// Query parameters for the `/trades` endpoint.
#[derive(Deserialize)]
struct TradesQuery {
    /// Filter to trades closed at or after this epoch-millisecond timestamp.
    /// Omit to return all closed trades.
    since: Option<i64>,
}

/// API response row: a closed round-trip trade enriched with optional strategy
/// metadata from the local trade journal.
#[derive(Serialize)]
struct TradeResponse {
    #[serde(flatten)]
    trade: crate::api::trades::ClosedTrade,
    confidence: Option<u8>,
    timeframe: Option<String>,
    profile: Option<String>,
    leverage: Option<i64>,
}

async fn trades(
    State(state): State<ApiState>,
    Query(query): Query<TradesQuery>,
) -> Result<Json<Vec<TradeResponse>>, StatusCode> {
    let fills = state
        .exchange
        .fills_detailed()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let mut assembled = crate::api::trades::assemble_trades(&fills);
    if let Some(since_ms) = query.since {
        assembled.retain(|trade| trade.closed_at_ms >= since_ms);
    }
    // Drop financially nonsensical trades that arose from parse failures upstream.
    // A non-positive entry_px, exit_px, or size indicates a 0.0 fallback from
    // unwrap_or(0.0) in the fill-detail mapping layer.
    assembled.retain(|trade| {
        if trade.entry_px <= 0.0 || trade.exit_px <= 0.0 || trade.size <= 0.0 {
            tracing::warn!(
                coin = %trade.coin,
                external_id = %trade.external_id,
                entry_px = trade.entry_px,
                exit_px = trade.exit_px,
                size = trade.size,
                "dropping degenerate trade with non-positive entry_px, exit_px, or size"
            );
            return false;
        }
        true
    });
    // Journal metadata is best-effort: a read failure must not 500 the response.
    let metadata_map = crate::journal::Journal::open(&state.db_path)
        .and_then(|journal| journal.metadata_by_order_id())
        .unwrap_or_default();
    let response_rows = assembled
        .into_iter()
        .map(|trade| {
            let matched_meta = trade.entry_oid.and_then(|oid| metadata_map.get(&oid));
            TradeResponse {
                confidence: matched_meta.and_then(|m| m.confidence),
                timeframe: matched_meta.and_then(|m| m.timeframe.clone()),
                profile: matched_meta.and_then(|m| m.profile.clone()),
                leverage: matched_meta.map(|m| m.leverage),
                trade,
            }
        })
        .collect();
    Ok(Json(response_rows))
}

/// Query parameters for the `/flows` endpoint.
#[derive(Deserialize)]
struct FlowsQuery {
    /// Filter to flows at or after this epoch-millisecond timestamp.
    /// Omit to return all flows.
    since: Option<i64>,
}

async fn flows(
    State(state): State<ApiState>,
    Query(query): Query<FlowsQuery>,
) -> Result<Json<Vec<crate::hyperliquid::LedgerFlow>>, StatusCode> {
    let mut ledger_flows = state
        .exchange
        .usdc_flows()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if let Some(since) = query.since {
        ledger_flows.retain(|flow| flow.time_ms >= since);
    }
    Ok(Json(ledger_flows))
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

/// Drains the manual `/scan` queue: returns the pending coins and marks them processed,
/// so the scraper picks up each request exactly once. Same SQLite file as the journal.
async fn manual_scans(State(state): State<ApiState>) -> Result<Json<ManualScansResponse>, StatusCode> {
    let store = crate::manual_scan_store::ManualScanStore::open(&state.db_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let coins = store.drain_pending().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ManualScansResponse { coins }))
}

/// How long an auto-scalp LIMIT entry may rest before it's cancelled as unfilled. Kept
/// short: the /execute call blocks the scraper synchronously while it waits, and a scalp
/// entry is time-sensitive (the scraper re-scans on its own interval). Must stay under the
/// scraper's EXECUTE_TIMEOUT_MS so the client doesn't give up before the order resolves.
const AUTO_SCALP_FILL_TIMEOUT_SECS: u64 = 20;

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
    /// Set by a manual `/scan` request: bypasses only the auto-scalp kill-switch (the
    /// user asked for this trade explicitly). Confidence, position-cap and margin gates
    /// still apply.
    #[serde(default)]
    manual: bool,
}

/// Error returned by the [`execute`] handler.
///
/// Carries a machine-readable `reason` (and, where useful, contextual fields)
/// in the JSON body so the caller can tell *which* gate rejected the request.
/// A bare `409 CONFLICT` is ambiguous between the kill-switch and the position
/// cap; this lets the scraper report the actual cause instead of guessing.
struct ExecuteError {
    status: StatusCode,
    body: serde_json::Value,
}

impl ExecuteError {
    /// Builds an error whose body is `{ "ok": false, "reason": <reason> }`.
    fn reason(status: StatusCode, reason: &str) -> Self {
        Self {
            status,
            body: serde_json::json!({ "ok": false, "reason": reason }),
        }
    }

    /// Builds an error with a caller-supplied JSON body (for extra context such
    /// as the open/max counts on a position-cap rejection).
    fn detailed(status: StatusCode, body: serde_json::Value) -> Self {
        Self { status, body }
    }
}

/// Lets `?` convert the bare `StatusCode` returned by infrastructure failures
/// (DB/exchange errors mapped via `map_err`/`ok_or`) into an [`ExecuteError`]
/// with a generic reason derived from the status.
impl From<StatusCode> for ExecuteError {
    fn from(status: StatusCode) -> Self {
        let reason = match status {
            StatusCode::INTERNAL_SERVER_ERROR => "internal_error",
            StatusCode::UNPROCESSABLE_ENTITY => "unprocessable",
            StatusCode::BAD_REQUEST => "bad_request",
            _ => "error",
        };
        Self::reason(status, reason)
    }
}

impl IntoResponse for ExecuteError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

async fn execute(
    State(state): State<ApiState>,
    Json(req): Json<ExecuteRequest>,
) -> Result<Json<serde_json::Value>, ExecuteError> {
    // Load current settings from the DB.
    let store = crate::settings::SettingsStore::open(&state.db_path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let settings = store
        .load(state.settings_seed.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Gate 1: master kill-switch — bypassed for an explicit manual /scan request.
    if !settings.auto_scalp_enabled && !req.manual {
        return Err(ExecuteError::reason(StatusCode::CONFLICT, "kill_switch"));
    }
    // Gate 2: confidence threshold.
    if req.confidence < 7 {
        return Err(ExecuteError::reason(
            StatusCode::UNPROCESSABLE_ENTITY,
            "low_confidence",
        ));
    }
    // Gate 3: position cap.
    let open_positions = state
        .exchange
        .positions()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if open_positions.len() as u32 >= settings.max_open_positions {
        return Err(ExecuteError::detailed(
            StatusCode::CONFLICT,
            serde_json::json!({
                "ok": false,
                "reason": "position_cap",
                "open": open_positions.len(),
                "max": settings.max_open_positions,
            }),
        ));
    }

    // Build the TradeSetup (single TP, 100% allocation).
    let direction = match req.direction.to_lowercase().as_str() {
        "long" => crate::parser::Direction::Long,
        "short" => crate::parser::Direction::Short,
        _ => return Err(ExecuteError::reason(StatusCode::BAD_REQUEST, "bad_direction")),
    };
    let setup = crate::parser::TradeSetup {
        coin: req.coin.to_uppercase(),
        direction,
        timeframe: Some("scalp".to_string()),
        risk_reward: None,
        confidence: Some(req.confidence),
        entry: req.entry,
        stop_loss: req.stop_loss,
        take_profits: vec![crate::parser::TakeProfit {
            price: req.take_profit,
            allocation_pct: 100.0,
        }],
    };

    // Size with the Moderate profile (default for auto-scalp), market entry.
    let equity = state
        .exchange
        .equity()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let asset_meta = state
        .exchange
        .asset_meta(&setup.coin)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
    let plan = crate::sizing::build_plan(&crate::sizing::SizingInput {
        setup: &setup,
        equity,
        risk_pct: settings.risk_pct,
        entry_mode: settings.entry_mode,
        entry_pct: settings.entry_pct,
        entry_fixed_usd: settings.entry_fixed_usd,
        profile: crate::sizing::RiskProfile::Moderate,
        leverage: &settings.leverage,
        asset_meta: &asset_meta,
    })
    .map_err(|error| {
        // Log the concrete sizing rejection (wrong stop side, below-min leg, leverage cap,
        // …) — without this a 422 is opaque in the pod logs.
        tracing::warn!(
            coin = %setup.coin, entry = setup.entry, stop = setup.stop_loss, tp = req.take_profit,
            "auto-scalp sizing rejected: {error}"
        );
        StatusCode::UNPROCESSABLE_ENTITY
    })?;

    // Affordability gate: sizing checks margin against TOTAL equity, but the exchange
    // accepts a new order only against FREE collateral (equity minus margin already used by
    // open positions). When the account is fully margined this is ~0, so the order would be
    // rejected with "Insufficient margin" → a 500. Reject up-front as a capacity conflict
    // instead, so the scraper backs off the coin rather than retrying a doomed order.
    let free_collateral = state
        .exchange
        .free_collateral()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if plan.margin > free_collateral {
        tracing::warn!(
            coin = %setup.coin, required_margin = plan.margin, free_collateral,
            "auto-scalp skipped: insufficient free collateral (account near capacity)"
        );
        return Err(ExecuteError::detailed(
            StatusCode::CONFLICT,
            serde_json::json!({
                "ok": false,
                "reason": "insufficient_margin",
                "required_margin": plan.margin,
                "free_collateral": free_collateral,
            }),
        ));
    }

    // Execute: LIMIT entry at the setup price + SL/TP bracket, no confirm. A limit (maker)
    // matches the manual SETUP flow — exact entry, cheaper fees, no market slippage. If it
    // doesn't fill within AUTO_SCALP_FILL_TIMEOUT_SECS the order is cancelled and no
    // position is opened (a stale scalp entry shouldn't rest — the scraper retries later).
    // NOT settings.entry_fill_timeout_secs, which is the manual flow's (much longer) value.
    crate::telegram::execute_plan(
        state.exchange.as_ref(),
        &plan,
        true,
        AUTO_SCALP_FILL_TIMEOUT_SECS,
        std::sync::Arc::new(tokio::sync::Notify::new()),
        &crate::telegram::NoopReporter,
    )
    .await
    .map_err(|error| {
        // The Hyperliquid order placement (leverage / market entry / bracket) failed. This
        // is the LINK-style 500 — surface the underlying exchange error instead of dropping
        // it, so the cause (tick size, min notional, margin, TP side, …) is visible.
        tracing::error!(
            coin = %setup.coin, size = plan.size, entry = plan.entry,
            stop = plan.stop_loss.price, leverage = plan.leverage,
            "auto-scalp execution failed: {error:#}"
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Notify via Telegram (best-effort: individual send failures are swallowed).
    use teloxide::prelude::Requester;
    let bot = teloxide::Bot::new(&state.telegram_bot_token);
    let msg = format!(
        "{} {} {} {} @ {} · SL {} · TP {} · conf {}/10\n{}",
        if req.manual { "🖐 Manual-buka" } else { "🤖 Auto-buka" },
        setup.coin,
        if matches!(setup.direction, crate::parser::Direction::Long) { "LONG" } else { "SHORT" },
        plan.size,
        plan.entry,
        plan.stop_loss.price,
        req.take_profit,
        req.confidence,
        req.thesis
    );
    for user_id in &state.allowed_user_ids {
        let _ = bot
            .send_message(teloxide::types::ChatId(*user_id), &msg)
            .await;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Returns the current time as milliseconds since the Unix epoch.
///
/// Isolated into its own function so handlers remain deterministic under test
/// if a future revision stubs time injection.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        }
    }

    #[tokio::test]
    async fn balance_requires_auth() {
        let app = router(state_with(1000.0));
        let response = app
            .oneshot(Request::builder().uri("/balance").body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn balance_returns_equity() {
        let app = router(state_with(1234.5));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/balance")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["equity_usd"].as_f64().unwrap(), 1234.5);
    }

    #[tokio::test]
    async fn execute_rejects_when_free_collateral_below_required_margin() {
        // Account has plenty of equity but is fully margined (free collateral ~0). The
        // order must be rejected up-front as a capacity conflict (409), not attempted and
        // 500'd at the exchange.
        let mock = MockExchange {
            equity: 1000.0,
            meta: Some(crate::sizing::AssetMeta { sz_decimals: 2, max_leverage: 0 }),
            ..Default::default()
        };
        mock.set_free_collateral(1.0);
        let mut seed = crate::settings::sample();
        seed.auto_scalp_enabled = true;
        let state = ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: seed,
            telegram_bot_token: "test-token".into(),
            allowed_user_ids: vec![1],
        };
        let body = serde_json::json!({
            "coin": "LINK", "direction": "long", "entry": 7.33, "stop_loss": 7.17,
            "take_profit": 7.60, "confidence": 7, "thesis": "t"
        })
        .to_string();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execute")
                    .header("authorization", "Bearer t")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["reason"].as_str().unwrap(), "insufficient_margin");
    }

    fn state_with_positions(open_positions: Vec<crate::hyperliquid::OpenPosition>) -> ApiState {
        let mock = MockExchange::with_equity(0.0);
        mock.set_positions(open_positions);
        ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        }
    }

    #[tokio::test]
    async fn positions_requires_auth() {
        let app = router(state_with_positions(vec![]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/positions")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn positions_returns_seeded_positions() {
        let seeded_position = crate::hyperliquid::OpenPosition {
            coin: "BTC".into(),
            direction: "long".into(),
            size: 0.5,
            entry_px: 60000.0,
            mark_px: 62000.0,
            unrealized_pnl: 1000.0,
            leverage: 10.0,
            notional: 31000.0,
        };
        let app = router(state_with_positions(vec![seeded_position]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/positions")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let positions_array = json.as_array().unwrap();
        assert_eq!(positions_array.len(), 1);
        assert_eq!(positions_array[0]["coin"].as_str().unwrap(), "BTC");
        assert_eq!(positions_array[0]["direction"].as_str().unwrap(), "long");
        assert_eq!(positions_array[0]["size"].as_f64().unwrap(), 0.5);
    }

    #[tokio::test]
    async fn positions_returns_empty_array_when_no_positions() {
        let app = router(state_with_positions(vec![]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/positions")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    fn make_fill_detail(
        coin: &str,
        oid: u64,
        dir: &str,
        px: f64,
        sz: f64,
        pnl: f64,
        fee: f64,
        time_ms: i64,
    ) -> crate::hyperliquid::FillDetail {
        crate::hyperliquid::FillDetail {
            coin: coin.into(),
            oid,
            dir: dir.into(),
            px,
            sz,
            closed_pnl: pnl,
            fee,
            time_ms,
            start_position: 0.0,
        }
    }

    #[tokio::test]
    async fn trades_returns_one_closed_trade_with_no_journal_metadata() {
        // Two fills forming a complete ETH long round-trip. Journal is ":memory:"
        // (empty) so meta fields must all be null.
        let mock = MockExchange::new_for_test();
        mock.set_fills_detailed(vec![
            make_fill_detail("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            make_fill_detail("ETH", 2, "Close Long", 2100.0, 1.0, 100.0, 1.0, 2000),
        ]);
        let app = router(ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/trades")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let trades_array = json.as_array().unwrap();
        assert_eq!(trades_array.len(), 1);
        let trade = &trades_array[0];
        assert_eq!(trade["coin"].as_str().unwrap(), "ETH");
        assert_eq!(trade["realized_pnl"].as_f64().unwrap(), 100.0);
        // No journal entry — metadata fields must be null.
        assert!(trade["confidence"].is_null());
        assert!(trade["timeframe"].is_null());
        assert!(trade["profile"].is_null());
        assert!(trade["leverage"].is_null());
    }

    #[tokio::test]
    async fn trades_excludes_degenerate_trades_with_zero_entry_px() {
        // A fill with px=0.0 (parse failure fallback) must be dropped with a warn.
        // The valid ETH trade must remain; the degenerate BTC trade must be excluded.
        let mock = MockExchange::new_for_test();
        mock.set_fills_detailed(vec![
            // Valid ETH long round-trip.
            make_fill_detail("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            make_fill_detail("ETH", 2, "Close Long", 2100.0, 1.0, 100.0, 1.0, 2000),
            // Degenerate BTC trade: entry_px will be 0.0 (simulates parse failure).
            make_fill_detail("BTC", 3, "Open Long", 0.0, 0.5, 0.0, 0.5, 3000),
            make_fill_detail("BTC", 4, "Close Long", 50000.0, 0.5, 50.0, 0.5, 4000),
        ]);
        let app = router(ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/trades")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let trades_array = json.as_array().unwrap();
        // Only the valid ETH trade must appear; BTC is dropped.
        assert_eq!(trades_array.len(), 1);
        assert_eq!(trades_array[0]["coin"].as_str().unwrap(), "ETH");
    }

    #[tokio::test]
    async fn trades_excludes_degenerate_trades_with_zero_exit_px() {
        // A trade whose closing fill parses as px=0.0 must be dropped.
        // The valid ETH trade must remain; the SOL trade with zero exit_px must be excluded.
        let mock = MockExchange::new_for_test();
        mock.set_fills_detailed(vec![
            // Valid ETH long round-trip.
            make_fill_detail("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            make_fill_detail("ETH", 2, "Close Long", 2100.0, 1.0, 100.0, 1.0, 2000),
            // Degenerate SOL trade: exit_px (close px) is 0.0 — simulates a parse failure.
            make_fill_detail("SOL", 3, "Open Long", 150.0, 10.0, 0.0, 0.5, 3000),
            make_fill_detail("SOL", 4, "Close Long", 0.0, 10.0, 50.0, 0.5, 4000),
        ]);
        let app = router(ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/trades")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let trades_array = json.as_array().unwrap();
        // Only the valid ETH trade must appear; SOL with zero exit_px is dropped.
        assert_eq!(trades_array.len(), 1);
        assert_eq!(trades_array[0]["coin"].as_str().unwrap(), "ETH");
    }

    fn state_with_flows(ledger_flows: Vec<crate::hyperliquid::LedgerFlow>) -> ApiState {
        let mock = MockExchange::new_for_test();
        mock.set_flows(ledger_flows);
        ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        }
    }

    #[tokio::test]
    async fn flows_requires_auth() {
        let app = router(state_with_flows(vec![]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/flows")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn flows_returns_seeded_deposit_and_withdrawal() {
        let deposit = crate::hyperliquid::LedgerFlow {
            external_id: "0xabc:deposit".into(),
            kind: "deposit".into(),
            usdc: 500.0,
            time_ms: 1000,
        };
        let withdrawal = crate::hyperliquid::LedgerFlow {
            external_id: "0xdef:withdrawal".into(),
            kind: "withdrawal".into(),
            usdc: 200.0,
            time_ms: 2000,
        };
        let app = router(state_with_flows(vec![deposit, withdrawal]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/flows")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let flows_array = json.as_array().unwrap();
        assert_eq!(flows_array.len(), 2);
        assert_eq!(flows_array[0]["kind"].as_str().unwrap(), "deposit");
        assert_eq!(flows_array[0]["usdc"].as_f64().unwrap(), 500.0);
        assert_eq!(flows_array[1]["kind"].as_str().unwrap(), "withdrawal");
        assert_eq!(flows_array[1]["usdc"].as_f64().unwrap(), 200.0);
    }

    /// Pins the signed-usdc convention at the handler level: a withdrawal seeded
    /// with a negative usdc value must be returned as-is — the handler must not
    /// negate or take abs() of the value.
    #[tokio::test]
    async fn flows_withdrawal_usdc_is_negative() {
        let withdrawal = crate::hyperliquid::LedgerFlow {
            external_id: "0xwithdraw:withdrawal".into(),
            kind: "withdrawal".into(),
            usdc: -200.5,
            time_ms: 1000,
        };
        let app = router(state_with_flows(vec![withdrawal]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/flows")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let flows_array = json.as_array().unwrap();
        assert_eq!(flows_array.len(), 1);
        assert_eq!(flows_array[0]["kind"].as_str().unwrap(), "withdrawal");
        assert_eq!(
            flows_array[0]["usdc"].as_f64().unwrap(),
            -200.5,
            "withdrawal usdc must be negative — sign must not be stripped by the handler"
        );
    }

    #[tokio::test]
    async fn flows_since_filters_by_time_ms() {
        let old_flow = crate::hyperliquid::LedgerFlow {
            external_id: "0xold:deposit".into(),
            kind: "deposit".into(),
            usdc: 100.0,
            time_ms: 1000,
        };
        let new_flow = crate::hyperliquid::LedgerFlow {
            external_id: "0xnew:deposit".into(),
            kind: "deposit".into(),
            usdc: 300.0,
            time_ms: 5000,
        };
        let app = router(state_with_flows(vec![old_flow, new_flow]));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/flows?since=5000")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let flows_array = json.as_array().unwrap();
        assert_eq!(flows_array.len(), 1);
        assert_eq!(flows_array[0]["usdc"].as_f64().unwrap(), 300.0);
    }

    #[tokio::test]
    async fn trades_since_filters_by_closed_at_ms() {
        let mock = MockExchange::new_for_test();
        mock.set_fills_detailed(vec![
            // Old ETH trade closed at ms=2000.
            make_fill_detail("ETH", 1, "Open Long", 2000.0, 1.0, 0.0, 1.0, 1000),
            make_fill_detail("ETH", 2, "Close Long", 2100.0, 1.0, 100.0, 1.0, 2000),
            // Recent SOL trade closed at ms=6000.
            make_fill_detail("SOL", 5, "Open Long", 150.0, 10.0, 0.0, 0.5, 5000),
            make_fill_detail("SOL", 6, "Close Long", 160.0, 10.0, 100.0, 0.5, 6000),
        ]);
        let app = router(ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: crate::settings::sample(),
            telegram_bot_token: "test-token".to_string(),
            allowed_user_ids: vec![1],
        });
        // Filter since=5000 — only SOL should come back.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/trades?since=5000")
                    .header("authorization", "Bearer t")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let trades_array = json.as_array().unwrap();
        assert_eq!(trades_array.len(), 1);
        assert_eq!(trades_array[0]["coin"].as_str().unwrap(), "SOL");
    }

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
            Request::builder().uri("/watchlist").header("authorization", "Bearer t")
                .body(axum::body::Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    fn execute_body(confidence: u8) -> String {
        format!(
            r#"{{"coin":"AVAX","direction":"long","entry":6.12,"stop_loss":5.99,"take_profit":6.32,"confidence":{confidence},"thesis":"t"}}"#
        )
    }

    /// Parses the JSON body of an `/execute` rejection and returns its `reason`,
    /// so tests can assert *which* gate fired rather than only the status code.
    async fn rejection_reason(response: axum::response::Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["reason"].as_str().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn execute_rejected_when_auto_scalp_disabled() {
        // state_with leaves auto_scalp_enabled at the seed default (false).
        let app = router(state_with(1000.0));
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execute")
                    .header("authorization", "Bearer t")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(execute_body(8)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT); // 409 kill-switch
        assert_eq!(rejection_reason(res).await, "kill_switch");
    }

    #[tokio::test]
    async fn execute_manual_request_bypasses_kill_switch() {
        // auto_scalp is off (seed default false). A normal request returns kill_switch; a
        // manual one must get PAST gate 1 — it then fails later (no asset meta on the mock),
        // so the one thing we assert is that the reason is NOT kill_switch.
        let app = router(state_with(1000.0));
        let body = r#"{"coin":"AVAX","direction":"long","entry":6.12,"stop_loss":5.99,"take_profit":6.32,"confidence":8,"thesis":"t","manual":true}"#;
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execute")
                    .header("authorization", "Bearer t")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(rejection_reason(res).await, "kill_switch");
    }

    #[tokio::test]
    async fn manual_scans_requires_auth_and_returns_coins_array() {
        let app = router(state_with(1000.0));
        // no bearer → 401
        let res = app.clone().oneshot(
            Request::builder().uri("/manual-scans").body(axum::body::Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // with bearer → 200 + {"coins": []} (fresh :memory: queue is empty)
        let res = app.oneshot(
            Request::builder().uri("/manual-scans").header("authorization", "Bearer t")
                .body(axum::body::Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["coins"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn execute_rejected_when_confidence_below_gate() {
        let mut state = state_with(1000.0);
        state.settings_seed.auto_scalp_enabled = true;
        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execute")
                    .header("authorization", "Bearer t")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(execute_body(6)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY); // 422 conf<7
        assert_eq!(rejection_reason(res).await, "low_confidence");
    }

    #[tokio::test]
    async fn execute_rejected_at_position_cap_reports_open_and_max() {
        // Auto-scalp on, but open positions already at the cap → 409 position_cap.
        let mut seed = crate::settings::sample();
        seed.auto_scalp_enabled = true;
        seed.max_open_positions = 1;
        let mock = MockExchange::with_equity(1000.0);
        mock.set_positions(vec![crate::hyperliquid::OpenPosition {
            coin: "BTC".into(),
            direction: "long".into(),
            size: 0.1,
            entry_px: 60000.0,
            mark_px: 60000.0,
            unrealized_pnl: 0.0,
            leverage: 10.0,
            notional: 6000.0,
        }]);
        let state = ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
            settings_seed: seed,
            telegram_bot_token: "test-token".into(),
            allowed_user_ids: vec![1],
        };
        let res = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execute")
                    .header("authorization", "Bearer t")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(execute_body(8)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT); // 409 position cap
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["reason"].as_str().unwrap(), "position_cap");
        assert_eq!(json["open"].as_u64().unwrap(), 1);
        assert_eq!(json["max"].as_u64().unwrap(), 1);
    }
}
