//! Loads runtime configuration from environment variables.

use crate::sizing::EntryMode;

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
    /// Default entry sizing mode; seeds the runtime Settings on first boot.
    pub entry_mode: EntryMode,
    /// Percent-of-equity notional for `EntryMode::PercentBalance` (seed).
    pub entry_pct: f64,
    /// Fixed USD notional for `EntryMode::FixedUsd` (seed).
    pub entry_fixed_usd: f64,
    pub leverage: LeverageMap,
    pub entry_fill_timeout_secs: u64,
    pub confidence_gate: Option<u8>,
    pub deepseek_api_key: Option<String>,
    pub deepseek_base_url: String,
    pub deepseek_model: String,
    /// Master account address (0x…) that holds funds and positions.
    /// Required when `agent_key` is an API/agent wallet; if unset the bot queries
    /// the agent wallet's own address and equity will read 0.
    pub account_address: Option<String>,
    /// When true, the bot reads equity from the spot USDC balance instead of the
    /// perp account value. Under unified-account mode the perp `accountValue` is 0;
    /// all collateral lives in the SPOT clearinghouse.
    pub unified_account: bool,
    /// Maximum cumulative risk to commit per UTC day, expressed as a percentage
    /// of equity (e.g. `5.0` = 5%). Confirmed trades that would exceed this cap
    /// are rejected without executing. `None` disables the cap.
    pub max_daily_risk_pct: Option<f64>,
    /// API key for OpenAI (used for vision/image parsing). When `None`, image
    /// parsing is disabled and the bot will prompt the user to set it.
    pub openai_api_key: Option<String>,
    /// Base URL for the OpenAI-compatible vision API.
    pub openai_base_url: String,
    /// Vision model to use (e.g. `gpt-4o-mini`).
    pub openai_vision_model: String,
    /// TCP address the read-only portfolio HTTP API binds to.
    /// Set via `HTTP_BIND_ADDR` (default `127.0.0.1:8088`).
    pub http_bind_addr: String,
    /// Bearer token that protects the portfolio API.
    /// Set via `PORTFOLIO_API_TOKEN`. When absent or empty the API is disabled.
    pub api_token: Option<String>,
    /// Path to the SQLite trade journal shared by the bot and the API server.
    /// Set via `JOURNAL_DB_PATH` (default `trades.db`).
    pub journal_path: String,
    /// Seconds between background polls of the fill history for close
    /// (TP/SL) notifications. Set via `MONITOR_POLL_SECS` (default 30).
    pub monitor_poll_secs: u64,
    /// Seconds an unfilled trigger entry rests before auto-cancellation.
    /// Set via `TRIGGER_EXPIRY_SECS` (default 14400 = 4h).
    pub trigger_expiry_secs: u64,
    /// Whether the "Confirm Trigger" (stop-entry) button is shown/usable. OFF by
    /// default: the trigger tpsl direction mapping is unverified against the live
    /// exchange. Set via `TRIGGER_ENTRY_ENABLED` (1/true/yes to enable).
    pub trigger_entry_enabled: bool,
    /// Seconds between background P&L push updates while positions are open.
    /// Set via `PNL_PUSH_SECS` (default 900 = 15min; 0 disables).
    pub pnl_push_secs: u64,
}

impl Config {
    pub fn is_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.contains(&user_id)
    }
}

/// Parses `raw` as a comma-separated list of i64 user IDs.
///
/// - Blank segments (from double commas or trailing commas) are silently skipped.
/// - Non-blank segments that cannot be parsed as i64 emit a `tracing::warn!` and are dropped.
fn parse_user_ids(raw: &str) -> Vec<i64> {
    raw.split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .filter_map(|segment| match segment.parse::<i64>() {
            Ok(id) => Some(id),
            Err(_) => {
                tracing::warn!(
                    token = segment,
                    "TELEGRAM_ALLOWED_USER_IDS: dropping non-integer token"
                );
                None
            }
        })
        .collect()
}

/// Returns the parsed value of the environment variable `key`, or `default` when the variable is
/// absent.  Returns an error when the variable is present but cannot be parsed as `T`.
fn parse_env_or<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|err| anyhow::anyhow!("invalid {key}={raw}: {err}")),
        Err(_) => Ok(default),
    }
}

pub fn from_env() -> anyhow::Result<Config> {
    let telegram_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .map_err(|_| anyhow::anyhow!("TELEGRAM_BOT_TOKEN is required"))?;
    let agent_key = std::env::var("HYPERLIQUID_AGENT_KEY")
        .map_err(|_| anyhow::anyhow!("HYPERLIQUID_AGENT_KEY is required"))?;

    let allowed_user_ids = parse_user_ids(
        &std::env::var("TELEGRAM_ALLOWED_USER_IDS").unwrap_or_default(),
    );
    if allowed_user_ids.is_empty() {
        tracing::warn!("TELEGRAM_ALLOWED_USER_IDS is empty; the bot will serve no users");
    }

    let network = match std::env::var("HYPERLIQUID_NETWORK").as_deref() {
        Ok("mainnet") => Network::Mainnet,
        _ => Network::Testnet,
    };

    let confidence_gate = match std::env::var("CONFIDENCE_GATE") {
        Ok(raw) => Some(
            raw.parse::<u8>()
                .map_err(|err| anyhow::anyhow!("invalid CONFIDENCE_GATE={raw}: {err}"))?,
        ),
        Err(_) => None,
    };

    Ok(Config {
        telegram_token,
        allowed_user_ids,
        agent_key,
        network,
        risk_pct: parse_env_or("RISK_PCT", 1.0_f64)?,
        entry_mode: match std::env::var("ENTRY_MODE").as_deref() {
            Ok("percent") => EntryMode::PercentBalance,
            Ok("fixed") => EntryMode::FixedUsd,
            _ => EntryMode::RiskBased,
        },
        entry_pct: parse_env_or("ENTRY_PCT", 10.0_f64)?,
        entry_fixed_usd: parse_env_or("ENTRY_FIXED_USD", 50.0_f64)?,
        leverage: LeverageMap {
            conservative: parse_env_or("LEVERAGE_CONSERVATIVE", 2_u32)?,
            moderate: parse_env_or("LEVERAGE_MODERATE", 3_u32)?,
            aggressive: parse_env_or("LEVERAGE_AGGRESSIVE", 5_u32)?,
        },
        entry_fill_timeout_secs: parse_env_or("ENTRY_FILL_TIMEOUT_SECS", 300_u64)?,
        confidence_gate,
        deepseek_api_key: std::env::var("DEEPSEEK_API_KEY").ok().filter(|s| !s.is_empty()),
        deepseek_base_url: std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
        deepseek_model: std::env::var("DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string()),
        account_address: std::env::var("HYPERLIQUID_ACCOUNT_ADDRESS").ok().filter(|s| !s.is_empty()),
        unified_account: std::env::var("HYPERLIQUID_UNIFIED_ACCOUNT")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false),
        max_daily_risk_pct: std::env::var("MAX_DAILY_RISK_PCT").ok().and_then(|v| v.parse().ok()),
        openai_api_key: std::env::var("OPENAI_API_KEY").ok().filter(|s| !s.is_empty()),
        openai_base_url: std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        openai_vision_model: std::env::var("OPENAI_VISION_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        http_bind_addr: std::env::var("HTTP_BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8088".to_string()),
        api_token: std::env::var("PORTFOLIO_API_TOKEN").ok().filter(|s| !s.is_empty()),
        journal_path: std::env::var("JOURNAL_DB_PATH").unwrap_or_else(|_| "trades.db".to_string()),
        monitor_poll_secs: parse_env_or("MONITOR_POLL_SECS", 30_u64)?,
        trigger_expiry_secs: parse_env_or("TRIGGER_EXPIRY_SECS", 14400_u64)?,
        trigger_entry_enabled: std::env::var("TRIGGER_ENTRY_ENABLED")
            .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false),
        pnl_push_secs: parse_env_or("PNL_PUSH_SECS", 900_u64)?,
    })
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
            entry_mode: crate::sizing::EntryMode::RiskBased,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
            entry_fill_timeout_secs: 300,
            confidence_gate: None,
            deepseek_api_key: None,
            deepseek_base_url: "https://api.deepseek.com".into(),
            deepseek_model: "deepseek-chat".into(),
            account_address: None,
            unified_account: false,
            max_daily_risk_pct: None,
            openai_api_key: None,
            openai_base_url: "https://api.openai.com/v1".into(),
            openai_vision_model: "gpt-4o-mini".into(),
            http_bind_addr: "127.0.0.1:8088".into(),
            api_token: None,
            journal_path: "trades.db".into(),
            monitor_poll_secs: 30,
            trigger_expiry_secs: 14400,
            trigger_entry_enabled: false,
            pnl_push_secs: 900,
        };
        assert!(config.is_allowed(42));
        assert!(!config.is_allowed(99));
    }

    #[test]
    fn monitor_poll_secs_defaults_to_30_when_absent() {
        std::env::remove_var("MONITOR_POLL_SECS");
        assert_eq!(parse_env_or::<u64>("MONITOR_POLL_SECS", 30).unwrap(), 30);
    }

    #[test]
    fn parse_env_or_errors_on_present_invalid_value() {
        std::env::set_var("AGENT_HL_TEST_INVALID", "not_a_number");
        let result = parse_env_or::<f64>("AGENT_HL_TEST_INVALID", 1.0);
        std::env::remove_var("AGENT_HL_TEST_INVALID");
        assert!(result.is_err());
    }

    #[test]
    fn parse_env_or_uses_default_when_absent() {
        std::env::remove_var("AGENT_HL_TEST_ABSENT");
        assert_eq!(parse_env_or::<f64>("AGENT_HL_TEST_ABSENT", 1.0).unwrap(), 1.0);
    }

}
