//! Telegram rendering helpers and (Task 8) handlers.

use crate::config::LeverageMap;
use crate::hyperliquid::{EntryOrder, Exchange, TriggerOrder};
use crate::parser::Direction;
use crate::sizing::{build_plan, ExecutionPlan, RiskProfile, SizingError, SizingInput};
use crate::state::PendingTrade;
use std::time::Duration;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

const WELCOME_TEXT: &str = "\u{1F44B} Agent Hyperliquid\n\nPaste a trading-setup card and I'll size it with risk-based position sizing, then ask you to confirm before executing a long/short with SL/TP brackets on Hyperliquid.\n\nExample card:\n\nTrading setup for PENDLE\nDirection\nLONG\nTimeframe\nswing\nRisk : Reward\n2.8 : 1\nConfidence\n8/10\nSL\n$1.25\nEntry\n$1.40\nTP1\n$1.70\n60%\nTP2\n$2.00\n40%\n\nAfter you paste it: pick a risk profile (Conservative/Moderate/Aggressive), then Confirm Limit or Confirm Market -- or Cancel.\n\nCommands: /start, /help, /stats";

pub const CB_CONSERVATIVE: &str = "profile:conservative";
pub const CB_MODERATE: &str = "profile:moderate";
pub const CB_AGGRESSIVE: &str = "profile:aggressive";
pub const CB_LIMIT: &str = "confirm:limit";
pub const CB_MARKET: &str = "confirm:market";
pub const CB_CANCEL: &str = "cancel";

/// Escapes text for Telegram MarkdownV2 (every reserved char gets a backslash).
fn escape_markdown_v2(text: &str) -> String {
    const RESERVED: &[char] = &['_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!'];
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if RESERVED.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub fn render_summary(plan: &ExecutionPlan, profile: RiskProfile) -> String {
    let direction = match plan.direction {
        Direction::Long => "LONG",
        Direction::Short => "SHORT",
    };

    // Bold header: escape inner content, wrap with literal * delimiters.
    let header_inner = escape_markdown_v2(&format!("{} {}  ({:?})", plan.coin, direction, profile));
    let mut text = format!("*{}*\n", header_inner);

    text.push_str(&escape_markdown_v2(&format!(
        "Size: {} (notional ${:.2})",
        plan.size, plan.notional,
    )));
    text.push('\n');

    text.push_str(&escape_markdown_v2(&format!(
        "Entry: ${:.4}  Leverage: {}x",
        plan.entry, plan.leverage,
    )));
    text.push('\n');

    text.push_str(&escape_markdown_v2(&format!(
        "Margin: ${:.2}  Risk: ${:.2}",
        plan.margin, plan.risk_amount,
    )));
    text.push('\n');

    text.push_str(&escape_markdown_v2(&format!(
        "SL: ${:.4} (100%)  est. liq ${:.4}",
        plan.stop_loss.price, plan.liquidation_price,
    )));
    text.push('\n');

    for (index, take_profit) in plan.take_profits.iter().enumerate() {
        text.push_str(&escape_markdown_v2(&format!(
            "TP{}: ${:.4} ({})",
            index + 1,
            take_profit.price,
            take_profit.size,
        )));
        text.push('\n');
    }

    for warning in &plan.warnings {
        // Emoji is not a reserved char — concatenate it raw, escape only the warning text.
        text.push_str(&format!("⚠️ {}\n", escape_markdown_v2(warning)));
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
use crate::state::PendingStore;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::ParseMode;

pub struct BotContext<E: Exchange + 'static> {
    pub config: crate::config::Config,
    pub exchange: Arc<E>,
    pub store: Arc<PendingStore>,
    pub journal: Arc<Journal>,
    pub http: reqwest::Client,
}

/// Returns a reply for a slash-command message, or None if the text is not a
/// command (and should be parsed as a trading-setup card).
fn command_response(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    // Take the command word without args, strip a possible @botname suffix, lowercase.
    let command = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match command.as_str() {
        "/start" | "/help" => Some(WELCOME_TEXT.to_string()),
        _ => Some("Unknown command. Send /help, or paste a trading-setup card.".to_string()),
    }
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

    // Photo messages: parse the screenshot via OpenAI vision.
    // Must run BEFORE the text extraction early-return — photos have no `.text()`.
    if let Some(photos) = message.photo() {
        // Take the last (largest) photo size.
        let Some(photo) = photos.last() else {
            return Ok(());
        };
        let data_url = match download_photo_as_data_url(&bot, &photo.file.id).await {
            Ok(url) => url,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not download image: {error}")).await?;
                return Ok(());
            }
        };
        let api_key = match &context.config.openai_api_key {
            Some(key) => key,
            None => {
                bot.send_message(message.chat.id, "Image parsing needs OPENAI_API_KEY set in .env.").await?;
                return Ok(());
            }
        };
        match crate::llm_parser::parse_setups_llm_image(
            &context.http,
            &context.config.openai_base_url,
            api_key,
            &context.config.openai_vision_model,
            &data_url,
        )
        .await
        {
            Ok(setups) => {
                process_setups(&bot, &context, message.chat.id, setups).await?;
            }
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not read signal from image: {error}")).await?;
            }
        }
        return Ok(());
    }

    let text = match message.text() {
        Some(text) => text,
        None => return Ok(()),
    };

    // /stats is handled here (needs async exchange + journal) rather than in the
    // pure command_response function. Intercept before command_response so it
    // does not fall through to the "unknown command" branch.
    if text
        .trim()
        .split_whitespace()
        .next()
        .map(|w| w.split('@').next().unwrap_or("").eq_ignore_ascii_case("/stats"))
        .unwrap_or(false)
    {
        let fills = context.exchange.user_fills().await.unwrap_or_default();
        let trades = context.journal.all_trades().unwrap_or_default();
        let stats = crate::stats::compute_stats(&fills, &trades);
        // Plain text — no parse_mode — so no MarkdownV2 escaping needed.
        bot.send_message(message.chat.id, crate::stats::render_stats(&stats))
            .await?;
        return Ok(());
    }

    if let Some(reply) = command_response(text) {
        bot.send_message(message.chat.id, reply).await?;
        return Ok(());
    }

    process_signal(&bot, &context, message.chat.id, text).await
}

/// Processes a list of already-parsed `TradeSetup`s: fetches equity once, then
/// sends a sized confirmation card to `chat_id` for each valid setup.
///
/// This is factored out of `process_signal` so it can be shared with the
/// image-parsing path (which calls the OpenAI vision API instead of DeepSeek).
///
/// @param bot - The Telegram bot instance for sending messages
/// @param context - Shared bot state (config, exchange, store, journal)
/// @param chat_id - The Telegram chat that will receive the confirmation cards
/// @param setups - Pre-parsed and validated trade setups to process
/// @returns `Ok(())` on success, or an error if a critical step fails
pub async fn process_setups<E: Exchange + 'static>(
    bot: &Bot,
    context: &BotContext<E>,
    chat_id: teloxide::types::ChatId,
    setups: Vec<crate::parser::TradeSetup>,
) -> anyhow::Result<()> {
    // Fetch equity once for all signals.
    let equity = match context.exchange.equity().await {
        Ok(value) => value,
        Err(error) => {
            bot.send_message(chat_id, format!("Could not fetch account equity: {error}")).await?;
            return Ok(());
        }
    };

    if setups.len() > 1 {
        bot.send_message(
            chat_id,
            format!(
                "Found {} signals. Each is sized at {}% risk — confirming all = {:.1}% total risk.",
                setups.len(),
                context.config.risk_pct,
                context.config.risk_pct * setups.len() as f64,
            ),
        )
        .await?;
    }

    for setup in setups {
        // Confidence gate: when a gate is configured, a missing or below-threshold
        // confidence BOTH fail closed. unwrap_or(false) means "no confidence => skip".
        if let Some(gate) = context.config.confidence_gate {
            let passes = setup.confidence.map(|confidence| confidence >= gate).unwrap_or(false);
            if !passes {
                bot.send_message(
                    chat_id,
                    format!("{}: confidence {:?} below gate {gate} — skipped.", setup.coin, setup.confidence),
                )
                .await?;
                continue;
            }
        }

        // Asset listing check: Ok(Some) = listed, Ok(None) = not listed (skip gracefully),
        // Err = network failure (skip with note).
        let asset_meta = match context.exchange.asset_meta(&setup.coin).await {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                bot.send_message(
                    chat_id,
                    format!("{} is not listed as a perp on Hyperliquid — skipped.", setup.coin),
                )
                .await?;
                continue;
            }
            Err(error) => {
                bot.send_message(
                    chat_id,
                    format!("Could not load market for {}: {error} — skipped.", setup.coin),
                )
                .await?;
                continue;
            }
        };

        // Skip if we already hold a position in this coin (avoid averaging/stacking).
        match context.exchange.position_size(&setup.coin).await {
            Ok(size) if size > 0.0 => {
                bot.send_message(
                    chat_id,
                    format!(
                        "Already holding {} (size {}) — skipped to avoid stacking. Close it first to re-enter.",
                        setup.coin, size
                    ),
                )
                .await?;
                continue;
            }
            Ok(_) => {} // flat — proceed
            Err(error) => {
                bot.send_message(
                    chat_id,
                    format!("Could not verify existing position for {} ({error}) — skipped.", setup.coin),
                )
                .await?;
                continue;
            }
        }

        // Skip if there's already a resting/open order for this coin (e.g. an unfilled limit entry).
        match context.exchange.open_order_count(&setup.coin).await {
            Ok(count) if count > 0 => {
                bot.send_message(
                    chat_id,
                    format!(
                        "Already have {} open order(s) for {} — skipped. Cancel them first to re-enter.",
                        count, setup.coin
                    ),
                )
                .await?;
                continue;
            }
            Ok(_) => {} // no resting orders — proceed
            Err(error) => {
                bot.send_message(
                    chat_id,
                    format!("Could not verify open orders for {} ({error}) — skipped.", setup.coin),
                )
                .await?;
                continue;
            }
        }

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
                bot.send_message(
                    chat_id,
                    format!("{}: cannot size — {error} — skipped.", setup.coin),
                )
                .await?;
                continue;
            }
        };

        // Send a separate confirmation card per signal. Each card gets its own
        // message id as the PendingStore key → on_callback handles them independently.
        let sent = bot
            .send_message(chat_id, render_summary(&plan, profile))
            .parse_mode(ParseMode::MarkdownV2)
            .reply_markup(confirmation_keyboard(profile))
            .await?;

        context.store.insert(
            sent.id.0 as i64,
            PendingTrade { setup, equity, asset_meta, profile, plan },
        );
    }
    Ok(())
}

/// Parses a trading-setup card and sends confirmation cards to `chat_id`.
///
/// This is the core signal-processing pipeline used by the Telegram message
/// handler (parse → size → confirmation cards).
///
/// @param bot - The Telegram bot instance for sending messages
/// @param context - Shared bot state (config, exchange, store, journal)
/// @param chat_id - The Telegram chat that will receive the confirmation cards
/// @param text - The raw trading-setup card text to parse and process
/// @returns `Ok(())` on success, or an error if a critical step fails
pub async fn process_signal<E: Exchange + 'static>(
    bot: &Bot,
    context: &BotContext<E>,
    chat_id: teloxide::types::ChatId,
    text: &str,
) -> anyhow::Result<()> {
    let parse_result = match &context.config.deepseek_api_key {
        Some(api_key) => {
            let attempt = crate::llm_parser::parse_setups_llm(
                &context.http, &context.config.deepseek_base_url, api_key, &context.config.deepseek_model, text,
            );
            crate::llm_parser::parse_with_fallback(attempt, text).await
        }
        None => crate::parser::parse_setup(text).map(|s| (vec![s], crate::llm_parser::ParseSource::RegexFallback)),
    };
    let setups = match parse_result {
        Ok((setups, source)) => {
            tracing::info!(?source, count = setups.len(), "parsed setups");
            setups
        }
        Err(error) => {
            bot.send_message(chat_id, format!("Could not parse setup: {error}")).await?;
            return Ok(());
        }
    };

    process_setups(bot, context, chat_id, setups).await
}

/// Downloads a Telegram photo by file ID and returns it as a base64 data URL.
///
/// IMPORTANT: The file download URL contains the bot token and MUST NOT be
/// logged. This function intentionally avoids any tracing/logging of the URL.
///
/// @param bot - The Telegram bot instance (used to call getFile and for the token)
/// @param file_id - The `file_id` string from the `FileMeta` of the chosen `PhotoSize`
/// @returns A `data:image/jpeg;base64,...` string on success
/// @throws anyhow::Error - When the Telegram API or download fails
async fn download_photo_as_data_url(bot: &Bot, file_id: &str) -> anyhow::Result<String> {
    use base64::Engine;
    let file = bot.get_file(file_id).await?;
    // Construct the download URL using the bot token — NEVER log this URL.
    let url = format!("https://api.telegram.org/file/bot{}/{}", bot.token(), file.path);
    let response = reqwest::get(&url).await.map_err(|e| e.without_url())?;
    let response = response.error_for_status().map_err(|e| e.without_url())?;
    let bytes = response.bytes().await.map_err(|e| e.without_url())?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
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
                    .parse_mode(ParseMode::MarkdownV2)
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

    // Daily risk cap check: reject the trade if adding its risk_amount would
    // exceed max_daily_risk_pct % of equity. Must run BEFORE execute_plan and
    // BEFORE journaling so a rejected trade does not consume cap or audit log.
    if let Some(cap_pct) = context.config.max_daily_risk_pct {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let day_start = crate::risk::start_of_utc_day(now_secs);
        let used_today = context.journal.risk_used_since(day_start).unwrap_or(0.0);
        let cap_amount = trade.equity * cap_pct / 100.0;
        if !crate::risk::within_daily_cap(used_today, trade.plan.risk_amount, cap_amount) {
            bot.answer_callback_query(&query.id).await.ok();
            bot.edit_message_text(
                message.chat.id, message.id,
                format!("Daily risk cap reached: ${:.2} used + ${:.2} new > ${:.2} cap ({}%). {} skipped.",
                    used_today, trade.plan.risk_amount, cap_amount, cap_pct, trade.plan.coin),
            ).await?;
            return Ok(());
        }
    }

    bot.answer_callback_query(&query.id).await?;
    bot.edit_message_text(
        message.chat.id,
        message.id,
        format!("Executing {}…", trade.plan.coin),
    )
    .await?;

    // Capture signal metadata and timestamp before the async execute_plan call
    // so we can journal even on error. `trade` is not moved by execute_plan
    // (which only borrows &trade.plan), but extracting up-front keeps the
    // borrow checker happy and avoids re-accessing after a potential move.
    let opened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let trade_confidence = trade.setup.confidence;
    let trade_timeframe = trade.setup.timeframe.clone();
    let trade_risk_reward = trade.setup.risk_reward;
    let trade_profile = format!("{:?}", trade.profile);

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
            let _ = context.journal.record(
                &trade.plan,
                None,
                trade_confidence,
                trade_timeframe.as_deref(),
                trade_risk_reward,
                &trade_profile,
                opened_at,
            );
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
            let _ = context.journal.record(
                &trade.plan,
                None,
                trade_confidence,
                trade_timeframe.as_deref(),
                trade_risk_reward,
                &trade_profile,
                opened_at,
            );
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
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let context: Arc<BotContext<E>> = Arc::new(BotContext {
        config,
        exchange,
        store: Arc::new(PendingStore::new()),
        journal: Arc::new(Journal::open("trades.db")?),
        http,
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

    #[test]
    fn summary_escapes_markdown_v2_special_chars() {
        let text = render_summary(&plan(), RiskProfile::Moderate);
        // prices contain '.', notional/TP lines contain '(' — both must be backslash-escaped for MarkdownV2
        assert!(text.contains("\\."), "decimal points must be escaped");
        assert!(text.contains("\\("), "parentheses must be escaped");
    }

    #[test]
    fn start_and_help_return_welcome() {
        assert!(super::command_response("/start").unwrap().contains("Agent Hyperliquid"));
        assert!(super::command_response("/help").unwrap().contains("Example card"));
        assert!(super::command_response("/start@MyBot").unwrap().contains("Agent Hyperliquid"));
    }

    #[test]
    fn unknown_command_is_handled() {
        assert!(super::command_response("/foo").unwrap().to_lowercase().contains("unknown command"));
    }

    #[test]
    fn non_command_text_is_not_intercepted() {
        assert!(super::command_response("Trading setup for PENDLE").is_none());
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
