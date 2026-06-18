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
}
