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
