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
}
