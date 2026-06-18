//! Local HTTP endpoint so web-only signal sources can be piped in via a hotkey.
//! Binds 127.0.0.1 ONLY and requires a shared token. Reuses process_signal.

use crate::config::ingest_authorized;
use crate::hyperliquid::Exchange;
use crate::telegram::{process_signal, BotContext};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::ChatId;

/// Shared state passed to every ingest handler invocation.
pub struct IngestState<E: Exchange + 'static> {
    pub bot: Bot,
    pub context: Arc<BotContext<E>>,
    /// The configured bearer token. `None` means the server was not started.
    pub token: Option<String>,
    /// Target Telegram chat that receives confirmation cards.
    pub chat_id: ChatId,
}

/// Handles `POST /ingest`: verifies the token, rejects empty bodies, then
/// hands the signal text to `process_signal` — the same pipeline as Telegram.
async fn handle_ingest<E: Exchange + Send + Sync + 'static>(
    State(state): State<Arc<IngestState<E>>>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, &'static str) {
    let provided = headers
        .get("x-ingest-token")
        .and_then(|header_value| header_value.to_str().ok());

    if !ingest_authorized(state.token.as_deref(), provided) {
        return (StatusCode::UNAUTHORIZED, "unauthorized");
    }

    if body.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "empty body");
    }

    match process_signal(&state.bot, &state.context, state.chat_id, &body).await {
        Ok(()) => (StatusCode::OK, "ok"),
        Err(error) => {
            tracing::error!("ingest processing failed: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "processing error")
        }
    }
}

/// Spawns the ingest HTTP server if configured (port + token + a resolvable chat id).
///
/// Returns immediately; the server runs in a background task. If any required
/// piece is missing the function logs a warning and returns without starting.
/// The server ALWAYS binds `127.0.0.1` only — never `0.0.0.0`.
pub fn spawn<E: Exchange + Send + Sync + 'static>(
    bot: Bot,
    context: Arc<BotContext<E>>,
    port: Option<u16>,
    token: Option<String>,
    chat_id: Option<i64>,
) {
    let port = match port {
        Some(port_number) => port_number,
        None => return,
    };

    if token.is_none() {
        tracing::warn!(
            "INGEST_PORT set but INGEST_TOKEN missing; \
             ingest server NOT started (refusing tokenless endpoint)"
        );
        return;
    }

    let resolved_chat_id = match chat_id {
        Some(raw_id) => ChatId(raw_id),
        None => {
            tracing::warn!(
                "ingest: no INGEST_CHAT_ID and no allowlisted user to default to; \
                 ingest server NOT started"
            );
            return;
        }
    };

    let state = Arc::new(IngestState {
        bot,
        context,
        token,
        chat_id: resolved_chat_id,
    });

    let app = Router::new()
        .route("/ingest", post(handle_ingest::<E>))
        .with_state(state);

    tokio::spawn(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!("ingest endpoint listening on http://127.0.0.1:{port}/ingest");
                if let Err(bind_error) = axum::serve(listener, app).await {
                    tracing::error!("ingest server error: {bind_error}");
                }
            }
            Err(bind_error) => {
                tracing::error!("ingest bind failed on 127.0.0.1:{port}: {bind_error}");
            }
        }
    });
}
