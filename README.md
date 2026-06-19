# Agentic Hyperliquid

Telegram bot that turns a pasted trading-setup card (or screenshot) into a
long/short position with native SL/TP brackets on the Hyperliquid perpetuals DEX.

## Prerequisites
- Rust (stable, edition 2021)
- A Hyperliquid **API/agent wallet** private key, **approved under your master account** (see below)
- A Telegram bot token (from @BotFather) and your numeric Telegram user id
- When using an API/agent wallet, set `HYPERLIQUID_ACCOUNT_ADDRESS` to your master account address (the one that holds funds); the agent key only signs — it holds no balance of its own.
- If your account is in unified-account mode, set `HYPERLIQUID_UNIFIED_ACCOUNT=true` so the bot reads collateral from the spot USDC balance (the perp account reports 0 under unified mode).

### Approving the agent wallet (required before trading)
Hyperliquid will only accept orders signed by an agent wallet that has been
**authorized** under your master account. A locally generated keypair is not
enough — until you approve it, every order is rejected with:

```
exchange returned error: User or API Wallet 0x… does not exist
```

To authorize it:
1. Open <https://app.hyperliquid.xyz/API> and connect your **master** wallet
   (the one set in `HYPERLIQUID_ACCOUNT_ADDRESS`).
2. Generate/authorize an API wallet. The page returns a private key and registers
   the derived address as an approved agent on your master account.
3. Put that private key in `HYPERLIQUID_AGENT_KEY`.

Verify the agent is approved (returns the agent address, not `[]`):

```bash
curl -s -X POST https://api.hyperliquid.xyz/info -H "Content-Type: application/json" \
  -d '{"type":"extraAgents","user":"<YOUR_MASTER_ADDRESS>"}'
```

## Setup
1. `cp .env.example .env` and fill in the values.
2. Keep `HYPERLIQUID_NETWORK=testnet` until you have validated end-to-end.
3. `cargo run`

## Usage
Paste a trading-setup card into the bot chat. It replies with a sized summary
and buttons: switch risk profile (Conservative/Moderate/Aggressive → leverage),
then **Confirm Limit** or **Confirm Market**, or **Cancel**. On confirmation it
sets leverage, places the entry, and arms a reduce-only SL + TP1 + TP2 bracket.

While a trade executes the bot reports each step (entry submitted, fill, bracket
armed; partial-fill and timeout are flagged). A background monitor polls fill
history every `MONITOR_POLL_SECS` (default 30s) and messages you when a TP or SL
closes a position, naming the leg (TP1/TP2/SL) and the realized PnL.

Executions run in the background, so you can submit and confirm multiple setups
without waiting for a prior limit order to fill.

**Confirm Trigger** places a stop entry that rests until price crosses the entry level
(buy-stop for longs, sell-stop for shorts), then market-fills and the bot arms the SL/TP
bracket. An unfilled trigger is cancelled after `TRIGGER_EXPIRY_SECS` (default 4h, editable
via `/set trigger_expiry_secs`).

While any position is open the bot pushes a running-P&L summary (total + per-position
unrealized PnL) every `PNL_PUSH_SECS` (default 15min; `0` disables; editable via
`/set pnl_push_secs`). `/account` also shows a total uPnL line.

## LLM parsing
When `DEEPSEEK_API_KEY` is set, the bot uses DeepSeek as the primary card parser —
it calls the DeepSeek chat API (OpenAI-compatible) to extract structured fields from
the free-form trading-setup card. If the API key is unset, the network is
unreachable, or the response is invalid for any reason, the bot automatically falls
back to the built-in deterministic regex parser. Trade sizing, risk management, and
execution logic are always deterministic regardless of which parser is used.

Send a screenshot of a signal and the bot reads it via OpenAI vision (`OPENAI_VISION_MODEL`, default gpt-4o-mini); requires `OPENAI_API_KEY`. Text paste still works (DeepSeek + regex).

## Configuration
See `.env.example`. Defaults: risk 1%/trade, leverage 2/3/5x, fill timeout 300s.
`MAX_DAILY_RISK_PCT` caps cumulative daily risk (% equity); confirmed trades exceeding it are skipped.

## Deploy (k3s)

CI/CD builds the bot to GHCR and rolls it out to the production k3s cluster on
every push to `main` (`.github/workflows/deploy.yml`). It runs as a singleton
daemon (no inbound traffic). See [`k8s/README.md`](k8s/README.md) for one-time
cluster setup (secrets, image pull, PVC).

## Safety
- Uses an agent wallet, never your main wallet key.
- Only allowlisted Telegram user ids are served.
- Every execution requires explicit confirmation.

