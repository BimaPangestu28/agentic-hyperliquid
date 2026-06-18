//! Telegram rendering helpers and (Task 8) handlers.

use crate::config::LeverageMap;
use crate::hyperliquid::{EntryOrder, Exchange, TriggerOrder};
use crate::parser::Direction;
use crate::sizing::{build_plan, ExecutionPlan, RiskProfile, SizingError, SizingInput};
use crate::state::PendingTrade;
use std::time::Duration;
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
/// places the reduce-only bracket sized to the ACTUAL held position.
///
/// On a fill-wait timeout the resting order is cancelled (best-effort). If
/// any size was partially filled the bracket is armed on that partial size so
/// the position is never left without a stop-loss. Only when zero size was
/// filled does the function bail without placing any bracket.
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

    // Determine the size we actually hold before arming the bracket.
    // For market orders (or an immediately-filled limit) this is plan.size.
    // For a resting limit we poll until full, or handle the timeout safely.
    let effective_size = if use_limit && !entry_result.filled {
        let mut elapsed = 0u64;
        loop {
            let held = exchange.position_size(&plan.coin).await?;
            if held >= plan.size * 0.99 {
                // Treat ~full fill as full.
                break plan.size;
            }
            if elapsed >= fill_timeout_secs {
                // Timed out: cancel any resting remainder, then decide.
                if let Some(oid) = entry_result.order_id {
                    // best-effort cancel — ignore errors to avoid masking the
                    // partial-fill handling below.
                    let _ = exchange.cancel_order(&plan.coin, oid).await;
                }
                let held = exchange.position_size(&plan.coin).await?;
                if held <= 0.0 {
                    anyhow::bail!(
                        "entry limit order not filled within {fill_timeout_secs}s; \
                         order cancelled, no position opened"
                    );
                }
                // Partial fill: arm the bracket on exactly what we hold.
                break held;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            elapsed += 1;
        }
    } else {
        plan.size
    };

    // Scale factor: for a partial fill, TP sizes shrink proportionally so the
    // total closing volume never exceeds the actual position size.
    let scale = if plan.size > 0.0 { effective_size / plan.size } else { 0.0 };

    // Bracket: SL covers the full held position; each TP is scaled.
    // Closing side is always opposite the entry direction.
    let close_is_buy = !is_buy;
    exchange
        .place_trigger(&TriggerOrder {
            coin: plan.coin.clone(),
            is_buy: close_is_buy,
            size: effective_size,
            trigger_price: plan.stop_loss.price,
            is_take_profit: false,
        })
        .await?;
    for take_profit in &plan.take_profits {
        exchange
            .place_trigger(&TriggerOrder {
                coin: plan.coin.clone(),
                is_buy: close_is_buy,
                size: take_profit.size * scale,
                trigger_price: take_profit.price,
                is_take_profit: true,
            })
            .await?;
    }
    Ok(())
}

// ── Dispatcher wiring ─────────────────────────────────────────────────────────

use crate::journal::Journal;
use crate::parser::parse_setup;
use crate::state::PendingStore;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::ParseMode;

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
    // teloxide-core 0.10: Message.from is Option<User> (field, not method)
    let user_id = match message.from.as_ref() {
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

    // Confidence gate: when a gate is configured, a missing or below-threshold
    // confidence BOTH fail closed. unwrap_or(false) here means "no confidence
    // value => does not pass the gate".
    if let Some(gate) = context.config.confidence_gate {
        let passes = setup.confidence.map(|confidence| confidence >= gate).unwrap_or(false);
        if !passes {
            bot.send_message(
                message.chat.id,
                format!("Confidence {:?} does not meet gate {gate}; skipped.", setup.confidence),
            )
            .await?;
            return Ok(());
        }
    }

    let equity = context.exchange.equity().await?;
    let asset_meta = context.exchange.asset_meta(&setup.coin).await?;
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

    // Key by message id (i32 from MessageId(i32), cast to i64 for PendingStore)
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
    // teloxide-core 0.10: query.message is Option<MaybeInaccessibleMessage>
    // Use the helper regular_message() to get a &Message only when accessible.
    let message = match query.regular_message() {
        Some(message) => message.clone(),
        None => return Ok(()),
    };
    // MessageId(i32), cast to i64 for PendingStore key
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
                    bot.edit_message_text(
                        message.chat.id,
                        message.id,
                        render_summary(&plan, profile),
                    )
                    .parse_mode(ParseMode::Markdown)
                    .reply_markup(confirmation_keyboard(profile))
                    .await?;
                    bot.answer_callback_query(&query.id).await?;
                }
                Err(error) => {
                    bot.answer_callback_query(&query.id)
                        .text(format!("{error}"))
                        .show_alert(true)
                        .await?;
                }
            }
        } else {
            bot.answer_callback_query(&query.id).text("Setup expired.").await?;
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
            bot.answer_callback_query(&query.id).text("Setup expired.").await?;
            return Ok(());
        }
    };

    bot.answer_callback_query(&query.id).await?;
    bot.edit_message_text(
        message.chat.id,
        message.id,
        format!("Executing {}…", trade.plan.coin),
    )
    .await?;

    match execute_plan(
        context.exchange.as_ref(),
        &trade.plan,
        use_limit,
        context.config.entry_fill_timeout_secs,
    )
    .await
    {
        Ok(()) => {
            // Journal every attempt so a position that was opened is always
            // auditable — even when subsequent steps (e.g. bracket placement)
            // returned Ok. execute_plan signature is kept as Result<()>; order
            // id is journalled as None here (thread-out is a future task).
            let _ = context.journal.record(&trade.plan, None);
            bot.send_message(
                message.chat.id,
                format!("✅ Executed {} with SL/TP bracket.", trade.plan.coin),
            )
            .await?;
        }
        Err(error) => {
            // Journal on failure too: a partial fill may have opened a position
            // even when execute_plan returns Err, so we must leave an audit
            // trail before sending the error message.
            let _ = context.journal.record(&trade.plan, None);
            bot.send_message(
                message.chat.id,
                format!("❌ Execution failed: {error}"),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn run<E: Exchange + 'static>(
    config: crate::config::Config,
    exchange: Arc<E>,
) -> anyhow::Result<()> {
    let bot = Bot::new(&config.telegram_token);
    let context: Arc<BotContext<E>> = Arc::new(BotContext {
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

    /// When a limit entry is placed and the fill-wait times out with a partial
    /// fill (400 of 666.6), the resting remainder must be cancelled and the
    /// bracket must be sized to the ACTUAL held position, not the planned size.
    #[tokio::test]
    async fn partial_fill_timeout_cancels_remainder_and_brackets_actual_size() {
        // Simulate holding 400 units — a partial fill of the 666.6 plan size.
        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            simulated_position: std::sync::Mutex::new(Some(400.0)),
            ..Default::default()
        };
        let plan = plan(); // long, size 666.6, SL + 2 TPs
        // use_limit=true so a limit entry is placed; fill_timeout_secs=0 so the
        // loop times out immediately on the first iteration (400 < 666.6 * 0.99).
        super::execute_plan(&exchange, &plan, true, 0).await.unwrap();

        // The resting order (id=1 from MockExchange) must have been cancelled.
        let cancels = exchange.cancels.lock().unwrap();
        assert_eq!(cancels.len(), 1, "expected exactly one cancel call");

        // All three bracket legs must be sized to the actual 400 held.
        let triggers = exchange.triggers.lock().unwrap();
        assert_eq!(triggers.len(), 3);

        // SL covers the entire held position.
        let stop_loss = triggers.iter().find(|t| !t.is_take_profit).unwrap();
        assert!(
            (stop_loss.size - 400.0).abs() < 1e-6,
            "SL size should be 400.0 but was {}",
            stop_loss.size
        );

        // Each TP is scaled proportionally (scale = 400 / 666.6 ≈ 0.6001).
        let scale = 400.0_f64 / 666.6_f64;
        let take_profits: Vec<_> = triggers.iter().filter(|t| t.is_take_profit).collect();
        assert_eq!(take_profits.len(), 2);
        for (take_profit, original) in take_profits.iter().zip(plan.take_profits.iter()) {
            let expected = original.size * scale;
            assert!(
                (take_profit.size - expected).abs() < 1e-6,
                "TP size should be {expected:.6} but was {}",
                take_profit.size
            );
        }
    }
}
