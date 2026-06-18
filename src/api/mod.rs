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
        .route("/balance", get(balance))
        .route("/positions", get(positions))
        .layer(middleware::from_fn_with_state(state.clone(), require_bearer))
        .with_state(state)
}

// ── Handlers ─────────────────────────────────────────────────────────────────

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
}
