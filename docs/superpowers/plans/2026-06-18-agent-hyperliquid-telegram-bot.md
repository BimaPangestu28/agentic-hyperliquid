# Agent Hyperliquid Telegram Bot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust Telegram bot that parses a pasted trading-setup card and, after explicit confirmation, executes a long/short position with native SL/TP brackets on the Hyperliquid perpetuals DEX.

**Architecture:** Single async Rust binary. Pure-logic modules (`parser`, `sizing`, `state`, render helpers) are unit-tested in isolation. All Hyperliquid network access sits behind an `Exchange` trait so orchestration is testable with a mock; the real SDK impl is the only network-touching code. Telegram I/O via `teloxide`.

**Tech Stack:** Rust (edition 2021), `tokio`, `teloxide`, `hyperliquid-rust-sdk`, `async-trait`, `serde`, `rusqlite`, `anyhow`, `thiserror`, `tracing`.

## Global Constraints

- Language: Rust, edition 2021. Single binary crate named `agent-hyperliquid`.
- Network default: Hyperliquid **testnet**; mainnet only via `HYPERLIQUID_NETWORK=mainnet`.
- Default risk per trade: **1.0%** of equity. Default leverage map: Conservative **2x**, Moderate **3x**, Aggressive **5x**. Default risk profile when unspecified: **Moderate**.
- Minimum order notional: **$10** (Hyperliquid floor).
- Position sizing is risk-based; leverage never changes the risk amount.
- Secrets only via env / `.env` (gitignored); never logged.
- Telegram access gated by a user-id allowlist.
- Handlers never panic: domain errors via `thiserror`, edges via `anyhow`.
- All money/price math uses `f64`; sizes rounded **down** to the asset's `szDecimals`.

---

### Task 1: Project scaffold + configuration

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs` (temporary stub, replaced in Task 8)
- Create: `src/config.rs`
- Create: `.env.example`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum Network { Testnet, Mainnet }`
  - `pub struct LeverageMap { pub conservative: u32, pub moderate: u32, pub aggressive: u32 }`
  - `pub struct Config { pub telegram_token: String, pub allowed_user_ids: Vec<i64>, pub agent_key: String, pub network: Network, pub risk_pct: f64, pub leverage: LeverageMap, pub entry_fill_timeout_secs: u64, pub confidence_gate: Option<u8> }`
  - `pub fn from_env() -> anyhow::Result<Config>`
  - `impl Config { pub fn is_allowed(&self, user_id: i64) -> bool }`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "agent-hyperliquid"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time", "sync"] }
teloxide = { version = "0.13", features = ["macros"] }
hyperliquid-rust-sdk = "0.6"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
rusqlite = { version = "0.32", features = ["bundled"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
dotenvy = "0.15"

[dev-dependencies]
# (unit tests use std only)
```

- [ ] **Step 2: Write the failing test for config parsing**

Create `src/config.rs`:

```rust
//! Loads runtime configuration from environment variables.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Testnet,
    Mainnet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeverageMap {
    pub conservative: u32,
    pub moderate: u32,
    pub aggressive: u32,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub telegram_token: String,
    pub allowed_user_ids: Vec<i64>,
    pub agent_key: String,
    pub network: Network,
    pub risk_pct: f64,
    pub leverage: LeverageMap,
    pub entry_fill_timeout_secs: u64,
    pub confidence_gate: Option<u8>,
}

impl Config {
    pub fn is_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.contains(&user_id)
    }
}

fn parse_user_ids(raw: &str) -> Vec<i64> {
    raw.split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .filter_map(|segment| segment.parse::<i64>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_user_ids_ignoring_blanks() {
        assert_eq!(parse_user_ids("111, 222 ,, 333"), vec![111, 222, 333]);
    }

    #[test]
    fn allowlist_membership_check() {
        let config = Config {
            telegram_token: "t".into(),
            allowed_user_ids: vec![42],
            agent_key: "k".into(),
            network: Network::Testnet,
            risk_pct: 1.0,
            leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
            entry_fill_timeout_secs: 300,
            confidence_gate: None,
        };
        assert!(config.is_allowed(42));
        assert!(!config.is_allowed(99));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass (logic-only, no env yet)**

Run: `cargo test --lib config`
Expected: 2 passed.

- [ ] **Step 4: Add `from_env` with defaults**

Append to `src/config.rs`:

```rust
fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn from_env() -> anyhow::Result<Config> {
    let telegram_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .map_err(|_| anyhow::anyhow!("TELEGRAM_BOT_TOKEN is required"))?;
    let agent_key = std::env::var("HYPERLIQUID_AGENT_KEY")
        .map_err(|_| anyhow::anyhow!("HYPERLIQUID_AGENT_KEY is required"))?;
    let allowed_user_ids = parse_user_ids(
        &std::env::var("TELEGRAM_ALLOWED_USER_IDS").unwrap_or_default(),
    );
    let network = match std::env::var("HYPERLIQUID_NETWORK").as_deref() {
        Ok("mainnet") => Network::Mainnet,
        _ => Network::Testnet,
    };
    let confidence_gate = std::env::var("CONFIDENCE_GATE")
        .ok()
        .and_then(|v| v.parse::<u8>().ok());

    Ok(Config {
        telegram_token,
        allowed_user_ids,
        agent_key,
        network,
        risk_pct: env_or("RISK_PCT", 1.0_f64),
        leverage: LeverageMap {
            conservative: env_or("LEVERAGE_CONSERVATIVE", 2),
            moderate: env_or("LEVERAGE_MODERATE", 3),
            aggressive: env_or("LEVERAGE_AGGRESSIVE", 5),
        },
        entry_fill_timeout_secs: env_or("ENTRY_FILL_TIMEOUT_SECS", 300),
        confidence_gate,
    })
}
```

- [ ] **Step 5: Create temporary `src/main.rs` stub so the crate builds**

```rust
mod config;

fn main() {
    println!("agent-hyperliquid scaffold");
}
```

- [ ] **Step 6: Create `.env.example`**

```bash
TELEGRAM_BOT_TOKEN=
TELEGRAM_ALLOWED_USER_IDS=123456789
HYPERLIQUID_AGENT_KEY=
HYPERLIQUID_NETWORK=testnet
RISK_PCT=1.0
LEVERAGE_CONSERVATIVE=2
LEVERAGE_MODERATE=3
LEVERAGE_AGGRESSIVE=5
ENTRY_FILL_TIMEOUT_SECS=300
# CONFIDENCE_GATE=6
```

- [ ] **Step 7: Build and commit**

Run: `cargo build && cargo test --lib config`
Expected: build succeeds, 2 tests pass.

```bash
git add Cargo.toml Cargo.lock src/main.rs src/config.rs .env.example
git commit -m "feat: scaffold crate and configuration loading"
```

---

### Task 2: Trading card parser

**Files:**
- Create: `src/parser.rs`
- Modify: `src/main.rs` (add `mod parser;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum Direction { Long, Short }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct TakeProfit { pub price: f64, pub allocation_pct: f64 }`
  - `pub struct TradeSetup { pub coin: String, pub direction: Direction, pub timeframe: Option<String>, pub risk_reward: Option<f64>, pub confidence: Option<u8>, pub entry: f64, pub stop_loss: f64, pub take_profits: Vec<TakeProfit> }`
  - `pub enum ParseError` (`thiserror`): `MissingFields(String)`, `InvalidValue(String)`
  - `pub fn parse_setup(text: &str) -> Result<TradeSetup, ParseError>`

- [ ] **Step 1: Write the failing test using the exact sample card**

Create `src/parser.rs`:

```rust
//! Parses a free-form "Trading setup" card into a structured `TradeSetup`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Long,
    Short,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TakeProfit {
    pub price: f64,
    pub allocation_pct: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TradeSetup {
    pub coin: String,
    pub direction: Direction,
    pub timeframe: Option<String>,
    pub risk_reward: Option<f64>,
    pub confidence: Option<u8>,
    pub entry: f64,
    pub stop_loss: f64,
    pub take_profits: Vec<TakeProfit>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("missing required fields: {0}")]
    MissingFields(String),
    #[error("invalid value: {0}")]
    InvalidValue(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Trading setup for PENDLE
Direction
LONG
Timeframe
swing
Risk : Reward
2.8 : 1
Confidence
8/10
Thesis
Pendle just went net deflationary.
Conservative
Moderate
Aggressive
SL
$1.25
-10.7%
Entry
$1.40
TP1
$1.70
+21.4%
60%
TP2
$2.00
+42.9%
40%";

    #[test]
    fn parses_full_sample_card() {
        let setup = parse_setup(SAMPLE).expect("should parse");
        assert_eq!(setup.coin, "PENDLE");
        assert_eq!(setup.direction, Direction::Long);
        assert_eq!(setup.timeframe.as_deref(), Some("swing"));
        assert_eq!(setup.confidence, Some(8));
        assert_eq!(setup.entry, 1.40);
        assert_eq!(setup.stop_loss, 1.25);
        assert_eq!(setup.take_profits.len(), 2);
        assert_eq!(setup.take_profits[0], TakeProfit { price: 1.70, allocation_pct: 60.0 });
        assert_eq!(setup.take_profits[1], TakeProfit { price: 2.00, allocation_pct: 40.0 });
    }

    #[test]
    fn parses_short_direction() {
        let text = "Trading setup for BTC\nDirection\nSHORT\nSL\n$70000\nEntry\n$68000\nTP1\n$64000\n100%";
        let setup = parse_setup(text).expect("should parse");
        assert_eq!(setup.direction, Direction::Short);
        assert_eq!(setup.take_profits[0].allocation_pct, 100.0);
    }

    #[test]
    fn reports_missing_entry() {
        let text = "Trading setup for BTC\nDirection\nLONG\nSL\n$70000\nTP1\n$80000\n100%";
        let err = parse_setup(text).unwrap_err();
        assert_eq!(err, ParseError::MissingFields("entry".into()));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib parser`
Expected: FAIL — `cannot find function parse_setup`.

- [ ] **Step 3: Implement `parse_setup`**

Insert above the `#[cfg(test)]` block in `src/parser.rs`:

```rust
/// Strips `$`, `,`, `+`, `%` and surrounding whitespace, then parses a float.
fn parse_money(token: &str) -> Option<f64> {
    let cleaned: String = token
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    cleaned.parse::<f64>().ok()
}

fn find_value_after<'a>(lines: &'a [&'a str], label: &str) -> Option<&'a str> {
    lines
        .iter()
        .position(|line| line.trim().eq_ignore_ascii_case(label))
        .and_then(|index| lines.get(index + 1))
        .map(|line| line.trim())
}

/// Parses a "Trading setup for X" card. Lines are label/value pairs; price
/// lines look like `$1.40`, allocation lines like `60%`.
pub fn parse_setup(text: &str) -> Result<TradeSetup, ParseError> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

    let coin = lines
        .iter()
        .find_map(|line| line.strip_prefix("Trading setup for "))
        .map(|c| c.trim().to_string())
        .ok_or_else(|| ParseError::MissingFields("coin".into()))?;

    let direction = match find_value_after(&lines, "Direction").map(str::to_ascii_uppercase).as_deref() {
        Some("LONG") => Direction::Long,
        Some("SHORT") => Direction::Short,
        _ => return Err(ParseError::MissingFields("direction".into())),
    };

    let timeframe = find_value_after(&lines, "Timeframe").map(str::to_string);

    let risk_reward = find_value_after(&lines, "Risk : Reward")
        .and_then(|v| v.split(':').next())
        .and_then(|v| v.trim().parse::<f64>().ok());

    let confidence = find_value_after(&lines, "Confidence")
        .and_then(|v| v.split('/').next())
        .and_then(|v| v.trim().parse::<u8>().ok());

    let stop_loss = find_value_after(&lines, "SL")
        .and_then(parse_money)
        .ok_or_else(|| ParseError::MissingFields("stop_loss".into()))?;

    let entry = find_value_after(&lines, "Entry")
        .and_then(parse_money)
        .ok_or_else(|| ParseError::MissingFields("entry".into()))?;

    // Take-profits: for each TPn label, the next price line is the price; the
    // first subsequent line ending in `%` that is not a +/- price-change is the
    // allocation. We treat the LAST `%` line before the next TP/end as allocation.
    let mut take_profits = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let label = line.trim();
        if !(label.len() >= 3 && label.to_ascii_uppercase().starts_with("TP") && label[2..].chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let price = lines.get(index + 1).and_then(|l| parse_money(l))
            .ok_or_else(|| ParseError::InvalidValue(format!("{label} price")))?;
        // Scan following lines until the next TP label or end for an allocation %.
        let mut allocation_pct = 100.0;
        for follow in &lines[index + 1..] {
            let f = follow.trim();
            if f.to_ascii_uppercase().starts_with("TP") && f.len() >= 3 && f[2..].chars().all(|c| c.is_ascii_digit()) {
                break;
            }
            // Allocation lines have no sign; price-change lines start with + or -.
            if f.ends_with('%') && !f.starts_with('+') && !f.starts_with('-') {
                if let Some(value) = parse_money(f) {
                    allocation_pct = value;
                }
            }
        }
        take_profits.push(TakeProfit { price, allocation_pct });
    }

    if take_profits.is_empty() {
        return Err(ParseError::MissingFields("take_profits".into()));
    }

    Ok(TradeSetup { coin, direction, timeframe, risk_reward, confidence, entry, stop_loss, take_profits })
}
```

- [ ] **Step 4: Add `mod parser;` to `src/main.rs`**

```rust
mod config;
mod parser;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib parser`
Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add src/parser.rs src/main.rs
git commit -m "feat: parse trading-setup card into TradeSetup"
```

---

### Task 3: Risk-based sizing

**Files:**
- Create: `src/sizing.rs`
- Modify: `src/main.rs` (add `mod sizing;`)

**Interfaces:**
- Consumes: `parser::{TradeSetup, Direction, TakeProfit}`, `config::LeverageMap`.
- Produces:
  - `pub enum RiskProfile { Conservative, Moderate, Aggressive }`
  - `pub struct AssetMeta { pub sz_decimals: u32, pub max_leverage: u32 }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct SizingInput<'a> { pub setup: &'a TradeSetup, pub equity: f64, pub risk_pct: f64, pub profile: RiskProfile, pub leverage: &'a LeverageMap, pub asset_meta: &'a AssetMeta }`
  - `pub struct BracketLeg { pub price: f64, pub size: f64 }`
  - `pub struct ExecutionPlan { pub coin: String, pub direction: Direction, pub size: f64, pub entry: f64, pub leverage: u32, pub notional: f64, pub margin: f64, pub risk_amount: f64, pub liquidation_price: f64, pub stop_loss: BracketLeg, pub take_profits: Vec<BracketLeg>, pub warnings: Vec<String> }`
  - `pub enum SizingError` (`thiserror`): `InvalidStopSide`, `BelowMinSize(f64)`, `LeverageTooHigh { requested: u32, cap: u32 }`, `ZeroStopDistance`
  - `impl LeverageMap { pub fn leverage_for(&self, profile: RiskProfile) -> u32 }`
  - `pub fn build_plan(input: &SizingInput) -> Result<ExecutionPlan, SizingError>`
  - `pub const MIN_ORDER_NOTIONAL: f64 = 10.0;`

- [ ] **Step 1: Write the failing tests**

Create `src/sizing.rs`:

```rust
//! Risk-based position sizing and bracket construction.

use crate::config::LeverageMap;
use crate::parser::{Direction, TradeSetup};

pub const MIN_ORDER_NOTIONAL: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskProfile {
    Conservative,
    Moderate,
    Aggressive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetMeta {
    pub sz_decimals: u32,
    pub max_leverage: u32,
}

pub struct SizingInput<'a> {
    pub setup: &'a TradeSetup,
    pub equity: f64,
    pub risk_pct: f64,
    pub profile: RiskProfile,
    pub leverage: &'a LeverageMap,
    pub asset_meta: &'a AssetMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BracketLeg {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPlan {
    pub coin: String,
    pub direction: Direction,
    pub size: f64,
    pub entry: f64,
    pub leverage: u32,
    pub notional: f64,
    pub margin: f64,
    pub risk_amount: f64,
    pub liquidation_price: f64,
    pub stop_loss: BracketLeg,
    pub take_profits: Vec<BracketLeg>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SizingError {
    #[error("stop-loss is on the wrong side of entry for this direction")]
    InvalidStopSide,
    #[error("entry equals stop-loss; risk distance is zero")]
    ZeroStopDistance,
    #[error("order notional below ${0} minimum")]
    BelowMinSize(f64),
    #[error("leverage {requested}x exceeds asset cap {cap}x")]
    LeverageTooHigh { requested: u32, cap: u32 },
}

impl LeverageMap {
    pub fn leverage_for(&self, profile: RiskProfile) -> u32 {
        match profile {
            RiskProfile::Conservative => self.conservative,
            RiskProfile::Moderate => self.moderate,
            RiskProfile::Aggressive => self.aggressive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::TakeProfit;

    fn pendle_setup() -> TradeSetup {
        TradeSetup {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            timeframe: Some("swing".into()),
            risk_reward: Some(2.8),
            confidence: Some(8),
            entry: 1.40,
            stop_loss: 1.25,
            take_profits: vec![
                TakeProfit { price: 1.70, allocation_pct: 60.0 },
                TakeProfit { price: 2.00, allocation_pct: 40.0 },
            ],
        }
    }

    fn leverage() -> LeverageMap {
        LeverageMap { conservative: 2, moderate: 3, aggressive: 5 }
    }

    #[test]
    fn computes_risk_based_size_and_brackets() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        let plan = build_plan(&input).expect("should size");

        // risk = 100; distance = 0.15; raw size = 666.66.. -> floor to 1 decimal = 666.6
        assert_eq!(plan.risk_amount, 100.0);
        assert_eq!(plan.size, 666.6);
        assert_eq!(plan.leverage, 3);
        assert_eq!(plan.stop_loss.size, 666.6);
        // TP1 60% of 666.6 = 399.96 -> floor 1 decimal = 399.9
        assert_eq!(plan.take_profits[0].size, 399.9);
        // TP2 40% = 266.64 -> 266.6
        assert_eq!(plan.take_profits[1].size, 266.6);
    }

    #[test]
    fn rejects_stop_on_wrong_side_for_long() {
        let mut setup = pendle_setup();
        setup.stop_loss = 1.50; // above entry for a long
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, profile: RiskProfile::Moderate, leverage: &leverage(), asset_meta: &meta };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::InvalidStopSide);
    }

    #[test]
    fn rejects_leverage_over_asset_cap() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 2 };
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, profile: RiskProfile::Aggressive, leverage: &leverage(), asset_meta: &meta };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::LeverageTooHigh { requested: 5, cap: 2 });
    }

    #[test]
    fn rejects_below_minimum_notional() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput { setup: &setup, equity: 1.0, risk_pct: 1.0, profile: RiskProfile::Moderate, leverage: &leverage(), asset_meta: &meta };
        // risk = 0.01, size ~0.066 -> floor 0.0 -> below min
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }

    #[test]
    fn warns_when_liquidation_is_tighter_than_stop() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 50 };
        // Conservative=2x: liq_long ~= entry*(1-1/2)=0.70, far below SL 1.25 -> no warn.
        // Force high leverage profile to push liq above SL.
        let high = LeverageMap { conservative: 2, moderate: 3, aggressive: 20 };
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, profile: RiskProfile::Aggressive, leverage: &high, asset_meta: &meta };
        let plan = build_plan(&input).unwrap();
        // liq_long = 1.40*(1-1/20)=1.33 > SL 1.25 -> warning present.
        assert!(plan.warnings.iter().any(|w| w.contains("liquidation")));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib sizing`
Expected: FAIL — `cannot find function build_plan`.

- [ ] **Step 3: Implement `build_plan`**

Insert above the `#[cfg(test)]` block in `src/sizing.rs`:

```rust
/// Rounds `value` DOWN to `decimals` places.
fn floor_to(value: f64, decimals: u32) -> f64 {
    let factor = 10_f64.powi(decimals as i32);
    (value * factor).floor() / factor
}

/// Approximate isolated-margin liquidation price (ignores maintenance margin;
/// used only for a proximity warning, never for sizing).
fn estimate_liquidation(direction: Direction, entry: f64, leverage: u32) -> f64 {
    let factor = 1.0 / leverage as f64;
    match direction {
        Direction::Long => entry * (1.0 - factor),
        Direction::Short => entry * (1.0 + factor),
    }
}

pub fn build_plan(input: &SizingInput) -> Result<ExecutionPlan, SizingError> {
    let setup = input.setup;
    let leverage = input.leverage.leverage_for(input.profile);
    if leverage > input.asset_meta.max_leverage {
        return Err(SizingError::LeverageTooHigh { requested: leverage, cap: input.asset_meta.max_leverage });
    }

    // Validate stop side.
    match setup.direction {
        Direction::Long if setup.stop_loss >= setup.entry => return Err(SizingError::InvalidStopSide),
        Direction::Short if setup.stop_loss <= setup.entry => return Err(SizingError::InvalidStopSide),
        _ => {}
    }

    let stop_distance = (setup.entry - setup.stop_loss).abs();
    if stop_distance == 0.0 {
        return Err(SizingError::ZeroStopDistance);
    }

    let risk_amount = input.equity * (input.risk_pct / 100.0);
    let raw_size = risk_amount / stop_distance;
    let size = floor_to(raw_size, input.asset_meta.sz_decimals);
    let notional = size * setup.entry;
    if notional < MIN_ORDER_NOTIONAL {
        return Err(SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }

    let margin = notional / leverage as f64;
    let liquidation_price = estimate_liquidation(setup.direction, setup.entry, leverage);

    let take_profits = setup
        .take_profits
        .iter()
        .map(|tp| BracketLeg {
            price: tp.price,
            size: floor_to(size * (tp.allocation_pct / 100.0), input.asset_meta.sz_decimals),
        })
        .collect();

    let mut warnings = Vec::new();
    let liq_breaches_stop = match setup.direction {
        Direction::Long => liquidation_price >= setup.stop_loss,
        Direction::Short => liquidation_price <= setup.stop_loss,
    };
    if liq_breaches_stop {
        warnings.push(format!(
            "estimated liquidation {:.4} is tighter than stop-loss {:.4}; lower the leverage",
            liquidation_price, setup.stop_loss
        ));
    }

    Ok(ExecutionPlan {
        coin: setup.coin.clone(),
        direction: setup.direction,
        size,
        entry: setup.entry,
        leverage,
        notional,
        margin,
        risk_amount,
        liquidation_price,
        stop_loss: BracketLeg { price: setup.stop_loss, size },
        take_profits,
        warnings,
    })
}
```

- [ ] **Step 4: Add `mod sizing;` to `src/main.rs`**

```rust
mod config;
mod parser;
mod sizing;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib sizing`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add src/sizing.rs src/main.rs
git commit -m "feat: risk-based sizing with bracket legs and liquidation warning"
```

---

### Task 4: Exchange abstraction + mock + SDK implementation

**Files:**
- Create: `src/hyperliquid/mod.rs`
- Modify: `src/main.rs` (add `mod hyperliquid;`)

**Interfaces:**
- Consumes: `sizing::AssetMeta`, `parser::Direction`, `config::{Config, Network}`.
- Produces:
  - `pub struct EntryOrder { pub coin: String, pub is_buy: bool, pub size: f64, pub limit_price: Option<f64> }` (`limit_price: None` = market)
  - `pub struct TriggerOrder { pub coin: String, pub is_buy: bool, pub size: f64, pub trigger_price: f64, pub is_take_profit: bool }`
  - `pub struct OrderResult { pub order_id: Option<u64>, pub filled: bool, pub avg_price: Option<f64> }`
  - `#[async_trait] pub trait Exchange: Send + Sync { async fn equity(&self) -> anyhow::Result<f64>; async fn asset_meta(&self, coin: &str) -> anyhow::Result<AssetMeta>; async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()>; async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult>; async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult>; async fn position_size(&self, coin: &str) -> anyhow::Result<f64>; }`
  - `pub struct HyperliquidExchange` with `pub async fn connect(config: &Config) -> anyhow::Result<Self>`
  - (test-only) `pub struct MockExchange` recording calls

- [ ] **Step 1: Write the failing test for the mock against the trait**

Create `src/hyperliquid/mod.rs`:

```rust
//! Hyperliquid access behind an `Exchange` trait so orchestration is testable
//! without network. `HyperliquidExchange` is the only network-touching code.

use crate::sizing::AssetMeta;
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq)]
pub struct EntryOrder {
    pub coin: String,
    pub is_buy: bool,
    pub size: f64,
    /// `None` means market order.
    pub limit_price: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TriggerOrder {
    pub coin: String,
    pub is_buy: bool,
    pub size: f64,
    pub trigger_price: f64,
    pub is_take_profit: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderResult {
    pub order_id: Option<u64>,
    pub filled: bool,
    pub avg_price: Option<f64>,
}

#[async_trait]
pub trait Exchange: Send + Sync {
    async fn equity(&self) -> anyhow::Result<f64>;
    async fn asset_meta(&self, coin: &str) -> anyhow::Result<AssetMeta>;
    async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()>;
    async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult>;
    async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult>;
    async fn position_size(&self, coin: &str) -> anyhow::Result<f64>;
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockExchange {
        pub equity: f64,
        pub meta: Option<AssetMeta>,
        pub entries: Mutex<Vec<EntryOrder>>,
        pub triggers: Mutex<Vec<TriggerOrder>>,
        pub leverage_calls: Mutex<Vec<(String, u32)>>,
    }

    #[async_trait]
    impl Exchange for MockExchange {
        async fn equity(&self) -> anyhow::Result<f64> {
            Ok(self.equity)
        }
        async fn asset_meta(&self, _coin: &str) -> anyhow::Result<AssetMeta> {
            self.meta.ok_or_else(|| anyhow::anyhow!("no meta configured"))
        }
        async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()> {
            self.leverage_calls.lock().unwrap().push((coin.to_string(), leverage));
            Ok(())
        }
        async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult> {
            self.entries.lock().unwrap().push(order.clone());
            Ok(OrderResult { order_id: Some(1), filled: order.limit_price.is_none(), avg_price: Some(order.limit_price.unwrap_or(1.40)) })
        }
        async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult> {
            self.triggers.lock().unwrap().push(order.clone());
            Ok(OrderResult { order_id: Some(2), filled: false, avg_price: None })
        }
        async fn position_size(&self, _coin: &str) -> anyhow::Result<f64> {
            Ok(self.entries.lock().unwrap().iter().map(|e| e.size).sum())
        }
    }

    #[tokio::test]
    async fn mock_records_entry_and_trigger_orders() {
        let exchange = MockExchange { equity: 5000.0, meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }), ..Default::default() };
        let entry = EntryOrder { coin: "PENDLE".into(), is_buy: true, size: 10.0, limit_price: None };
        let result = exchange.place_entry(&entry).await.unwrap();
        assert!(result.filled);
        assert_eq!(exchange.entries.lock().unwrap().len(), 1);
        assert_eq!(exchange.position_size("PENDLE").await.unwrap(), 10.0);
    }
}
```

- [ ] **Step 2: Add `mod hyperliquid;` to `src/main.rs` and run the mock test**

```rust
mod config;
mod parser;
mod sizing;
mod hyperliquid;
```

Run: `cargo test --lib hyperliquid`
Expected: 1 passed (`mock_records_entry_and_trigger_orders`).

- [ ] **Step 3: Implement `HyperliquidExchange` against the SDK**

Append to `src/hyperliquid/mod.rs`. Pin the SDK version in `Cargo.toml` to the one you install (`cargo add hyperliquid-rust-sdk`) and adapt these calls to that version's exact API — the trait above is the contract the rest of the bot depends on, so only this struct changes if the SDK differs.

```rust
use crate::config::{Config, Network};
use hyperliquid_rust_sdk::{
    BaseUrl, ClientLimit, ClientOrder, ClientOrderRequest, ClientTrigger,
    ExchangeClient, InfoClient,
};
use ethers::signers::LocalWallet;
use std::str::FromStr;

pub struct HyperliquidExchange {
    info: InfoClient,
    exchange: ExchangeClient,
    address: ethers::types::H160,
}

impl HyperliquidExchange {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let base = match config.network {
            Network::Testnet => BaseUrl::Testnet,
            Network::Mainnet => BaseUrl::Mainnet,
        };
        let wallet = LocalWallet::from_str(&config.agent_key)?;
        let address = ethers::signers::Signer::address(&wallet);
        let info = InfoClient::new(None, Some(base)).await?;
        let exchange = ExchangeClient::new(None, wallet, Some(base), None, None).await?;
        Ok(Self { info, exchange, address })
    }
}

#[async_trait]
impl Exchange for HyperliquidExchange {
    async fn equity(&self) -> anyhow::Result<f64> {
        let state = self.info.user_state(self.address).await?;
        Ok(state.margin_summary.account_value.parse::<f64>()?)
    }

    async fn asset_meta(&self, coin: &str) -> anyhow::Result<AssetMeta> {
        let meta = self.info.meta().await?;
        let asset = meta
            .universe
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(coin))
            .ok_or_else(|| anyhow::anyhow!("unknown asset {coin}"))?;
        Ok(AssetMeta {
            sz_decimals: asset.sz_decimals as u32,
            max_leverage: asset.max_leverage as u32,
        })
    }

    async fn set_leverage(&self, coin: &str, leverage: u32) -> anyhow::Result<()> {
        self.exchange.update_leverage(leverage, coin, false, None).await?;
        Ok(())
    }

    async fn place_entry(&self, order: &EntryOrder) -> anyhow::Result<OrderResult> {
        // Market = aggressive IOC limit far through the book; limit = GTC at price.
        let request = ClientOrderRequest {
            asset: order.coin.clone(),
            is_buy: order.is_buy,
            reduce_only: false,
            limit_px: order.limit_price.unwrap_or_else(|| if order.is_buy { f64::MAX } else { 0.0 }),
            sz: order.size,
            cloid: None,
            order_type: match order.limit_price {
                Some(_) => ClientOrder::Limit(ClientLimit { tif: "Gtc".to_string() }),
                None => ClientOrder::Limit(ClientLimit { tif: "Ioc".to_string() }),
            },
        };
        let _response = self.exchange.order(request, None).await?;
        Ok(OrderResult { order_id: None, filled: order.limit_price.is_none(), avg_price: order.limit_price })
    }

    async fn place_trigger(&self, order: &TriggerOrder) -> anyhow::Result<OrderResult> {
        let request = ClientOrderRequest {
            asset: order.coin.clone(),
            is_buy: order.is_buy,
            reduce_only: true,
            limit_px: order.trigger_price,
            sz: order.size,
            cloid: None,
            order_type: ClientOrder::Trigger(ClientTrigger {
                trigger_px: order.trigger_price,
                is_market: true,
                tpsl: if order.is_take_profit { "tp".to_string() } else { "sl".to_string() },
            }),
        };
        let _response = self.exchange.order(request, None).await?;
        Ok(OrderResult { order_id: None, filled: false, avg_price: None })
    }

    async fn position_size(&self, coin: &str) -> anyhow::Result<f64> {
        let state = self.info.user_state(self.address).await?;
        let size = state
            .asset_positions
            .iter()
            .find(|p| p.position.coin.eq_ignore_ascii_case(coin))
            .map(|p| p.position.szi.parse::<f64>().unwrap_or(0.0))
            .unwrap_or(0.0);
        Ok(size.abs())
    }
}
```

Add the SDK's transitive signer dep:

```bash
cargo add ethers --no-default-features --features signers
```

- [ ] **Step 4: Build (no network test for the real client)**

Run: `cargo build && cargo test --lib hyperliquid`
Expected: build succeeds; 1 mock test passes. Adjust SDK field/method names if `cargo build` reports mismatches (the trait surface stays fixed).

- [ ] **Step 5: Commit**

```bash
git add src/hyperliquid/mod.rs src/main.rs Cargo.toml Cargo.lock
git commit -m "feat: Exchange trait with mock and Hyperliquid SDK implementation"
```

---

### Task 5: Pending-confirmation state store

**Files:**
- Create: `src/state.rs`
- Modify: `src/main.rs` (add `mod state;`)

**Interfaces:**
- Consumes: `parser::TradeSetup`, `sizing::{ExecutionPlan, AssetMeta, RiskProfile}`.
- Produces:
  - `pub struct PendingTrade { pub setup: TradeSetup, pub equity: f64, pub asset_meta: AssetMeta, pub profile: RiskProfile, pub plan: ExecutionPlan }`
  - `pub struct PendingStore` with `pub fn new() -> Self`, `pub fn insert(&self, key: i64, trade: PendingTrade)`, `pub fn get(&self, key: i64) -> Option<PendingTrade>`, `pub fn remove(&self, key: i64) -> Option<PendingTrade>`
  - `PendingTrade` derives `Clone`.

- [ ] **Step 1: Write the failing test**

Create `src/state.rs`:

```rust
//! In-memory store holding a parsed setup + computed plan between the
//! confirmation message and the user's button press. Keyed by message id.

use crate::parser::TradeSetup;
use crate::sizing::{AssetMeta, ExecutionPlan, RiskProfile};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct PendingTrade {
    pub setup: TradeSetup,
    pub equity: f64,
    pub asset_meta: AssetMeta,
    pub profile: RiskProfile,
    pub plan: ExecutionPlan,
}

pub struct PendingStore {
    inner: Mutex<HashMap<i64, PendingTrade>>,
}

impl PendingStore {
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    pub fn insert(&self, key: i64, trade: PendingTrade) {
        self.inner.lock().unwrap().insert(key, trade);
    }

    pub fn get(&self, key: i64) -> Option<PendingTrade> {
        self.inner.lock().unwrap().get(&key).cloned()
    }

    pub fn remove(&self, key: i64) -> Option<PendingTrade> {
        self.inner.lock().unwrap().remove(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Direction, TakeProfit};
    use crate::sizing::BracketLeg;

    fn sample_trade() -> PendingTrade {
        let setup = TradeSetup {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            timeframe: None,
            risk_reward: None,
            confidence: None,
            entry: 1.40,
            stop_loss: 1.25,
            take_profits: vec![TakeProfit { price: 1.70, allocation_pct: 100.0 }],
        };
        let plan = ExecutionPlan {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            size: 100.0,
            entry: 1.40,
            leverage: 3,
            notional: 140.0,
            margin: 46.6,
            risk_amount: 100.0,
            liquidation_price: 0.93,
            stop_loss: BracketLeg { price: 1.25, size: 100.0 },
            take_profits: vec![BracketLeg { price: 1.70, size: 100.0 }],
            warnings: vec![],
        };
        PendingTrade { setup, equity: 10_000.0, asset_meta: AssetMeta { sz_decimals: 1, max_leverage: 10 }, profile: RiskProfile::Moderate, plan }
    }

    #[test]
    fn insert_get_remove_roundtrip() {
        let store = PendingStore::new();
        store.insert(7, sample_trade());
        assert_eq!(store.get(7).unwrap().plan.size, 100.0);
        assert!(store.remove(7).is_some());
        assert!(store.get(7).is_none());
    }
}
```

- [ ] **Step 2: Add `mod state;` to `src/main.rs` and run the test**

Run: `cargo test --lib state`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add src/state.rs src/main.rs
git commit -m "feat: in-memory pending-confirmation store"
```

---

### Task 6: Trade journal (SQLite)

**Files:**
- Create: `src/journal.rs`
- Modify: `src/main.rs` (add `mod journal;`)

**Interfaces:**
- Consumes: `sizing::ExecutionPlan`, `parser::Direction`.
- Produces:
  - `pub struct Journal` with `pub fn open(path: &str) -> anyhow::Result<Self>` and `pub fn record(&self, plan: &ExecutionPlan, entry_order_id: Option<u64>) -> anyhow::Result<()>`
  - `pub fn open_in_memory() -> anyhow::Result<Self>` (test/helper)

- [ ] **Step 1: Write the failing test**

Create `src/journal.rs`:

```rust
//! Append-only SQLite log of executed trades.

use crate::sizing::ExecutionPlan;
use rusqlite::Connection;
use std::sync::Mutex;

pub struct Journal {
    connection: Mutex<Connection>,
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    coin TEXT NOT NULL,
    direction TEXT NOT NULL,
    size REAL NOT NULL,
    entry REAL NOT NULL,
    leverage INTEGER NOT NULL,
    stop_loss REAL NOT NULL,
    entry_order_id INTEGER
)";

impl Journal {
    fn from_connection(connection: Connection) -> anyhow::Result<Self> {
        connection.execute(SCHEMA, [])?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    pub fn open(path: &str) -> anyhow::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn record(&self, plan: &ExecutionPlan, entry_order_id: Option<u64>) -> anyhow::Result<()> {
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO trades (coin, direction, size, entry, leverage, stop_loss, entry_order_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                plan.coin,
                format!("{:?}", plan.direction),
                plan.size,
                plan.entry,
                plan.leverage,
                plan.stop_loss.price,
                entry_order_id.map(|id| id as i64),
            ],
        )?;
        Ok(())
    }

    #[cfg(test)]
    fn count(&self) -> anyhow::Result<i64> {
        let connection = self.connection.lock().unwrap();
        Ok(connection.query_row("SELECT COUNT(*) FROM trades", [], |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Direction;
    use crate::sizing::BracketLeg;

    #[test]
    fn records_a_trade_row() {
        let journal = Journal::open_in_memory().unwrap();
        let plan = ExecutionPlan {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            size: 100.0,
            entry: 1.40,
            leverage: 3,
            notional: 140.0,
            margin: 46.6,
            risk_amount: 100.0,
            liquidation_price: 0.93,
            stop_loss: BracketLeg { price: 1.25, size: 100.0 },
            take_profits: vec![],
            warnings: vec![],
        };
        journal.record(&plan, Some(42)).unwrap();
        assert_eq!(journal.count().unwrap(), 1);
    }
}
```

- [ ] **Step 2: Add `mod journal;` to `src/main.rs` and run the test**

Run: `cargo test --lib journal`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add src/journal.rs src/main.rs
git commit -m "feat: SQLite trade journal"
```

---

### Task 7: Confirmation render helpers

**Files:**
- Create: `src/telegram.rs`
- Modify: `src/main.rs` (add `mod telegram;`)

**Interfaces:**
- Consumes: `sizing::{ExecutionPlan, RiskProfile}`, `parser::Direction`.
- Produces:
  - `pub fn render_summary(plan: &ExecutionPlan, profile: RiskProfile) -> String`
  - `pub fn confirmation_keyboard(active: RiskProfile) -> teloxide::types::InlineKeyboardMarkup`
  - Callback data constants: `pub const CB_CONSERVATIVE`, `CB_MODERATE`, `CB_AGGRESSIVE`, `CB_LIMIT`, `CB_MARKET`, `CB_CANCEL` (all `&str`).

- [ ] **Step 1: Write the failing test for the summary text**

Create `src/telegram.rs`:

```rust
//! Telegram rendering helpers and (Task 8) handlers.

use crate::parser::Direction;
use crate::sizing::{ExecutionPlan, RiskProfile};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

pub const CB_CONSERVATIVE: &str = "profile:conservative";
pub const CB_MODERATE: &str = "profile:moderate";
pub const CB_AGGRESSIVE: &str = "profile:aggressive";
pub const CB_LIMIT: &str = "confirm:limit";
pub const CB_MARKET: &str = "confirm:market";
pub const CB_CANCEL: &str = "cancel";

pub fn render_summary(plan: &ExecutionPlan, profile: RiskProfile) -> String {
    let direction = match plan.direction {
        Direction::Long => "LONG",
        Direction::Short => "SHORT",
    };
    let mut text = format!(
        "*{coin}* {direction}  ({profile:?})\n\
         Size: {size} (notional ${notional:.2})\n\
         Entry: ${entry:.4}  Leverage: {leverage}x\n\
         Margin: ${margin:.2}  Risk: ${risk:.2}\n\
         SL: ${sl:.4} (100%)  est. liq ${liq:.4}\n",
        coin = plan.coin,
        size = plan.size,
        notional = plan.notional,
        entry = plan.entry,
        leverage = plan.leverage,
        margin = plan.margin,
        risk = plan.risk_amount,
        sl = plan.stop_loss.price,
        liq = plan.liquidation_price,
    );
    for (index, take_profit) in plan.take_profits.iter().enumerate() {
        text.push_str(&format!(
            "TP{}: ${:.4} ({})\n",
            index + 1,
            take_profit.price,
            take_profit.size,
        ));
    }
    for warning in &plan.warnings {
        text.push_str(&format!("⚠️ {warning}\n"));
    }
    text
}

fn label(text: &str, active: bool) -> String {
    if active { format!("{text} ✓") } else { text.to_string() }
}

pub fn confirmation_keyboard(active: RiskProfile) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(label("Conservative", active == RiskProfile::Conservative), CB_CONSERVATIVE),
            InlineKeyboardButton::callback(label("Moderate", active == RiskProfile::Moderate), CB_MODERATE),
            InlineKeyboardButton::callback(label("Aggressive", active == RiskProfile::Aggressive), CB_AGGRESSIVE),
        ],
        vec![
            InlineKeyboardButton::callback("✅ Confirm Limit", CB_LIMIT),
            InlineKeyboardButton::callback("⚡ Confirm Market", CB_MARKET),
        ],
        vec![InlineKeyboardButton::callback("❌ Cancel", CB_CANCEL)],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Direction;
    use crate::sizing::BracketLeg;

    fn plan() -> ExecutionPlan {
        ExecutionPlan {
            coin: "PENDLE".into(),
            direction: Direction::Long,
            size: 666.6,
            entry: 1.40,
            leverage: 3,
            notional: 933.24,
            margin: 311.08,
            risk_amount: 100.0,
            liquidation_price: 0.93,
            stop_loss: BracketLeg { price: 1.25, size: 666.6 },
            take_profits: vec![
                BracketLeg { price: 1.70, size: 399.9 },
                BracketLeg { price: 2.00, size: 266.6 },
            ],
            warnings: vec!["estimated liquidation is tighter than stop-loss".into()],
        }
    }

    #[test]
    fn summary_includes_key_fields() {
        let text = render_summary(&plan(), RiskProfile::Moderate);
        assert!(text.contains("PENDLE"));
        assert!(text.contains("LONG"));
        assert!(text.contains("3x"));
        assert!(text.contains("TP1"));
        assert!(text.contains("TP2"));
        assert!(text.contains("⚠️"));
    }

    #[test]
    fn keyboard_marks_active_profile() {
        let markup = confirmation_keyboard(RiskProfile::Aggressive);
        let first_row = &markup.inline_keyboard[0];
        assert!(first_row[2].text.contains('✓')); // Aggressive marked
        assert!(!first_row[0].text.contains('✓'));
    }
}
```

- [ ] **Step 2: Add `mod telegram;` to `src/main.rs` and run the tests**

Run: `cargo test --lib telegram`
Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add src/telegram.rs src/main.rs
git commit -m "feat: confirmation summary and inline keyboard rendering"
```

---

### Task 8: Orchestration + dispatcher wiring

**Files:**
- Modify: `src/telegram.rs` (add handlers + execution)
- Modify: `src/main.rs` (full bootstrap)
- Create: `README.md`

**Interfaces:**
- Consumes: everything above — `config::Config`, `parser::parse_setup`, `sizing::{build_plan, SizingInput, RiskProfile, AssetMeta}`, `hyperliquid::{Exchange, HyperliquidExchange, EntryOrder, TriggerOrder}`, `state::{PendingStore, PendingTrade}`, `journal::Journal`, render helpers.
- Produces:
  - `pub fn recompute_plan(trade: &PendingTrade, profile: RiskProfile, risk_pct: f64, leverage: &LeverageMap) -> Result<ExecutionPlan, SizingError>` (pure, unit-tested)
  - `pub async fn execute_plan<E: Exchange>(exchange: &E, plan: &ExecutionPlan, use_limit: bool, fill_timeout_secs: u64) -> anyhow::Result<()>` (unit-tested with `MockExchange`)
  - `pub async fn run(config: Config) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test for `execute_plan` order sequence**

Add to the `#[cfg(test)] mod tests` block in `src/telegram.rs`:

```rust
    use crate::hyperliquid::mock::MockExchange;
    use crate::sizing::AssetMeta;

    #[tokio::test]
    async fn execute_plan_sets_leverage_then_entry_then_brackets() {
        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            ..Default::default()
        };
        let plan = plan(); // long, size 666.6, 2 TPs
        super::execute_plan(&exchange, &plan, false, 1).await.unwrap();

        assert_eq!(exchange.leverage_calls.lock().unwrap().len(), 1);
        assert_eq!(exchange.entries.lock().unwrap().len(), 1);
        // SL + TP1 + TP2 = 3 trigger orders, all reduce-only, opposite side (sell).
        let triggers = exchange.triggers.lock().unwrap();
        assert_eq!(triggers.len(), 3);
        assert!(triggers.iter().all(|t| !t.is_buy)); // closing a long => sell
        assert_eq!(triggers.iter().filter(|t| t.is_take_profit).count(), 2);
        assert_eq!(triggers.iter().filter(|t| !t.is_take_profit).count(), 1);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib telegram::tests::execute_plan_sets_leverage_then_entry_then_brackets`
Expected: FAIL — `cannot find function execute_plan`.

- [ ] **Step 3: Implement `recompute_plan` and `execute_plan`**

Add to `src/telegram.rs` (above the test module). Add imports at the top of the file: `use crate::config::LeverageMap; use crate::hyperliquid::{Exchange, EntryOrder, TriggerOrder}; use crate::parser::Direction; use crate::sizing::{build_plan, SizingError, SizingInput}; use crate::state::PendingTrade; use std::time::Duration;`

```rust
/// Recomputes the plan for a different risk profile, reusing the cached equity
/// and asset metadata captured when the card was first parsed.
pub fn recompute_plan(
    trade: &PendingTrade,
    profile: RiskProfile,
    risk_pct: f64,
    leverage: &LeverageMap,
) -> Result<ExecutionPlan, SizingError> {
    build_plan(&SizingInput {
        setup: &trade.setup,
        equity: trade.equity,
        risk_pct,
        profile,
        leverage,
        asset_meta: &trade.asset_meta,
    })
}

/// Sets leverage, places the entry order, waits for fill (limit only), then
/// places the reduce-only bracket. Bracket side is opposite the position.
pub async fn execute_plan<E: Exchange>(
    exchange: &E,
    plan: &ExecutionPlan,
    use_limit: bool,
    fill_timeout_secs: u64,
) -> anyhow::Result<()> {
    let is_buy = matches!(plan.direction, Direction::Long);
    exchange.set_leverage(&plan.coin, plan.leverage).await?;

    let entry = EntryOrder {
        coin: plan.coin.clone(),
        is_buy,
        size: plan.size,
        limit_price: if use_limit { Some(plan.entry) } else { None },
    };
    let entry_result = exchange.place_entry(&entry).await?;

    // For a limit order, wait until the position actually exists before
    // arming the bracket; otherwise the triggers would have nothing to reduce.
    if use_limit && !entry_result.filled {
        let deadline = fill_timeout_secs;
        let mut elapsed = 0;
        loop {
            if exchange.position_size(&plan.coin).await? >= plan.size * 0.99 {
                break;
            }
            if elapsed >= deadline {
                anyhow::bail!("entry limit order not filled within {deadline}s; bracket not placed");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            elapsed += 1;
        }
    }

    // Bracket: SL (full size) + each TP. Closing side is opposite the entry.
    let close_is_buy = !is_buy;
    exchange
        .place_trigger(&TriggerOrder {
            coin: plan.coin.clone(),
            is_buy: close_is_buy,
            size: plan.stop_loss.size,
            trigger_price: plan.stop_loss.price,
            is_take_profit: false,
        })
        .await?;
    for take_profit in &plan.take_profits {
        exchange
            .place_trigger(&TriggerOrder {
                coin: plan.coin.clone(),
                is_buy: close_is_buy,
                size: take_profit.size,
                trigger_price: take_profit.price,
                is_take_profit: true,
            })
            .await?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify the test passes**

Run: `cargo test --lib telegram`
Expected: 3 passed (`summary_includes_key_fields`, `keyboard_marks_active_profile`, `execute_plan_sets_leverage_then_entry_then_brackets`).

- [ ] **Step 5: Implement the dispatcher `run` and full `main`**

Add to `src/telegram.rs` (top-level, after the helpers). Add imports: `use crate::journal::Journal; use crate::parser::parse_setup; use crate::sizing::AssetMeta; use crate::state::PendingStore; use crate::hyperliquid::Exchange as _; use std::sync::Arc; use teloxide::prelude::*; use teloxide::types::ParseMode;`

```rust
struct BotContext<E: Exchange + 'static> {
    config: crate::config::Config,
    exchange: Arc<E>,
    store: Arc<PendingStore>,
    journal: Arc<Journal>,
}

fn profile_from_callback(data: &str) -> Option<RiskProfile> {
    match data {
        CB_CONSERVATIVE => Some(RiskProfile::Conservative),
        CB_MODERATE => Some(RiskProfile::Moderate),
        CB_AGGRESSIVE => Some(RiskProfile::Aggressive),
        _ => None,
    }
}

async fn on_message<E: Exchange + 'static>(
    bot: Bot,
    message: Message,
    context: Arc<BotContext<E>>,
) -> anyhow::Result<()> {
    let user_id = match message.from() {
        Some(user) => user.id.0 as i64,
        None => return Ok(()),
    };
    if !context.config.is_allowed(user_id) {
        return Ok(()); // ignore non-allowlisted users
    }
    let text = match message.text() {
        Some(text) => text,
        None => return Ok(()),
    };

    let setup = match parse_setup(text) {
        Ok(setup) => setup,
        Err(error) => {
            bot.send_message(message.chat.id, format!("Could not parse setup: {error}")).await?;
            return Ok(());
        }
    };

    if let Some(gate) = context.config.confidence_gate {
        if setup.confidence.map(|c| c < gate).unwrap_or(false) {
            bot.send_message(message.chat.id, format!("Confidence {:?} below gate {gate}; skipped.", setup.confidence)).await?;
            return Ok(());
        }
    }

    let equity = context.exchange.equity().await?;
    let asset_meta: AssetMeta = context.exchange.asset_meta(&setup.coin).await?;
    let profile = RiskProfile::Moderate;
    let plan = match build_plan(&SizingInput {
        setup: &setup,
        equity,
        risk_pct: context.config.risk_pct,
        profile,
        leverage: &context.config.leverage,
        asset_meta: &asset_meta,
    }) {
        Ok(plan) => plan,
        Err(error) => {
            bot.send_message(message.chat.id, format!("Cannot size trade: {error}")).await?;
            return Ok(());
        }
    };

    let sent = bot
        .send_message(message.chat.id, render_summary(&plan, profile))
        .parse_mode(ParseMode::Markdown)
        .reply_markup(confirmation_keyboard(profile))
        .await?;

    context.store.insert(
        sent.id.0 as i64,
        PendingTrade { setup, equity, asset_meta, profile, plan },
    );
    Ok(())
}

async fn on_callback<E: Exchange + 'static>(
    bot: Bot,
    query: CallbackQuery,
    context: Arc<BotContext<E>>,
) -> anyhow::Result<()> {
    let message = match query.message.as_ref() {
        Some(message) => message,
        None => return Ok(()),
    };
    let key = message.id.0 as i64;
    let data = query.data.clone().unwrap_or_default();

    // Profile switch: recompute and edit the message in place.
    if let Some(profile) = profile_from_callback(&data) {
        if let Some(mut trade) = context.store.get(key) {
            match recompute_plan(&trade, profile, context.config.risk_pct, &context.config.leverage) {
                Ok(plan) => {
                    trade.profile = profile;
                    trade.plan = plan.clone();
                    context.store.insert(key, trade);
                    bot.edit_message_text(message.chat.id, message.id, render_summary(&plan, profile))
                        .parse_mode(ParseMode::Markdown)
                        .reply_markup(confirmation_keyboard(profile))
                        .await?;
                }
                Err(error) => {
                    bot.answer_callback_query(query.id).text(format!("{error}")).show_alert(true).await?;
                }
            }
        }
        return Ok(());
    }

    if data == CB_CANCEL {
        context.store.remove(key);
        bot.edit_message_text(message.chat.id, message.id, "Cancelled.").await?;
        return Ok(());
    }

    let use_limit = match data.as_str() {
        CB_LIMIT => true,
        CB_MARKET => false,
        _ => return Ok(()),
    };

    let trade = match context.store.remove(key) {
        Some(trade) => trade,
        None => {
            bot.answer_callback_query(query.id).text("Setup expired.").await?;
            return Ok(());
        }
    };

    bot.answer_callback_query(query.id).await?;
    bot.edit_message_text(message.chat.id, message.id, format!("Executing {}…", trade.plan.coin)).await?;

    match execute_plan(context.exchange.as_ref(), &trade.plan, use_limit, context.config.entry_fill_timeout_secs).await {
        Ok(()) => {
            let _ = context.journal.record(&trade.plan, None);
            bot.send_message(message.chat.id, format!("✅ Executed {} with SL/TP bracket.", trade.plan.coin)).await?;
        }
        Err(error) => {
            bot.send_message(message.chat.id, format!("❌ Execution failed: {error}")).await?;
        }
    }
    Ok(())
}

pub async fn run<E: Exchange + 'static>(
    config: crate::config::Config,
    exchange: Arc<E>,
) -> anyhow::Result<()> {
    let bot = Bot::new(&config.telegram_token);
    let context = Arc::new(BotContext {
        config,
        exchange,
        store: Arc::new(PendingStore::new()),
        journal: Arc::new(Journal::open("trades.db")?),
    });

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(on_message::<E>))
        .branch(Update::filter_callback_query().endpoint(on_callback::<E>));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![context])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}
```

- [ ] **Step 6: Replace `src/main.rs` with the full bootstrap**

```rust
mod config;
mod hyperliquid;
mod journal;
mod parser;
mod sizing;
mod state;
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
```

- [ ] **Step 7: Build everything and run the full test suite**

Run: `cargo build && cargo test`
Expected: build succeeds; all unit tests pass (config 2, parser 3, sizing 5, hyperliquid 1, state 1, journal 1, telegram 3).

Note: if `teloxide` 0.13 method/type names differ (e.g. `query.message` shape), adjust the handler signatures to match the installed version — the pure helpers (`render_summary`, `confirmation_keyboard`, `recompute_plan`, `execute_plan`) are the tested contract and stay fixed.

- [ ] **Step 8: Write `README.md`**

```markdown
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
```

- [ ] **Step 9: Commit**

```bash
git add src/telegram.rs src/main.rs README.md Cargo.toml Cargo.lock
git commit -m "feat: wire dispatcher, execution orchestration, and docs"
```

---

## Self-Review Notes

- **Spec coverage:** parser (Task 2), risk-based sizing + leverage mapping + liquidation warning (Task 3), Exchange trait/mock/SDK (Task 4), confirm-flow state (Task 5), journal (Task 6), summary+keyboard (Task 7), confirm buttons + entry-type-at-confirm + native bracket + allowlist + confidence gate + testnet default + dispatcher (Task 8). Config defaults (Task 1). All spec sections map to a task.
- **Entry-type-at-confirmation:** delivered by `CB_LIMIT`/`CB_MARKET` buttons feeding `use_limit` into `execute_plan`.
- **Type consistency:** `AssetMeta`, `ExecutionPlan`, `BracketLeg`, `RiskProfile`, `Direction`, `LeverageMap.leverage_for`, `build_plan`, `execute_plan`, `recompute_plan`, and the `Exchange` trait methods are referenced with identical signatures across tasks.
- **SDK/teloxide version risk:** isolated to Task 4's `HyperliquidExchange` and Task 8's handler wiring; all tested logic sits behind fixed-signature pure functions and the `Exchange` trait.
