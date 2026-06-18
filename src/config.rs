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
    /// Local HTTP port for the `/ingest` endpoint. Leave `None` to disable.
    pub ingest_port: Option<u16>,
    /// Shared token for the `/ingest` endpoint (checked via `X-Ingest-Token` header).
    /// When `None` the endpoint refuses to start even if a port is configured.
    pub ingest_token: Option<String>,
    /// Telegram chat id to receive confirmation cards from ingest signals.
    /// Defaults to the first entry in `allowed_user_ids` when `None`.
    pub ingest_chat_id: Option<i64>,
}

impl Config {
    pub fn is_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.contains(&user_id)
    }
}

/// Returns `true` when the ingest request is authorized: a token is configured
/// and the provided token matches it. Denies when either side is missing.
///
/// Simple string equality is sufficient here because the endpoint is
/// localhost-only — constant-time comparison would be overkill.
pub fn ingest_authorized(expected: Option<&str>, provided: Option<&str>) -> bool {
    match (expected, provided) {
        (Some(expected_token), Some(provided_token)) => expected_token == provided_token,
        _ => false,
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
        ingest_port: std::env::var("INGEST_PORT").ok().and_then(|v| v.parse().ok()),
        ingest_token: std::env::var("INGEST_TOKEN").ok().filter(|s| !s.is_empty()),
        ingest_chat_id: std::env::var("INGEST_CHAT_ID").ok().and_then(|v| v.parse().ok()),
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
            leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
            entry_fill_timeout_secs: 300,
            confidence_gate: None,
            deepseek_api_key: None,
            deepseek_base_url: "https://api.deepseek.com".into(),
            deepseek_model: "deepseek-chat".into(),
            account_address: None,
            unified_account: false,
            ingest_port: None,
            ingest_token: None,
            ingest_chat_id: None,
        };
        assert!(config.is_allowed(42));
        assert!(!config.is_allowed(99));
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

    #[test]
    fn ingest_authorized_grants_access_when_tokens_match() {
        assert!(ingest_authorized(Some("secret"), Some("secret")));
    }

    #[test]
    fn ingest_authorized_denies_on_token_mismatch() {
        assert!(!ingest_authorized(Some("secret"), Some("wrong")));
    }

    #[test]
    fn ingest_authorized_denies_when_no_token_provided() {
        assert!(!ingest_authorized(Some("secret"), None));
    }

    #[test]
    fn ingest_authorized_denies_when_no_token_configured() {
        assert!(!ingest_authorized(None, Some("secret")));
    }

    #[test]
    fn ingest_authorized_denies_when_both_sides_absent() {
        assert!(!ingest_authorized(None, None));
    }
}
