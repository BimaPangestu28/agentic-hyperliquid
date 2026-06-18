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
    let exchange = hyperliquid::HyperliquidExchange::connect(&config).await?;
    telegram::run(config, Arc::new(exchange)).await
}
