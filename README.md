# Agent Hyperliquid

Telegram bot that turns a pasted trading-setup card into a long/short position
with native SL/TP brackets on the Hyperliquid perpetuals DEX.

## Prerequisites
- Rust (stable, edition 2021)
- A Hyperliquid **API/agent wallet** private key
- A Telegram bot token (from @BotFather) and your numeric Telegram user id
- When using an API/agent wallet, set `HYPERLIQUID_ACCOUNT_ADDRESS` to your master account address (the one that holds funds); the agent key only signs — it holds no balance of its own.
- If your account is in unified-account mode, set `HYPERLIQUID_UNIFIED_ACCOUNT=true` so the bot reads collateral from the spot USDC balance (the perp account reports 0 under unified mode).

## Setup
1. `cp .env.example .env` and fill in the values.
2. Keep `HYPERLIQUID_NETWORK=testnet` until you have validated end-to-end.
3. `cargo run`

## Usage
Paste a trading-setup card into the bot chat. It replies with a sized summary
and buttons: switch risk profile (Conservative/Moderate/Aggressive → leverage),
then **Confirm Limit** or **Confirm Market**, or **Cancel**. On confirmation it
sets leverage, places the entry, and arms a reduce-only SL + TP1 + TP2 bracket.

## LLM parsing
When `DEEPSEEK_API_KEY` is set, the bot uses DeepSeek as the primary card parser —
it calls the DeepSeek chat API (OpenAI-compatible) to extract structured fields from
the free-form trading-setup card. If the API key is unset, the network is
unreachable, or the response is invalid for any reason, the bot automatically falls
back to the built-in deterministic regex parser. Trade sizing, risk management, and
execution logic are always deterministic regardless of which parser is used.

## Configuration
See `.env.example`. Defaults: risk 1%/trade, leverage 2/3/5x, fill timeout 300s.
`MAX_DAILY_RISK_PCT` caps cumulative daily risk (% equity); confirmed trades exceeding it are skipped.

## Safety
- Uses an agent wallet, never your main wallet key.
- Only allowlisted Telegram user ids are served.
- Every execution requires explicit confirmation.

## Web signal ingest

Some signal sources are web-only and cannot send Telegram messages directly.
The bot exposes a local `POST /ingest` endpoint so a clipboard hotkey can pipe
signals in without breaking the confirmation flow.

**Setup:**
1. Add to `.env` (see `.env.example`):
   ```
   INGEST_PORT=8787
   INGEST_TOKEN=change-me-to-a-random-string
   ```
2. Run the bot as usual (`cargo run` or `make run`).
3. Copy a trading-setup card on the web, then run:
   ```
   make signal
   ```
   Or bind `scripts/send-signal.sh` to a macOS hotkey via Shortcuts, Raycast, or skhd.

**How it works:** the script reads the macOS clipboard (`pbpaste`) and POSTs it
to `http://127.0.0.1:$INGEST_PORT/ingest` with `X-Ingest-Token: $INGEST_TOKEN`.
The bot processes the signal through the exact same pipeline as a Telegram message
and sends the confirmation card(s) to your Telegram chat — confirmation and safety
are fully preserved; only the input channel changes.

**Security:** the endpoint binds `127.0.0.1` only (never reachable from the network)
and refuses to start if `INGEST_TOKEN` is not set. The token is never logged.
