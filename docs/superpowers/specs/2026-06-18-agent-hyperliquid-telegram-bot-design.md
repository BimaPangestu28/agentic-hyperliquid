# Agent Hyperliquid — Telegram Trading Bot (Design Spec)

**Date:** 2026-06-18
**Status:** Approved for planning
**Language/Stack:** Rust

## Overview

A Telegram bot that turns a pasted "Trading setup" card into an executed
long/short position on the Hyperliquid perpetuals DEX. The user pastes a card
(example below); the bot parses it, computes a risk-based position size, shows a
confirmation summary with inline buttons, and on confirmation places the entry
order plus native bracket orders (stop-loss + two take-profits) on Hyperliquid.

The bot is single-user (personal), gated by a Telegram user-id allowlist, and
defaults to Hyperliquid **testnet** until explicitly switched to mainnet.

### Example input

```
Trading setup for PENDLE
Direction: LONG
Timeframe: swing
Risk : Reward: 2.8 : 1
Confidence: 8/10
Thesis: ...
Conservative / Moderate / Aggressive
SL    $1.25   -10.7%
Entry $1.40
TP1   $1.70   +21.4%   60%
TP2   $2.00   +42.9%   40%
```

## Goals

- Parse a free-form trading-setup card into a structured `TradeSetup`.
- Compute position size from a fixed risk-per-trade (risk-based sizing).
- Require explicit confirmation before any real execution.
- Place entry (limit or market, chosen at confirmation) and native reduce-only
  bracket orders (SL 100%, TP1 60%, TP2 40%) on Hyperliquid.
- Be safe by default: testnet first, allowlist, no key leakage, no panics.

## Non-Goals (YAGNI)

- Multi-user / multi-account support.
- Auto-discovery of trade setups (the user always pastes the card).
- Bot-side price monitoring for SL/TP (brackets live natively on the exchange).
- Position management beyond initial placement (no trailing stops, no DCA) in v1.
- A web UI or dashboard.

## Decisions (from brainstorming)

| Topic | Decision |
|---|---|
| Execution flow | Parse → confirmation summary with inline buttons → execute |
| Network | Testnet first; mainnet via config |
| Position sizing | Risk-based (size from equity × risk% ÷ \|entry − SL\|) |
| Risk profile tabs | Map to **leverage** (risk amount stays constant) |
| Entry order type | Chosen at confirmation time (Limit / Market buttons) |
| SL/TP | Native reduce-only bracket orders on Hyperliquid |
| Stack | Rust + `teloxide` + `hyperliquid-rust-sdk` |

## Architecture

Single Rust binary, async (`tokio`). Modules:

| Module | Responsibility |
|---|---|
| `config.rs` | Load env: Telegram bot token, allowlist user ids, Hyperliquid agent-wallet key, network (testnet/mainnet), default risk %, leverage map, entry fill timeout, optional confidence gate |
| `parser.rs` | Card text → `TradeSetup` (coin, direction, entry, SL, TP1+alloc, TP2+alloc, R:R, confidence) |
| `sizing.rs` | Compute size, notional, margin, liquidation estimate; round size to `szDecimals`; validate constraints; produce `ExecutionPlan` |
| `hyperliquid/mod.rs` | Trait `Exchange` + SDK impl: fetch equity, asset meta (`szDecimals`, max leverage), set leverage, place limit/market entry, place trigger SL/TP. Trait boundary enables mocking in tests |
| `telegram.rs` | Message + callback handlers; renders confirmation; orchestrates execution |
| `state.rs` | In-memory map of pending confirmations keyed by user/message, holding the parsed setup + computed plan between message and button press |
| `journal.rs` | SQLite log of executed trades (timestamp, coin, direction, prices, size, order ids) |
| `main.rs` | Bootstrap: load config, build `Exchange`, start teloxide dispatcher |

### Data flow

1. User pastes card → `telegram.rs` receives text.
2. Allowlist check on Telegram user id; reject unknown users silently/with notice.
3. `parser.rs` → `TradeSetup`. On missing/ambiguous fields, reply listing what is
   missing; do not execute.
4. Fetch account equity + asset meta from Hyperliquid.
5. `sizing.rs` → `ExecutionPlan` (size, notional per profile, margin, est.
   liquidation price, rounded size).
6. Validate: margin available, size ≥ min order, leverage ≤ asset cap, SL on the
   correct side of entry for the direction, liquidation price not inside SL.
7. Reply with summary + inline keyboard:
   - Row 1: `[Conservative] [Moderate ✓] [Aggressive]` — switching recomputes the
     plan and edits the message in place.
   - Row 2: `[Confirm Limit] [Confirm Market]`
   - Row 3: `[Cancel]`
   Store pending plan in `state.rs`.
8. On `Confirm`: set leverage for the asset → place entry order (limit at entry,
   or market). For market, fill is immediate → place bracket. For limit, spawn a
   task that polls position/fill until filled or timeout; on fill place bracket,
   on timeout notify the user (no bracket placed).
9. Reply with execution result (order ids, fill price) or a clear error. Record
   in journal.

## Sizing math (risk-based)

```
risk_amount = equity × risk_pct            // default 1%
size_coins  = risk_amount / |entry − SL|
notional    = size_coins × entry
margin      = notional / leverage          // leverage from selected risk profile
```

- Leverage shifts liquidation distance and margin used; it does **not** change
  the risk amount. The bot warns if the estimated liquidation price is closer to
  entry than the SL for the chosen leverage.
- `size_coins` is rounded down to the asset's `szDecimals`; if rounding drops it
  below the minimum order size, reject with a clear message.

### Scale-out / bracket

- SL: reduce-only trigger at SL price, sized to 100% of position.
- TP1: reduce-only trigger at TP1 price, sized to TP1 allocation (e.g. 60%).
- TP2: reduce-only trigger at TP2 price, sized to TP2 allocation (e.g. 40%).
- Because SL is reduce-only and sized to the full position, after a TP fills it
  simply closes whatever remains; it cannot flip the position.

## Risk profile → leverage

Configurable defaults: **Conservative 2x · Moderate 3x · Aggressive 5x.**
The active tab is not reliably readable from the pasted text, so the bot defaults
to **Moderate** and lets the user change it via buttons before confirming.

## Configuration

Loaded from environment (`.env`, gitignored). Defaults in parentheses.

| Key | Meaning |
|---|---|
| `TELEGRAM_BOT_TOKEN` | Bot token |
| `TELEGRAM_ALLOWED_USER_IDS` | Comma-separated allowlist |
| `HYPERLIQUID_AGENT_KEY` | API/agent wallet private key (not main wallet) |
| `HYPERLIQUID_NETWORK` | `testnet` (default) / `mainnet` |
| `RISK_PCT` | Risk per trade (1.0) |
| `LEVERAGE_CONSERVATIVE` / `_MODERATE` / `_AGGRESSIVE` | (2 / 3 / 5) |
| `ENTRY_FILL_TIMEOUT_SECS` | Limit-entry fill wait (300) |
| `CONFIDENCE_GATE` | Optional min confidence to allow execution (off by default) |

## Security

- Use a Hyperliquid **API/agent wallet** key so the main wallet key never touches
  the bot.
- Secrets only via env; never logged. `.env` is gitignored.
- Telegram user-id allowlist; non-allowlisted messages are ignored.
- Mandatory confirmation step prevents accidental/fat-finger execution.
- Testnet default reduces blast radius during development.

## Error handling

- Every external call returns `Result`; handlers never panic (`anyhow` at the
  edge, `thiserror` for domain errors).
- Parse failure → reply with the specific missing/invalid fields.
- Insufficient margin / sub-minimum size / leverage over cap → reject with reason.
- Limit entry not filled within timeout → notify; do not place bracket.
- Network/SDK errors → reply with a concise message; log full detail via
  `tracing` (no secrets).

## Testing

- `parser.rs`: unit tests against the exact sample card, plus SHORT direction,
  alternate number formats, and missing-field cases.
- `sizing.rs`: deterministic unit tests for size/margin/liquidation given fixed
  equity, prices, and leverage.
- `hyperliquid`: behind the `Exchange` trait so execution orchestration is tested
  with a mock (no network).
- Manual end-to-end validation on Hyperliquid testnet.

## Project layout

```
agent-hyperliquid/
  Cargo.toml  .env.example  .gitignore  README.md
  src/{main,config,parser,sizing,telegram,state,journal}.rs
  src/hyperliquid/mod.rs
  tests/{parser_tests,sizing_tests}.rs
```
