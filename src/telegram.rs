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

/// Returns the button label, appending ` ✓` when the profile is active.
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
