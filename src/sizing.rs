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

/// How the position size is determined for a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryMode {
    /// Size from risk: `equity × risk_pct/100 ÷ stop_distance`.
    RiskBased,
    /// Size so notional equals `equity × entry_pct/100`.
    PercentBalance,
    /// Size so notional equals a fixed USD amount.
    FixedUsd,
}

impl EntryMode {
    /// Stable token used for persistence and callback data.
    pub fn as_str(&self) -> &'static str {
        match self {
            EntryMode::RiskBased => "risk",
            EntryMode::PercentBalance => "percent",
            EntryMode::FixedUsd => "fixed",
        }
    }

    /// Parses the token produced by `as_str`; `None` for anything else.
    pub fn from_str_opt(token: &str) -> Option<EntryMode> {
        match token {
            "risk" => Some(EntryMode::RiskBased),
            "percent" => Some(EntryMode::PercentBalance),
            "fixed" => Some(EntryMode::FixedUsd),
            _ => None,
        }
    }

    /// Human-readable label for the settings UI.
    pub fn label(&self) -> &'static str {
        match self {
            EntryMode::RiskBased => "Risk-based",
            EntryMode::PercentBalance => "% Balance",
            EntryMode::FixedUsd => "Fixed USD",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetMeta {
    pub sz_decimals: u32,
    pub max_leverage: u32,
}

#[derive(Debug)]
pub struct SizingInput<'a> {
    pub setup: &'a TradeSetup,
    pub equity: f64,
    pub risk_pct: f64,
    pub entry_mode: EntryMode,
    pub entry_pct: f64,
    pub entry_fixed_usd: f64,
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
    #[error("required margin ${required:.2} exceeds available equity ${available:.2}")]
    MarginExceedsEquity { required: f64, available: f64 },
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

/// Rounds `value` DOWN to `decimals` places.
fn floor_to(value: f64, decimals: u32) -> f64 {
    debug_assert!(decimals <= 8, "szDecimals beyond f64-safe range");
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
    // max_leverage == 0 means the cap is unknown (SDK universe omits it); skip the check.
    if input.asset_meta.max_leverage > 0 && leverage > input.asset_meta.max_leverage {
        return Err(SizingError::LeverageTooHigh { requested: leverage, cap: input.asset_meta.max_leverage });
    }

    // Validate stop side.
    match setup.direction {
        Direction::Long if setup.stop_loss > setup.entry => return Err(SizingError::InvalidStopSide),
        Direction::Short if setup.stop_loss < setup.entry => return Err(SizingError::InvalidStopSide),
        _ => {}
    }

    let stop_distance = (setup.entry - setup.stop_loss).abs();
    if stop_distance == 0.0 {
        return Err(SizingError::ZeroStopDistance);
    }

    let raw_size = match input.entry_mode {
        EntryMode::RiskBased => (input.equity * (input.risk_pct / 100.0)) / stop_distance,
        EntryMode::PercentBalance => (input.equity * (input.entry_pct / 100.0)) / setup.entry,
        EntryMode::FixedUsd => input.entry_fixed_usd / setup.entry,
    };
    let size = floor_to(raw_size, input.asset_meta.sz_decimals);
    let notional = size * setup.entry;
    if notional < MIN_ORDER_NOTIONAL {
        return Err(SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }
    // Actual risk = what we lose if the stop hits, for all modes.
    let risk_amount = size * stop_distance;

    let margin = notional / leverage as f64;
    if margin > input.equity {
        return Err(SizingError::MarginExceedsEquity { required: margin, available: input.equity });
    }
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
            entry_mode: EntryMode::RiskBased,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        let plan = build_plan(&input).expect("should size");

        // risk = size 666.6 × stop_distance 0.15 = 99.99 (actual, post-floor)
        assert!((plan.risk_amount - 99.99).abs() < 1e-6);
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
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0, profile: RiskProfile::Moderate, leverage: &leverage(), asset_meta: &meta };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::InvalidStopSide);
    }

    #[test]
    fn rejects_zero_stop_distance() {
        let mut setup = pendle_setup();
        setup.stop_loss = setup.entry; // equal => zero risk distance
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0, profile: RiskProfile::Moderate, leverage: &leverage(), asset_meta: &meta };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::ZeroStopDistance);
    }

    #[test]
    fn rejects_leverage_over_asset_cap() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 2 };
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0, profile: RiskProfile::Aggressive, leverage: &leverage(), asset_meta: &meta };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::LeverageTooHigh { requested: 5, cap: 2 });
    }

    #[test]
    fn rejects_below_minimum_notional() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput { setup: &setup, equity: 1.0, risk_pct: 1.0, entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0, profile: RiskProfile::Moderate, leverage: &leverage(), asset_meta: &meta };
        // risk = 0.01, size ~0.066 -> floor 0.0 -> below min
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }

    #[test]
    fn unknown_max_leverage_skips_cap_check() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 0 }; // 0 = unknown cap
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0, profile: RiskProfile::Aggressive, leverage: &leverage(), asset_meta: &meta };
        assert!(build_plan(&input).is_ok());
    }

    #[test]
    fn warns_when_liquidation_is_tighter_than_stop() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 50 };
        // Conservative=2x: liq_long ~= entry*(1-1/2)=0.70, far below SL 1.25 -> no warn.
        // Force high leverage profile to push liq above SL.
        let high = LeverageMap { conservative: 2, moderate: 3, aggressive: 20 };
        let input = SizingInput { setup: &setup, equity: 10_000.0, risk_pct: 1.0, entry_mode: EntryMode::RiskBased, entry_pct: 10.0, entry_fixed_usd: 50.0, profile: RiskProfile::Aggressive, leverage: &high, asset_meta: &meta };
        let plan = build_plan(&input).unwrap();
        // liq_long = 1.40*(1-1/20)=1.33 > SL 1.25 -> warning present.
        assert!(plan.warnings.iter().any(|w| w.contains("liquidation")));
    }

    #[test]
    fn percent_balance_sizes_from_equity_notional() {
        let setup = pendle_setup(); // entry 1.40
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::PercentBalance,
            entry_pct: 10.0, // target notional = 1000
            entry_fixed_usd: 50.0,
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        let plan = build_plan(&input).expect("should size");
        // raw size = 1000 / 1.40 = 714.28.. -> floor 1 decimal = 714.2
        assert_eq!(plan.size, 714.2);
        // risk_amount = 714.2 × 0.15 = 107.13
        assert!((plan.risk_amount - 107.13).abs() < 1e-6);
    }

    #[test]
    fn fixed_usd_sizes_from_notional() {
        let setup = pendle_setup(); // entry 1.40
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::FixedUsd,
            entry_pct: 10.0,
            entry_fixed_usd: 100.0, // target notional = 100
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        let plan = build_plan(&input).expect("should size");
        // raw size = 100 / 1.40 = 71.42.. -> floor 1 decimal = 71.4
        assert_eq!(plan.size, 71.4);
    }

    #[test]
    fn fixed_usd_below_min_notional_is_rejected() {
        let setup = pendle_setup();
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 10_000.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::FixedUsd,
            entry_pct: 10.0,
            entry_fixed_usd: 5.0, // $5 notional < $10 minimum
            profile: RiskProfile::Moderate,
            leverage: &leverage(),
            asset_meta: &meta,
        };
        assert_eq!(build_plan(&input).unwrap_err(), SizingError::BelowMinSize(MIN_ORDER_NOTIONAL));
    }

    #[test]
    fn entry_mode_token_round_trips() {
        for mode in [EntryMode::RiskBased, EntryMode::PercentBalance, EntryMode::FixedUsd] {
            assert_eq!(EntryMode::from_str_opt(mode.as_str()), Some(mode));
        }
        assert_eq!(EntryMode::from_str_opt("nope"), None);
    }

    #[test]
    fn rejects_when_margin_exceeds_equity() {
        let setup = pendle_setup(); // entry 1.40
        let meta = AssetMeta { sz_decimals: 1, max_leverage: 10 };
        let input = SizingInput {
            setup: &setup,
            equity: 100.0,
            risk_pct: 1.0,
            entry_mode: EntryMode::FixedUsd,
            entry_pct: 10.0,
            entry_fixed_usd: 1000.0, // notional ~1000, margin ~333 > equity 100
            profile: RiskProfile::Moderate, // leverage 3
            leverage: &leverage(),
            asset_meta: &meta,
        };
        match build_plan(&input).unwrap_err() {
            SizingError::MarginExceedsEquity { .. } => {}
            other => panic!("expected MarginExceedsEquity, got {other:?}"),
        }
    }
}
