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
