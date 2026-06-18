mod api;
mod config;
mod hyperliquid;
mod journal;
mod llm_parser;
mod parser;
mod risk;
mod settings;
mod sizing;
mod state;
mod stats;
mod telegram;

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let config = config::from_env()?;
    let exchange = Arc::new(hyperliquid::HyperliquidExchange::connect(&config).await?);

    // Spawn the read-only portfolio HTTP API when a bearer token is configured.
    // A spawn failure must not bring down the Telegram bot.
    if let Some(api_token) = config.api_token.clone() {
        // Coerce the concrete Arc<HyperliquidExchange> to Arc<dyn Exchange> for ApiState.
        let api_exchange: Arc<dyn hyperliquid::Exchange> = exchange.clone();
        let api_state = api::ApiState {
            exchange: api_exchange,
            db_path: config.journal_path.clone(),
            token: api_token,
        };
        let bind_addr = config.http_bind_addr.clone();
        tokio::spawn(async move {
            match tokio::net::TcpListener::bind(&bind_addr).await {
                Ok(listener) => {
                    tracing::info!("portfolio API listening on {bind_addr}");
                    if let Err(server_error) = axum::serve(listener, api::router(api_state)).await {
                        tracing::error!("portfolio API server stopped: {server_error}");
                    }
                }
                Err(bind_error) => {
                    tracing::error!("portfolio API failed to bind {bind_addr}: {bind_error}");
                }
            }
        });
    } else {
        tracing::info!("PORTFOLIO_API_TOKEN unset — portfolio API disabled");
    }

    telegram::run(config, exchange).await
}
