//! Read-only HTTP API consumed by portfolio-tracker. Trading logic lives
//! elsewhere; nothing here mutates exchange or journal state.

pub mod auth;
pub mod trades;

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
        .route("/balance", get(balance))
        .route("/positions", get(positions))
        .route("/trades", get(trades))
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
    // A non-positive entry_px or size indicates a 0.0 fallback from unwrap_or(0.0).
    assembled.retain(|trade| {
        if trade.entry_px <= 0.0 || trade.size <= 0.0 {
            tracing::warn!(
                coin = %trade.coin,
                external_id = %trade.external_id,
                entry_px = trade.entry_px,
                size = trade.size,
                "dropping degenerate trade with non-positive entry_px or size"
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

    fn state_with_positions(open_positions: Vec<crate::hyperliquid::OpenPosition>) -> ApiState {
        let mock = MockExchange::with_equity(0.0);
        mock.set_positions(open_positions);
        ApiState {
            exchange: Arc::new(mock),
            db_path: ":memory:".into(),
            token: "t".into(),
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
}
