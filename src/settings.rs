//! Runtime-mutable trading settings: the parameters a user can change from
//! Telegram (`/settings`, `/set`). Persistence lives in `SettingsStore` (added
//! in a later task); this module holds the in-memory model and the pure
//! validation logic so it can be unit-tested without any I/O.

use crate::config::LeverageMap;
use crate::sizing::EntryMode;

/// The full set of runtime-tunable trading parameters. Cloned out of the
/// shared lock before use; never mutated in place across an await.
#[derive(Debug, Clone)]
pub struct Settings {
    pub entry_mode: EntryMode,
    pub risk_pct: f64,
    pub entry_pct: f64,
    pub entry_fixed_usd: f64,
    pub max_daily_risk_pct: Option<f64>,
    pub leverage: LeverageMap,
}

/// Comma-separated list of valid `/set` keys, used in error messages.
const VALID_KEYS: &str = "entry_mode, risk_pct, entry_pct, entry_fixed_usd, \
max_daily_risk_pct, leverage_conservative, leverage_moderate, leverage_aggressive";

/// Parses a percentage that must be strictly positive and at most 100.
fn parse_percent(value: &str) -> Result<f64, String> {
    let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
    if parsed <= 0.0 || parsed > 100.0 {
        return Err(format!("value must be > 0 and <= 100 (got {parsed})"));
    }
    Ok(parsed)
}

/// Parses a leverage multiplier that must be at least 1.
fn parse_leverage(value: &str) -> Result<u32, String> {
    let parsed: u32 = value.parse().map_err(|_| format!("'{value}' is not a whole number"))?;
    if parsed < 1 {
        return Err("leverage must be >= 1".to_string());
    }
    Ok(parsed)
}

/// Returns a new `Settings` with `key` set to `raw_value`, or an error message
/// suitable for sending straight back to the user. Pure — no I/O, no mutation
/// of `current`.
///
/// `max_daily_risk_pct` accepts an empty string or `0` to disable the cap.
/// `entry_mode` accepts the tokens `risk`, `percent`, `fixed`.
pub fn apply_setting(current: &Settings, key: &str, raw_value: &str) -> Result<Settings, String> {
    let value = raw_value.trim();
    let mut next = current.clone();
    match key {
        "entry_mode" => {
            next.entry_mode = EntryMode::from_str_opt(value)
                .ok_or_else(|| "entry_mode must be one of: risk, percent, fixed".to_string())?;
        }
        "risk_pct" => next.risk_pct = parse_percent(value)?,
        "entry_pct" => next.entry_pct = parse_percent(value)?,
        "entry_fixed_usd" => {
            let parsed: f64 = value.parse().map_err(|_| format!("'{value}' is not a number"))?;
            if parsed <= 0.0 {
                return Err(format!("entry_fixed_usd must be > 0 (got {parsed})"));
            }
            next.entry_fixed_usd = parsed;
        }
        "max_daily_risk_pct" => {
            next.max_daily_risk_pct = if value.is_empty() || value == "0" {
                None
            } else {
                Some(parse_percent(value)?)
            };
        }
        "leverage_conservative" => next.leverage.conservative = parse_leverage(value)?,
        "leverage_moderate" => next.leverage.moderate = parse_leverage(value)?,
        "leverage_aggressive" => next.leverage.aggressive = parse_leverage(value)?,
        _ => return Err(format!("Unknown setting '{key}'. Valid keys: {VALID_KEYS}")),
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        Settings {
            entry_mode: EntryMode::RiskBased,
            risk_pct: 1.0,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            max_daily_risk_pct: Some(5.0),
            leverage: LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
        }
    }

    #[test]
    fn sets_a_valid_numeric_key() {
        let next = apply_setting(&sample(), "entry_pct", "20").unwrap();
        assert_eq!(next.entry_pct, 20.0);
    }

    #[test]
    fn sets_entry_mode_token() {
        let next = apply_setting(&sample(), "entry_mode", "percent").unwrap();
        assert_eq!(next.entry_mode, EntryMode::PercentBalance);
    }

    #[test]
    fn empty_or_zero_disables_daily_cap() {
        assert_eq!(apply_setting(&sample(), "max_daily_risk_pct", "").unwrap().max_daily_risk_pct, None);
        assert_eq!(apply_setting(&sample(), "max_daily_risk_pct", "0").unwrap().max_daily_risk_pct, None);
    }

    #[test]
    fn rejects_unknown_key() {
        let err = apply_setting(&sample(), "wat", "1").unwrap_err();
        assert!(err.contains("Unknown setting"));
    }

    #[test]
    fn rejects_non_numeric_value() {
        assert!(apply_setting(&sample(), "risk_pct", "abc").is_err());
    }

    #[test]
    fn rejects_percent_out_of_range() {
        assert!(apply_setting(&sample(), "risk_pct", "0").is_err());
        assert!(apply_setting(&sample(), "risk_pct", "150").is_err());
    }

    #[test]
    fn rejects_leverage_below_one() {
        assert!(apply_setting(&sample(), "leverage_moderate", "0").is_err());
    }
}
