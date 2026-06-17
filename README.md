# Agent Hyperliquid

Telegram bot that turns a pasted trading-setup card into a long/short position
with native SL/TP brackets on the Hyperliquid perpetuals DEX.

## Prerequisites
- Rust (stable, edition 2021)
- A Hyperliquid **API/agent wallet** private key
- A Telegram bot token (from @BotFather) and your numeric Telegram user id

## Setup
1. `cp .env.example .env` and fill in the values.
2. Keep `HYPERLIQUID_NETWORK=testnet` until you have validated end-to-end.
3. `cargo run`

## Usage
Paste a trading-setup card into the bot chat. It replies with a sized summary
and buttons: switch risk profile (Conservative/Moderate/Aggressive → leverage),
then **Confirm Limit** or **Confirm Market**, or **Cancel**. On confirmation it
sets leverage, places the entry, and arms a reduce-only SL + TP1 + TP2 bracket.

## Configuration
See `.env.example`. Defaults: risk 1%/trade, leverage 2/3/5x, fill timeout 300s.

## Safety
- Uses an agent wallet, never your main wallet key.
- Only allowlisted Telegram user ids are served.
- Every execution requires explicit confirmation.
