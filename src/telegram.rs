//! Telegram rendering helpers and (Task 8) handlers.

use crate::config::LeverageMap;
use crate::hyperliquid::{EntryOrder, Exchange, OpenPosition, TriggerOrder};
use crate::monitor::format_pnl;
use crate::parser::Direction;
use crate::settings::{Settings, SettingsStore};
use crate::sizing::{build_plan, EntryMode, ExecutionPlan, RiskProfile, SizingError, SizingInput};
use crate::state::PendingTrade;
use std::time::Duration;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

const WELCOME_TEXT: &str = "\u{1F44B} Agentic Hyperliquid\n\nPaste a trading-setup card and I'll size it with risk-based position sizing, then ask you to confirm before executing a long/short with SL/TP brackets on Hyperliquid.\n\nExample card:\n\nTrading setup for PENDLE\nDirection\nLONG\nTimeframe\nswing\nRisk : Reward\n2.8 : 1\nConfidence\n8/10\nSL\n$1.25\nEntry\n$1.40\nTP1\n$1.70\n60%\nTP2\n$2.00\n40%\n\nAfter you paste it: pick a risk profile (Conservative/Moderate/Aggressive), then Confirm Limit or Confirm Market -- or Cancel.\n\nCommands: /start, /help, /stats, /account, /closeall, /close <COIN>, /scan <COIN>, /watch, /settings, /set <key> <value>\n\n/closeall — tutup semua posisi\n/close <COIN> — tutup satu posisi (contoh: /close BTC)\n/scan <COIN> — analisa manual 1 coin via Neurobro di poll berikutnya (bypass cooldown & kill-switch)\n/watch — lihat watchlist & status auto-scalp · /watch add <COIN> · /watch remove <COIN>\n/set auto_scalp_enabled on|off — nyalakan/matikan auto-scalp · /set max_open_positions <n> — batas posisi terbuka";

pub const CB_CONSERVATIVE: &str = "profile:conservative";
pub const CB_MODERATE: &str = "profile:moderate";
pub const CB_AGGRESSIVE: &str = "profile:aggressive";
pub const CB_LIMIT: &str = "confirm:limit";
pub const CB_MARKET: &str = "confirm:market";
pub const CB_TRIGGER: &str = "confirm:trigger";
pub const CB_CANCEL: &str = "cancel";
pub const CB_CANCEL_FILL_PREFIX: &str = "cancel_fill:";
pub const CB_CLOSE_ALL: &str = "close_all";
pub const CB_CLOSE_ONE_PREFIX: &str = "close_one:";
pub const CB_MODE_RISK: &str = "entry_mode:risk";
pub const CB_MODE_PERCENT: &str = "entry_mode:percent";
pub const CB_MODE_FIXED: &str = "entry_mode:fixed";
pub const CB_LEV_PREFIX: &str = "lev:";

/// True when a trigger entry would fire immediately because price is already past
/// the trigger (long with trigger ≤ mark, or short with trigger ≥ mark) — i.e. it
/// would behave like a market order rather than waiting for a breakout.
#[allow(dead_code)] // TODO: wire into confirm flow once Exchange exposes a mark-price accessor
pub fn trigger_fires_immediately(direction: Direction, trigger_px: f64, mark_px: f64) -> bool {
    match direction {
        Direction::Long => trigger_px <= mark_px,
        Direction::Short => trigger_px >= mark_px,
    }
}

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

pub fn confirmation_keyboard(active: RiskProfile, trigger_enabled: bool) -> InlineKeyboardMarkup {
    // The execute row always offers Limit/Market; the Trigger button is gated off
    // by default because its stop-direction mapping is unverified on the live exchange.
    let mut execute_row = vec![
        InlineKeyboardButton::callback("✅ Confirm Limit", CB_LIMIT),
        InlineKeyboardButton::callback("⚡ Confirm Market", CB_MARKET),
    ];
    if trigger_enabled {
        execute_row.push(InlineKeyboardButton::callback("🎯 Confirm Trigger", CB_TRIGGER));
    }
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(label("Conservative", active == RiskProfile::Conservative), CB_CONSERVATIVE),
            InlineKeyboardButton::callback(label("Moderate", active == RiskProfile::Moderate), CB_MODERATE),
            InlineKeyboardButton::callback(label("Aggressive", active == RiskProfile::Aggressive), CB_AGGRESSIVE),
        ],
        execute_row,
        vec![InlineKeyboardButton::callback("❌ Cancel", CB_CANCEL)],
    ])
}

/// One-button keyboard attached to the "menunggu fill…" message so the user can
/// cancel a resting limit entry before it fills. The callback data carries the
/// coin so `on_callback` can look up the right in-flight wait.
fn fill_wait_keyboard(coin: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "❌ Batalkan order",
        format!("{CB_CANCEL_FILL_PREFIX}{coin}"),
    )]])
}

/// Sum of unrealized PnL across all positions.
fn total_unrealized_pnl(positions: &[OpenPosition]) -> f64 {
    positions.iter().map(|position| position.unrealized_pnl).sum()
}

/// One position row, shared by /account and the P&L push.
fn position_line(position: &OpenPosition) -> String {
    format!(
        "  {:<6} {:<5} {} @ ${:.2}  mark ${:.2}  uPnL ${:+.2}  {:.0}x\n",
        position.coin,
        position.direction.to_uppercase(),
        position.size,
        position.entry_px,
        position.mark_px,
        position.unrealized_pnl,
        position.leverage,
    )
}

/// Running-P&L push: total + per-position unrealized PnL.
pub fn render_pnl_summary(equity: f64, positions: &[OpenPosition]) -> String {
    let mut out = String::from("📊 Running P&L\n");
    out.push_str(&format!("Equity: ${:.2}\n", equity));
    out.push_str(&format!("Total uPnL: ${:+.2}\n", total_unrealized_pnl(positions)));
    for position in positions {
        out.push_str(&position_line(position));
    }
    out
}

/// Plain-text account card (no parse_mode). The daily-risk line is omitted
/// when the cap is disabled (`cap_pct` is `None`).
pub fn render_account(
    equity: f64,
    positions: &[OpenPosition],
    used_today: f64,
    cap_pct: Option<f64>,
) -> String {
    let mut out = String::new();
    out.push_str("💰 Account\n");
    out.push_str(&format!("Equity: ${:.2}\n", equity));
    if let Some(cap) = cap_pct {
        let cap_amount = equity * cap / 100.0;
        out.push_str(&format!(
            "Daily risk used: ${:.2} / ${:.2} ({}%)\n",
            used_today, cap_amount, cap
        ));
    }
    if positions.is_empty() {
        out.push_str("\nFlat — no open positions.\n");
        return out;
    }
    out.push_str(&format!("\nOpen positions ({}):\n", positions.len()));
    out.push_str(&format!("Total uPnL: ${:+.2}\n", total_unrealized_pnl(positions)));
    for position in positions {
        out.push_str(&position_line(position));
    }
    out
}

/// Outcome of attempting to close one position.
pub struct CloseOutcome {
    pub coin: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Confirmation prompt listing every open position and the combined uPnL.
pub fn render_close_all_prompt(positions: &[OpenPosition]) -> String {
    let mut out = String::from("⚠️ Tutup SEMUA posisi?\n\n");
    let mut total = 0.0;
    for position in positions {
        total += position.unrealized_pnl;
        out.push_str(&format!(
            "• {} {} {} — {}\n",
            position.coin,
            position.direction,
            position.size,
            format_pnl(position.unrealized_pnl),
        ));
    }
    out.push_str(&format!("\nTotal uPnL: {}", format_pnl(total)));
    out
}

/// Confirmation prompt for closing a single position.
pub fn render_close_one_prompt(position: &OpenPosition) -> String {
    format!(
        "⚠️ Tutup posisi {} {} {} — uPnL {}?",
        position.coin,
        position.direction,
        position.size,
        format_pnl(position.unrealized_pnl),
    )
}

/// Result summary after an `execute_close` run.
pub fn render_close_result(outcomes: &[CloseOutcome]) -> String {
    let mut lines = Vec::new();
    for outcome in outcomes {
        if outcome.ok {
            lines.push(format!("✅ {} ditutup", outcome.coin));
        } else {
            let reason = outcome.error.as_deref().unwrap_or("error");
            lines.push(format!("⚠️ {} gagal: {}", outcome.coin, reason));
        }
    }
    lines.join("\n")
}

/// A `[✅ <label>] [❌ Batal]` confirmation keyboard. `close_data` is the
/// callback fired by the confirm button (`CB_CLOSE_ALL` or `CB_CLOSE_ONE_PREFIX` + coin).
pub fn close_confirm_keyboard(close_data: &str, button_label: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(button_label.to_string(), close_data.to_string()),
        InlineKeyboardButton::callback("❌ Batal", CB_CANCEL),
    ]])
}

/// Closes each target position best-effort: cancels its resting orders first
/// (orphan SL/TP cleanup), then market-closes. One failure never aborts the rest.
pub async fn execute_close<E: Exchange>(exchange: &E, targets: &[OpenPosition]) -> Vec<CloseOutcome> {
    let mut outcomes = Vec::new();
    for position in targets {
        if let Err(error) = exchange.cancel_orders_for_coin(&position.coin).await {
            tracing::warn!("execute_close: cancel orders for {} failed: {error}", position.coin);
        }
        let outcome = match exchange.close_position(&position.coin, position.size).await {
            Ok(_) => CloseOutcome { coin: position.coin.clone(), ok: true, error: None },
            Err(error) => CloseOutcome { coin: position.coin.clone(), ok: false, error: Some(error.to_string()) },
        };
        outcomes.push(outcome);
    }
    outcomes
}

/// Plain-text settings card (no MarkdownV2 — sent without parse_mode).
pub fn render_settings(settings: &Settings) -> String {
    let cap = match settings.max_daily_risk_pct {
        Some(value) => format!("{value}%"),
        None => "disabled".to_string(),
    };
    let auto_scalp = if settings.auto_scalp_enabled { "ON 🟢" } else { "OFF 🔴" };
    let min_rr = if settings.min_rr > 0.0 {
        format!("{:.2}", settings.min_rr)
    } else {
        "disabled".to_string()
    };
    let blacklist = if settings.coin_blacklist.is_empty() {
        "(kosong)".to_string()
    } else {
        settings.coin_blacklist.join(", ")
    };
    format!(
        "⚙️ Settings\n\n\
         Entry mode: {}\n\
         risk_pct: {}%\n\
         entry_pct: {}%\n\
         entry_fixed_usd: ${}\n\
         max_daily_risk_pct: {}\n\
         leverage_conservative: {}x\n\
         leverage_moderate: {}x\n\
         leverage_aggressive: {}x\n\
         entry_fill_timeout_secs: {}s\n\
         trigger_expiry_secs: {}s\n\
         pnl_push_secs: {}s\n\
         auto_scalp_enabled: {}\n\
         max_open_positions: {}\n\
         min_rr: {}\n\
         coin_blacklist: {}\n\n\
         Change a number:  /set <key> <value>\n\
         e.g.  /set min_rr 1.5  ·  /set coin_blacklist XPL,ZEC\n\
         Switch entry mode with the buttons below.",
        settings.entry_mode.label(),
        settings.risk_pct,
        settings.entry_pct,
        settings.entry_fixed_usd,
        cap,
        settings.leverage.conservative,
        settings.leverage.moderate,
        settings.leverage.aggressive,
        settings.entry_fill_timeout_secs,
        settings.trigger_expiry_secs,
        settings.pnl_push_secs,
        auto_scalp,
        settings.max_open_positions,
        min_rr,
        blacklist,
    )
}

/// Returns a new list with `coin` (upper-cased) appended if absent.
pub fn add_coin(list: &[String], coin: &str) -> Vec<String> {
    let coin = coin.trim().to_uppercase();
    let mut next = list.to_vec();
    if !coin.is_empty() && !next.iter().any(|c| c.eq_ignore_ascii_case(&coin)) {
        next.push(coin);
    }
    next
}

/// Returns a new list with `coin` removed (case-insensitive).
pub fn remove_coin(list: &[String], coin: &str) -> Vec<String> {
    let coin = coin.trim();
    list.iter().filter(|c| !c.eq_ignore_ascii_case(coin)).cloned().collect()
}

/// Renders the watchlist + auto-scalp status for the `/watch` command.
pub fn render_watch(settings: &Settings) -> String {
    let coins = if settings.watchlist.is_empty() {
        "(kosong)".to_string()
    } else {
        settings.watchlist.join(", ")
    };
    let status = if settings.auto_scalp_enabled { "ON 🟢" } else { "OFF 🔴" };
    format!(
        "👁️ Watchlist auto-scalp\n\nCoin: {coins}\nAuto-scalp: {status}\nMax posisi: {}\n\n\
         /watch add <COIN>  ·  /watch remove <COIN>\n\
         /set auto_scalp_enabled on|off  ·  /set max_open_positions <n>",
        settings.max_open_positions
    )
}

/// Returns a new `LeverageMap` with `profile`'s leverage stepped by `delta`,
/// clamped to the inclusive range [1, 50]. Other profiles are unchanged.
pub fn adjust_leverage(map: &LeverageMap, profile: RiskProfile, delta: i32) -> LeverageMap {
    let mut next = *map;
    let current = match profile {
        RiskProfile::Conservative => &mut next.conservative,
        RiskProfile::Moderate => &mut next.moderate,
        RiskProfile::Aggressive => &mut next.aggressive,
    };
    let stepped = (*current as i32 + delta).clamp(1, 50);
    *current = stepped as u32;
    next
}

/// Inline keyboard: entry-mode buttons (✓ on active) plus a −/+ stepper row
/// per leverage profile showing the current value.
pub fn settings_keyboard(active: EntryMode, leverage: &LeverageMap) -> InlineKeyboardMarkup {
    let mode_row = vec![
        InlineKeyboardButton::callback(label("Risk-based", active == EntryMode::RiskBased), CB_MODE_RISK),
        InlineKeyboardButton::callback(label("% Balance", active == EntryMode::PercentBalance), CB_MODE_PERCENT),
        InlineKeyboardButton::callback(label("Fixed USD", active == EntryMode::FixedUsd), CB_MODE_FIXED),
    ];
    let lev_row = |name: &str, value: u32| {
        vec![
            InlineKeyboardButton::callback("➖".to_string(), format!("{CB_LEV_PREFIX}{name}:dec")),
            InlineKeyboardButton::callback(format!("{name} {value}x"), format!("{CB_LEV_PREFIX}{name}:noop")),
            InlineKeyboardButton::callback("➕".to_string(), format!("{CB_LEV_PREFIX}{name}:inc")),
        ]
    };
    InlineKeyboardMarkup::new(vec![
        mode_row,
        lev_row("conservative", leverage.conservative),
        lev_row("moderate", leverage.moderate),
        lev_row("aggressive", leverage.aggressive),
    ])
}

fn entry_mode_from_callback(data: &str) -> Option<EntryMode> {
    match data {
        CB_MODE_RISK => Some(EntryMode::RiskBased),
        CB_MODE_PERCENT => Some(EntryMode::PercentBalance),
        CB_MODE_FIXED => Some(EntryMode::FixedUsd),
        _ => None,
    }
}

/// Parses `lev:<profile>:<dir>` into `(RiskProfile, delta)`. `dir` is `inc`
/// (+1), `dec` (-1), or `noop` (0, a tap on the value label).
fn leverage_step_from_callback(data: &str) -> Option<(RiskProfile, i32)> {
    let rest = data.strip_prefix(CB_LEV_PREFIX)?;
    let (profile_str, dir) = rest.split_once(':')?;
    let profile = match profile_str {
        "conservative" => RiskProfile::Conservative,
        "moderate" => RiskProfile::Moderate,
        "aggressive" => RiskProfile::Aggressive,
        _ => return None,
    };
    let delta = match dir {
        "inc" => 1,
        "dec" => -1,
        "noop" => 0,
        _ => return None,
    };
    Some((profile, delta))
}

/// Recomputes the plan for a different risk profile, reusing the cached equity
/// and asset metadata captured when the card was first parsed.
pub fn recompute_plan(
    trade: &PendingTrade,
    profile: RiskProfile,
    settings: &Settings,
) -> Result<ExecutionPlan, SizingError> {
    build_plan(&SizingInput {
        setup: &trade.setup,
        equity: trade.equity,
        risk_pct: settings.risk_pct,
        entry_mode: settings.entry_mode,
        entry_pct: settings.entry_pct,
        entry_fixed_usd: settings.entry_fixed_usd,
        profile,
        leverage: &settings.leverage,
        asset_meta: &trade.asset_meta,
    })
}

/// A concise execution-progress event emitted by [`execute_plan`].
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionEvent {
    /// Entry order accepted by the exchange. `limit` distinguishes a resting
    /// limit order (awaiting fill) from a market order (filling now).
    EntrySubmitted { limit: bool, price: f64 },
    /// A fill was observed. `partial` marks a partial fill after a limit timeout.
    Filled { size: f64, partial: bool },
    /// A limit entry did not fill within the timeout. `cancelled` notes the
    /// best-effort cancel of the resting remainder.
    FillTimeout { cancelled: bool },
    /// A resting limit entry was cancelled by the user before it filled.
    /// `cancelled` notes the best-effort cancel of the resting order succeeded.
    EntryCancelled { cancelled: bool },
    /// The reduce-only SL + TP bracket was placed.
    BracketArmed { stop_loss: f64, take_profits: usize },
}

/// Receives [`ExecutionEvent`]s during [`execute_plan`]. Implementations must
/// be best-effort: a failure to deliver must not abort execution.
#[async_trait::async_trait]
pub trait ProgressReporter: Send + Sync {
    async fn report(&self, event: ExecutionEvent);
}

/// A [`ProgressReporter`] that drops every event — used by the API auto-execute
/// path, which reports via a Telegram notification instead of progress events.
pub struct NoopReporter;

#[async_trait::async_trait]
impl ProgressReporter for NoopReporter {
    async fn report(&self, _event: ExecutionEvent) {}
}

/// Formats an [`ExecutionEvent`] into the Indonesian message shown to the user.
pub fn format_execution_event(coin: &str, timeout_secs: u64, event: &ExecutionEvent) -> String {
    match event {
        ExecutionEvent::EntrySubmitted { limit: true, price } => {
            format!("⏳ Limit {coin} @ ${price:.4} dipasang, menunggu fill…")
        }
        ExecutionEvent::EntrySubmitted { limit: false, .. } => {
            format!("⏳ Entry {coin} dikirim…")
        }
        ExecutionEvent::Filled { size, partial: false } => {
            format!("✅ {coin} terisi (size {size}).")
        }
        ExecutionEvent::Filled { size, partial: true } => {
            format!("⚠️ Partial fill {coin}: bracket dipasang di size {size}.")
        }
        ExecutionEvent::FillTimeout { cancelled: true } => {
            format!("❌ Limit {coin} tak terisi dalam {timeout_secs}s — dibatalkan, tidak ada posisi.")
        }
        ExecutionEvent::FillTimeout { cancelled: false } => {
            format!("⚠️ Limit {coin} tak terisi dalam {timeout_secs}s dan pembatalan TIDAK terkonfirmasi — cek manual, mungkin masih ada order tersisa.")
        }
        ExecutionEvent::EntryCancelled { cancelled: true } => {
            format!("❌ Order {coin} dibatalkan — tidak ada posisi.")
        }
        ExecutionEvent::EntryCancelled { cancelled: false } => {
            format!("⚠️ Pembatalan order {coin} TIDAK terkonfirmasi — cek manual, mungkin masih ada order tersisa.")
        }
        ExecutionEvent::BracketArmed { stop_loss, take_profits } => {
            format!("✅ SL/TP {coin} terpasang (SL ${stop_loss:.4}, {take_profits} TP).")
        }
    }
}

/// Production [`ProgressReporter`] that forwards each event to a Telegram chat.
/// Send failures are logged and swallowed so execution never aborts mid-bracket.
struct TelegramReporter {
    bot: Bot,
    chat_id: teloxide::types::ChatId,
    coin: String,
    timeout_secs: u64,
}

#[async_trait::async_trait]
impl ProgressReporter for TelegramReporter {
    async fn report(&self, event: ExecutionEvent) {
        let text = format_execution_event(&self.coin, self.timeout_secs, &event);
        let send = self.bot.send_message(self.chat_id, text);
        let result = if matches!(event, ExecutionEvent::EntrySubmitted { limit: true, .. }) {
            send.reply_markup(fill_wait_keyboard(&self.coin)).await
        } else {
            send.await
        };
        if let Err(error) = result {
            tracing::warn!("progress notification failed: {error}");
        }
    }
}

/// Places the reduce-only bracket: SL covering the full held size, and each TP
/// scaled by `effective_size/planned_size` (for partial fills). Closing side is
/// opposite `direction`. Shared by limit/market execution and the trigger monitor.
pub async fn arm_bracket<E: Exchange + ?Sized>(
    exchange: &E,
    coin: &str,
    direction: Direction,
    planned_size: f64,
    effective_size: f64,
    stop_loss: f64,
    take_profits: &[crate::sizing::BracketLeg],
) -> anyhow::Result<()> {
    let scale = if planned_size > 0.0 { effective_size / planned_size } else { 0.0 };
    let close_is_buy = !matches!(direction, Direction::Long);
    exchange
        .place_trigger(&TriggerOrder {
            coin: coin.to_string(),
            is_buy: close_is_buy,
            size: effective_size,
            trigger_price: stop_loss,
            is_take_profit: false,
        })
        .await?;
    for take_profit in take_profits {
        exchange
            .place_trigger(&TriggerOrder {
                coin: coin.to_string(),
                is_buy: close_is_buy,
                size: take_profit.size * scale,
                trigger_price: take_profit.price,
                is_take_profit: true,
            })
            .await?;
    }
    Ok(())
}

/// Sets leverage, places the entry order, waits for fill (limit only), then
/// places the reduce-only bracket sized to the ACTUAL held position.
///
/// On a fill-wait timeout the resting order is cancelled (best-effort). If
/// any size was partially filled the bracket is armed on that partial size so
/// the position is never left without a stop-loss. Only when zero size was
/// filled does the function bail without placing any bracket.
///
/// The `cancel` signal (fired by the cancel button via the pending_fills
/// registry) ends the wait early: the resting order is cancelled, then if zero
/// size filled the function returns `Ok(())` (no position, no bracket —
/// distinct from the timeout path which bails), or if partially filled the
/// bracket is armed on the held size. A market execution never triggers this
/// signal; the `cancel` parameter is still required by the function signature.
pub async fn execute_plan<E: Exchange + ?Sized>(
    exchange: &E,
    plan: &ExecutionPlan,
    use_limit: bool,
    fill_timeout_secs: u64,
    cancel: std::sync::Arc<tokio::sync::Notify>,
    reporter: &dyn ProgressReporter,
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
    reporter
        .report(ExecutionEvent::EntrySubmitted { limit: use_limit, price: plan.entry })
        .await;

    // Determine the size we actually hold before arming the bracket.
    // For market orders (or an immediately-filled limit) this is plan.size.
    // For a resting limit we poll until full, or handle the timeout safely.
    let effective_size = if use_limit && !entry_result.filled {
        let mut elapsed = 0u64;
        loop {
            let held = exchange.position_size(&plan.coin).await?;
            if held >= plan.size * 0.99 {
                // Treat ~full fill as full.
                reporter.report(ExecutionEvent::Filled { size: plan.size, partial: false }).await;
                break plan.size;
            }

            // Wait up to 1s for a fill, unless the user cancels first. When the
            // timeout has already elapsed we skip the wait and wind down as a timeout.
            let timed_out = elapsed >= fill_timeout_secs;
            let user_cancelled = if timed_out {
                false
            } else {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => { elapsed += 1; false }
                    _ = cancel.notified() => true,
                }
            };

            if timed_out || user_cancelled {
                // Single canceller: this loop owns order_id, so the timeout and the
                // button can never both cancel.
                let cancelled = match entry_result.order_id {
                    Some(oid) => exchange.cancel_order(&plan.coin, oid).await.is_ok(),
                    None => false,
                };
                let held = exchange.position_size(&plan.coin).await?;
                if held >= plan.size * 0.99 {
                    // Filled at the instant of the tap/timeout — arm the full bracket.
                    reporter.report(ExecutionEvent::Filled { size: plan.size, partial: false }).await;
                    break plan.size;
                }
                if held <= 0.0 {
                    if user_cancelled {
                        reporter.report(ExecutionEvent::EntryCancelled { cancelled }).await;
                        return Ok(());
                    }
                    reporter.report(ExecutionEvent::FillTimeout { cancelled }).await;
                    anyhow::bail!(
                        "entry limit order not filled within {fill_timeout_secs}s; \
                         order cancelled, no position opened"
                    );
                }
                reporter.report(ExecutionEvent::Filled { size: held, partial: true }).await;
                break held;
            }
            // Slept 1s without a fill or a cancel — poll again.
        }
    } else {
        reporter.report(ExecutionEvent::Filled { size: plan.size, partial: false }).await;
        plan.size
    };

    arm_bracket(
        exchange,
        &plan.coin,
        plan.direction,
        plan.size,
        effective_size,
        plan.stop_loss.price,
        &plan.take_profits,
    )
    .await?;
    reporter
        .report(ExecutionEvent::BracketArmed {
            stop_loss: plan.stop_loss.price,
            take_profits: plan.take_profits.len(),
        })
        .await;
    Ok(())
}

// ── Dispatcher wiring ─────────────────────────────────────────────────────────

use crate::journal::Journal;
use crate::state::PendingStore;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use teloxide::prelude::*;
use teloxide::types::ParseMode;

pub struct BotContext<E: Exchange + 'static> {
    pub config: crate::config::Config,
    pub exchange: Arc<E>,
    pub store: Arc<PendingStore>,
    pub journal: Arc<Journal>,
    pub settings: Arc<Mutex<Settings>>,
    pub settings_store: Arc<SettingsStore>,
    pub triggers: Arc<crate::trigger_store::TriggerStore>,
    /// Queue of manual `/scan <COIN>` requests drained by the scraper's `/manual-scans` poll.
    pub manual_scans: Arc<crate::manual_scan_store::ManualScanStore>,
    pub http: reqwest::Client,
    /// Coin (upper-cased) → cancel signal for an in-flight resting-limit fill-wait.
    /// Lets the cancel button signal the `execute_plan` loop that owns the order id.
    pub pending_fills: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

/// First whitespace-separated token of `text`, lowercased, with any @botname
/// suffix stripped. Returns "" when there is no token.
fn first_command_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Returns a reply for a slash-command message, or None if the text is not a
/// command (and should be parsed as a trading-setup card).
fn command_response(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    // Take the command word without args, strip a possible @botname suffix, lowercase.
    let command = text
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

    // /account — live equity + open positions + daily-risk-used. Like /stats,
    // it needs the async exchange + journal + settings, so it is handled here.
    if text
        .split_whitespace()
        .next()
        .map(|w| w.split('@').next().unwrap_or("").eq_ignore_ascii_case("/account"))
        .unwrap_or(false)
    {
        let equity = match context.exchange.equity().await {
            Ok(value) => value,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not fetch account state: {error}")).await?;
                return Ok(());
            }
        };
        let positions = match context.exchange.positions().await {
            Ok(value) => value,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not fetch account state: {error}")).await?;
                return Ok(());
            }
        };
        let cap_pct = context.settings.lock().unwrap().max_daily_risk_pct;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let used_today = context
            .journal
            .risk_used_since(crate::risk::start_of_utc_day(now_secs))
            .unwrap_or(0.0);
        bot.send_message(message.chat.id, render_account(equity, &positions, used_today, cap_pct))
            .await?;
        return Ok(());
    }

    // /closeall — confirm, then flatten every open position.
    if first_command_word(text) == "/closeall" {
        let positions = match context.exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not fetch positions: {error}")).await?;
                return Ok(());
            }
        };
        if positions.is_empty() {
            bot.send_message(message.chat.id, "Tidak ada posisi terbuka.").await?;
            return Ok(());
        }
        bot.send_message(message.chat.id, render_close_all_prompt(&positions))
            .reply_markup(close_confirm_keyboard(CB_CLOSE_ALL, "✅ Tutup semua"))
            .await?;
        return Ok(());
    }

    // /close <COIN> — confirm, then flatten a single position.
    if first_command_word(text) == "/close" {
        let coin_arg = text.split_whitespace().nth(1).map(|c| c.to_uppercase());
        let coin = match coin_arg {
            Some(coin) if !coin.is_empty() => coin,
            _ => {
                bot.send_message(message.chat.id, "Pakai: /close <COIN>  (contoh: /close BTC)").await?;
                return Ok(());
            }
        };
        let positions = match context.exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => {
                bot.send_message(message.chat.id, format!("Could not fetch positions: {error}")).await?;
                return Ok(());
            }
        };
        match positions.iter().find(|p| p.coin.eq_ignore_ascii_case(&coin)) {
            Some(position) => {
                let close_data = format!("{}{}", CB_CLOSE_ONE_PREFIX, position.coin);
                bot.send_message(message.chat.id, render_close_one_prompt(position))
                    .reply_markup(close_confirm_keyboard(&close_data, &format!("✅ Tutup {}", position.coin)))
                    .await?;
            }
            None => {
                bot.send_message(message.chat.id, format!("Tidak ada posisi {coin} terbuka.")).await?;
            }
        }
        return Ok(());
    }

    // /settings — show current values + entry-mode buttons.
    if first_command_word(text) == "/settings" {
        let settings = context.settings.lock().unwrap().clone();
        bot.send_message(message.chat.id, render_settings(&settings))
            .reply_markup(settings_keyboard(settings.entry_mode, &settings.leverage))
            .await?;
        return Ok(());
    }

    // /watch [add|remove <COIN>] — manage the auto-scalp watchlist.
    if first_command_word(text) == "/watch" {
        let mut parts = text.split_whitespace();
        let _cmd = parts.next();
        let action = parts.next().map(|a| a.to_lowercase());
        let coin = parts.next();
        match (action.as_deref(), coin) {
            (Some("add"), Some(coin)) => {
                let next = {
                    let mut guard = context.settings.lock().unwrap();
                    guard.watchlist = add_coin(&guard.watchlist, coin);
                    guard.clone()
                };
                if let Err(error) = context.settings_store.persist(&next) {
                    tracing::warn!(%error, "failed to persist watchlist add");
                }
                bot.send_message(message.chat.id, render_watch(&next)).await?;
            }
            (Some("remove"), Some(coin)) => {
                let next = {
                    let mut guard = context.settings.lock().unwrap();
                    guard.watchlist = remove_coin(&guard.watchlist, coin);
                    guard.clone()
                };
                if let Err(error) = context.settings_store.persist(&next) {
                    tracing::warn!(%error, "failed to persist watchlist remove");
                }
                bot.send_message(message.chat.id, render_watch(&next)).await?;
            }
            (None, _) => {
                let settings = context.settings.lock().unwrap().clone();
                bot.send_message(message.chat.id, render_watch(&settings)).await?;
            }
            _ => {
                bot.send_message(message.chat.id, "Pakai: /watch  ·  /watch add <COIN>  ·  /watch remove <COIN>").await?;
            }
        }
        return Ok(());
    }

    // /set <key> <value> — validate, persist, confirm.
    if first_command_word(text) == "/set" {
        let mut parts = text.split_whitespace();
        parts.next(); // skip "/set"
        let key = parts.next();
        let value = parts.collect::<Vec<_>>().join(" ");
        match key {
            None => {
                bot.send_message(message.chat.id,
                    "Usage: /set <key> <value> — send /settings to see the keys.").await?;
            }
            Some(key) => {
                let current = context.settings.lock().unwrap().clone();
                match crate::settings::apply_setting(&current, key, &value) {
                    Ok(next) => {
                        if let Err(error) = context.settings_store.persist(&next) {
                            bot.send_message(message.chat.id,
                                format!("Could not save setting: {error}")).await?;
                            return Ok(());
                        }
                        *context.settings.lock().unwrap() = next.clone();
                        bot.send_message(message.chat.id, render_settings(&next)).await?;
                    }
                    Err(error_message) => {
                        bot.send_message(message.chat.id, error_message).await?;
                    }
                }
            }
        }
        return Ok(());
    }

    // /scan <COIN> — queue a one-off manual analysis. The scraper drains the queue on its
    // next poll and runs it bypassing the per-coin cooldown and the auto-scalp kill-switch
    // (a manual override); the confidence, position-cap and margin gates still apply.
    if first_command_word(text) == "/scan" {
        let coin_arg = text.split_whitespace().nth(1).map(|c| c.to_uppercase());
        let coin = match coin_arg {
            Some(coin) if !coin.is_empty() => coin,
            _ => {
                bot.send_message(message.chat.id, "Pakai: /scan <COIN>  (contoh: /scan PENDLE)").await?;
                return Ok(());
            }
        };
        match context.manual_scans.enqueue(&coin) {
            Ok(_) => {
                bot.send_message(
                    message.chat.id,
                    format!("🔍 {coin} masuk antrian scan manual — scraper akan analisa di poll berikutnya (bypass cooldown & kill-switch)."),
                )
                .await?;
            }
            Err(error) => {
                tracing::warn!(%error, %coin, "failed to enqueue manual scan");
                bot.send_message(message.chat.id, format!("Gagal antri scan {coin}: {error}")).await?;
            }
        }
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
        let settings = context.settings.lock().unwrap().clone();
        let message = match settings.entry_mode {
            EntryMode::RiskBased => format!(
                "Found {} signals. Each sized at {}% risk — confirming all = {:.1}% total risk.",
                setups.len(), settings.risk_pct, settings.risk_pct * setups.len() as f64,
            ),
            EntryMode::PercentBalance => format!(
                "Found {} signals. Each sized at {}% of balance.",
                setups.len(), settings.entry_pct,
            ),
            EntryMode::FixedUsd => format!(
                "Found {} signals. Each sized at ${:.2} notional.",
                setups.len(), settings.entry_fixed_usd,
            ),
        };
        bot.send_message(chat_id, message).await?;
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

        // Position stacking is allowed: a new signal on a coin you already hold —
        // or one that still has a resting/unfilled order — opens an additional
        // position instead of being skipped.

        let profile = RiskProfile::Moderate;
        let settings = context.settings.lock().unwrap().clone();
        let plan = match build_plan(&SizingInput {
            setup: &setup,
            equity,
            risk_pct: settings.risk_pct,
            entry_mode: settings.entry_mode,
            entry_pct: settings.entry_pct,
            entry_fixed_usd: settings.entry_fixed_usd,
            profile,
            leverage: &settings.leverage,
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
            .reply_markup(confirmation_keyboard(profile, context.config.trigger_entry_enabled))
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

/// Runs `execute_close` on `targets`, renders the result, and edits `message_id` in place.
///
/// Shared by the `CB_CLOSE_ALL` and `CB_CLOSE_ONE_PREFIX` confirm handlers to avoid
/// duplicating the execute → render → edit sequence.
async fn run_close_and_render<E: Exchange>(
    bot: &Bot,
    exchange: &E,
    chat_id: teloxide::types::ChatId,
    message_id: teloxide::types::MessageId,
    targets: &[OpenPosition],
) -> anyhow::Result<()> {
    let outcomes = execute_close(exchange, targets).await;
    bot.edit_message_text(chat_id, message_id, render_close_result(&outcomes)).await?;
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

    // Authorization guard: mirror on_message's allowlist check.
    // CallbackQuery.from is User (non-Option) in teloxide-core 0.10.
    let caller_id = query.from.id.0 as i64;
    if !context.config.is_allowed(caller_id) {
        return Ok(());
    }

    // Entry-mode switch from the /settings keyboard.
    if let Some(mode) = entry_mode_from_callback(&data) {
        let next = {
            let mut guard = context.settings.lock().unwrap();
            guard.entry_mode = mode;
            guard.clone()
        };
        if let Err(error) = context.settings_store.persist(&next) {
            tracing::warn!(%error, "failed to persist entry_mode change");
        }
        bot.edit_message_text(message.chat.id, message.id, render_settings(&next))
            .reply_markup(settings_keyboard(mode, &next.leverage))
            .await?;
        bot.answer_callback_query(&query.id).await?;
        return Ok(());
    }

    // Leverage stepper from the /settings keyboard.
    if let Some((profile, delta)) = leverage_step_from_callback(&data) {
        if delta == 0 {
            // Tap on the value label — nothing to change.
            bot.answer_callback_query(&query.id).await?;
            return Ok(());
        }
        let next = {
            let mut guard = context.settings.lock().unwrap();
            guard.leverage = adjust_leverage(&guard.leverage, profile, delta);
            guard.clone()
        };
        if let Err(error) = context.settings_store.persist(&next) {
            tracing::warn!(%error, "failed to persist leverage change");
        }
        bot.edit_message_text(message.chat.id, message.id, render_settings(&next))
            .reply_markup(settings_keyboard(next.entry_mode, &next.leverage))
            .await?;
        bot.answer_callback_query(&query.id).await?;
        return Ok(());
    }

    // Profile switch: recompute and edit the message in place.
    if let Some(profile) = profile_from_callback(&data) {
        if let Some(mut trade) = context.store.get(key) {
            let settings = context.settings.lock().unwrap().clone();
            match recompute_plan(&trade, profile, &settings) {
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
                    .reply_markup(confirmation_keyboard(profile, context.config.trigger_entry_enabled))
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

    // Cancel a resting limit entry that is still waiting to fill. The button lives
    // on the "menunggu fill…" message, so `message` IS that message — edit it to
    // drop the keyboard. The execute_plan loop performs the actual cancel_order.
    if let Some(coin) = data.strip_prefix(CB_CANCEL_FILL_PREFIX) {
        let signal = context
            .pending_fills
            .lock()
            .unwrap()
            .get(&coin.to_uppercase())
            .cloned();
        match signal {
            Some(notify) => {
                notify.notify_one();
                bot.answer_callback_query(&query.id).text("Membatalkan…").await?;
                bot.edit_message_text(
                    message.chat.id,
                    message.id,
                    format!("⏳ Membatalkan order {coin}…"),
                )
                .await?;
            }
            None => {
                bot.answer_callback_query(&query.id)
                    .text(format!("Order {coin} sudah tidak aktif."))
                    .await?;
                let _ = bot
                    .edit_message_reply_markup(message.chat.id, message.id)
                    .await;
            }
        }
        return Ok(());
    }

    // Confirm: close ALL positions. Re-fetch for freshness (book may have moved).
    if data == CB_CLOSE_ALL {
        bot.answer_callback_query(&query.id).text("Menutup semua…").await?;
        let positions = match context.exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => {
                bot.edit_message_text(message.chat.id, message.id, format!("Gagal ambil posisi: {error}")).await?;
                return Ok(());
            }
        };
        if positions.is_empty() {
            bot.edit_message_text(message.chat.id, message.id, "Tidak ada posisi terbuka.").await?;
            return Ok(());
        }
        run_close_and_render(&bot, context.exchange.as_ref(), message.chat.id, message.id, &positions).await?;
        return Ok(());
    }

    // Confirm: close a single named position.
    if let Some(coin) = data.strip_prefix(CB_CLOSE_ONE_PREFIX) {
        bot.answer_callback_query(&query.id).text(format!("Menutup {coin}…")).await?;
        let positions = match context.exchange.positions().await {
            Ok(positions) => positions,
            Err(error) => {
                bot.edit_message_text(message.chat.id, message.id, format!("Gagal ambil posisi: {error}")).await?;
                return Ok(());
            }
        };
        match positions.into_iter().find(|p| p.coin.eq_ignore_ascii_case(coin)) {
            Some(position) => {
                run_close_and_render(&bot, context.exchange.as_ref(), message.chat.id, message.id, &[position]).await?;
            }
            None => {
                bot.edit_message_text(message.chat.id, message.id, format!("Posisi {coin} sudah tidak ada.")).await?;
            }
        }
        return Ok(());
    }

    // Defense-in-depth: reject a trigger confirm even if a stale keyboard still
    // shows the button while the feature is disabled.
    if data == CB_TRIGGER && !context.config.trigger_entry_enabled {
        bot.answer_callback_query(&query.id).text("Trigger entry dinonaktifkan").await?;
        return Ok(());
    }

    enum Confirm { Limit, Market, Trigger }
    let confirm = match data.as_str() {
        CB_LIMIT => Confirm::Limit,
        CB_MARKET => Confirm::Market,
        CB_TRIGGER => Confirm::Trigger,
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
    let cap_pct_opt = context.settings.lock().unwrap().max_daily_risk_pct;
    if let Some(cap_pct) = cap_pct_opt {
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

    // Read the live timeout before spawning (never hold the lock across await).
    let fill_timeout_secs = context.settings.lock().unwrap().entry_fill_timeout_secs;

    // Reserve this trade's risk in the journal synchronously at confirm time so a
    // concurrent confirm's daily-cap check sees it immediately — execution runs in a
    // background task that may take up to the fill timeout to finish. Journalled once
    // here regardless of the eventual execution outcome (order id is recorded later).
    let opened_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    // The trigger path may early-return before any order is placed (set_leverage or
    // place_trigger_entry failure), so it must NOT reserve cap up front — it records
    // only after a successful placement. Limit/Market reserve here as before.
    if !matches!(confirm, Confirm::Trigger) {
        let _ = context.journal.record(
            &trade.plan,
            None,
            trade.setup.confidence,
            trade.setup.timeframe.as_deref(),
            trade.setup.risk_reward,
            &format!("{:?}", trade.profile),
            opened_at,
        );
    }

    let chat_id = message.chat.id;

    // Trigger path: place a resting trigger entry on the exchange, persist it to
    // TriggerStore, and return — the trigger monitor (run_trigger_monitor) will arm
    // the bracket once the fill lands, or cancel on expiry.
    if let Confirm::Trigger = confirm {
        bot.edit_message_text(
            message.chat.id,
            message.id,
            format!("Memasang trigger {}…", trade.plan.coin),
        )
        .await?;

        let is_buy = matches!(trade.plan.direction, Direction::Long);
        let expiry_secs = context.settings.lock().unwrap().trigger_expiry_secs;

        if let Err(error) = context.exchange.set_leverage(&trade.plan.coin, trade.plan.leverage).await {
            bot.send_message(chat_id, format!("❌ Gagal set leverage: {error}")).await.ok();
            return Ok(());
        }

        let placed = context.exchange.place_trigger_entry(
            &trade.plan.coin, is_buy, trade.plan.size, trade.plan.entry,
        ).await;

        match placed {
            Ok(result) => {
                let take_profits: Vec<crate::trigger_store::PendingLeg> = trade.setup.take_profits.iter()
                    .map(|tp| crate::trigger_store::PendingLeg { price: tp.price, alloc_pct: tp.allocation_pct })
                    .collect();
                let pending = crate::trigger_store::PendingTrigger {
                    id: 0,
                    coin: trade.plan.coin.clone(),
                    direction: format!("{:?}", trade.plan.direction),
                    size: trade.plan.size,
                    trigger_px: trade.plan.entry,
                    leverage: trade.plan.leverage,
                    stop_loss: trade.plan.stop_loss.price,
                    take_profits,
                    entry_oid: result.order_id,
                    chat_id: chat_id.0,
                    created_at: opened_at,
                    expiry_at: opened_at + expiry_secs as i64,
                    status: "active".into(),
                };
                if result.order_id.is_none() {
                    tracing::warn!("trigger {} placed without an order id; fill detection will fall back to coin match", trade.plan.coin);
                }
                // Reserve cap only now that the trigger entry is actually on the book.
                let _ = context.journal.record(
                    &trade.plan,
                    result.order_id,
                    trade.setup.confidence,
                    trade.setup.timeframe.as_deref(),
                    trade.setup.risk_reward,
                    &format!("{:?}", trade.profile),
                    opened_at,
                );
                let _ = context.triggers.insert(&pending);
                bot.send_message(
                    chat_id,
                    format!("🎯 Trigger {} @ ${:.4} dipasang — nunggu harga menembus.", trade.plan.coin, trade.plan.entry),
                ).await.ok();
            }
            Err(error) => {
                bot.send_message(chat_id, format!("❌ Gagal pasang trigger: {error}")).await.ok();
            }
        }
        return Ok(());
    }

    // Limit / Market path: edit message then spawn execution.
    bot.edit_message_text(
        message.chat.id,
        message.id,
        format!("Executing {}…", trade.plan.coin),
    )
    .await?;

    let use_limit = matches!(confirm, Confirm::Limit);
    let task_context = context.clone();
    let task_bot = bot.clone();

    let cancel = Arc::new(Notify::new());
    let coin_key = trade.plan.coin.to_uppercase();

    // Only register a cancellable wait when a resting limit order will be placed.
    // Market executions never show a cancel button, so their entry in the registry
    // would be a phantom that could confuse a concurrent cancel_fill callback.
    if use_limit {
        context
            .pending_fills
            .lock()
            .unwrap()
            .insert(coin_key.clone(), cancel.clone());
    }

    tokio::spawn(async move {
        let reporter = TelegramReporter {
            bot: task_bot.clone(),
            chat_id,
            coin: trade.plan.coin.clone(),
            timeout_secs: fill_timeout_secs,
        };

        let outcome = execute_plan(
            task_context.exchange.as_ref(),
            &trade.plan,
            use_limit,
            fill_timeout_secs,
            cancel,
            &reporter,
        )
        .await;

        // Clear the registry entry only when one was inserted (limit path).
        if use_limit {
            task_context.pending_fills.lock().unwrap().remove(&coin_key);
        }

        if let Err(error) = outcome {
            if let Err(send_error) = task_bot
                .send_message(chat_id, format!("❌ Execution failed: {error}"))
                .await
            {
                tracing::warn!("failed to send execution error: {send_error}");
            }
        }
    });
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
    // Extract journal_path before `config` is moved into BotContext. Settings
    // share the same SQLite file as the journal (separate connection + tables).
    let journal_path = config.journal_path.clone();
    let settings_store = Arc::new(SettingsStore::open(&journal_path)?);
    let seeded = settings_store.load(Settings::from_config(&config))?;
    // Single shared TriggerStore — BotContext and run_trigger_monitor both use this Arc.
    let trigger_store = Arc::new(crate::trigger_store::TriggerStore::open(&journal_path)?);
    // Manual-scan queue: /scan enqueues here, the scraper drains it via /manual-scans. Same
    // SQLite file as the journal/settings (separate connection + table).
    let manual_scan_store = Arc::new(crate::manual_scan_store::ManualScanStore::open(&journal_path)?);
    let context: Arc<BotContext<E>> = Arc::new(BotContext {
        config,
        exchange,
        store: Arc::new(PendingStore::new()),
        journal: Arc::new(Journal::open(&journal_path)?),
        settings: Arc::new(Mutex::new(seeded)),
        settings_store,
        triggers: trigger_store.clone(),
        manual_scans: manual_scan_store,
        http,
        pending_fills: Arc::new(Mutex::new(HashMap::new())),
    });

    // Background close (TP/SL) notifications. Uses its own Journal connection
    // (separate SQLite handle) so it never contends with the bot's writes.
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let monitor_journal = Arc::new(Journal::open(&journal_path)?);
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        let monitor_poll_secs = context.config.monitor_poll_secs;
        tokio::spawn(async move {
            crate::monitor::run_fill_monitor(
                monitor_bot,
                monitor_exchange,
                monitor_journal,
                monitor_user_ids,
                monitor_poll_secs,
            )
            .await;
        });
    }

    // Background trigger-entry monitor: arms SL/TP when the trigger fill lands,
    // or cancels and notifies on expiry. Uses the same shared TriggerStore Arc as
    // BotContext so inserts from on_callback are immediately visible to the monitor.
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let monitor_trigger_store = trigger_store.clone();
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        let monitor_poll_secs = context.config.monitor_poll_secs;
        tokio::spawn(async move {
            crate::monitor::run_trigger_monitor(
                monitor_bot, monitor_exchange, monitor_trigger_store, monitor_user_ids, monitor_poll_secs,
            ).await;
        });
    }

    // Background P&L push: periodically sends running unrealized-P&L summary to
    // all allowed users while positions are open. Interval is read live from
    // settings each tick; 0 disables without restarting the task.
    {
        let monitor_bot = bot.clone();
        let monitor_exchange = context.exchange.clone();
        let monitor_settings = context.settings.clone();
        let monitor_user_ids = context.config.allowed_user_ids.clone();
        tokio::spawn(async move {
            crate::monitor::run_pnl_monitor(
                monitor_bot, monitor_exchange, monitor_settings, monitor_user_ids,
            ).await;
        });
    }

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
        let markup = confirmation_keyboard(RiskProfile::Aggressive, true);
        let first_row = &markup.inline_keyboard[0];
        assert!(first_row[2].text.contains('✓')); // Aggressive marked
        assert!(!first_row[0].text.contains('✓'));
    }

    #[test]
    fn confirm_keyboard_gates_trigger_button() {
        use teloxide::types::InlineKeyboardButtonKind;
        let has_trigger = |markup: &InlineKeyboardMarkup| {
            markup.inline_keyboard.iter().flatten().any(|button| {
                matches!(&button.kind, InlineKeyboardButtonKind::CallbackData(data) if data == CB_TRIGGER)
            })
        };
        let with = confirmation_keyboard(RiskProfile::Moderate, true);
        let without = confirmation_keyboard(RiskProfile::Moderate, false);
        assert!(has_trigger(&with), "trigger button must be present when enabled");
        assert!(!has_trigger(&without), "trigger button must be absent when disabled");
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
        assert!(super::command_response("/start").unwrap().contains("Agentic Hyperliquid"));
        assert!(super::command_response("/help").unwrap().contains("Example card"));
        assert!(super::command_response("/start@MyBot").unwrap().contains("Agentic Hyperliquid"));
    }

    #[test]
    fn unknown_command_is_handled() {
        assert!(super::command_response("/foo").unwrap().to_lowercase().contains("unknown command"));
    }

    #[test]
    fn non_command_text_is_not_intercepted() {
        assert!(super::command_response("Trading setup for PENDLE").is_none());
    }

    #[test]
    fn settings_card_lists_keys_and_mode() {
        let settings = crate::settings::Settings {
            entry_mode: EntryMode::PercentBalance,
            risk_pct: 1.0,
            entry_pct: 10.0,
            entry_fixed_usd: 50.0,
            max_daily_risk_pct: Some(5.0),
            leverage: crate::config::LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
            entry_fill_timeout_secs: 300,
            trigger_expiry_secs: 14400,
            pnl_push_secs: 900,
            watchlist: Vec::new(),
            auto_scalp_enabled: false,
            max_open_positions: 5,
            min_rr: 0.0,
            coin_blacklist: Vec::new(),
        };
        let text = super::render_settings(&settings);
        assert!(text.contains("% Balance"));
        assert!(text.contains("entry_pct"));
        assert!(text.contains("max_daily_risk_pct"));
    }

    #[test]
    fn disabled_cap_renders_as_disabled() {
        let settings = crate::settings::Settings {
            entry_mode: EntryMode::RiskBased,
            risk_pct: 1.0, entry_pct: 10.0, entry_fixed_usd: 50.0,
            max_daily_risk_pct: None,
            leverage: crate::config::LeverageMap { conservative: 2, moderate: 3, aggressive: 5 },
            entry_fill_timeout_secs: 300,
            trigger_expiry_secs: 14400,
            pnl_push_secs: 900,
            watchlist: Vec::new(),
            auto_scalp_enabled: false,
            max_open_positions: 5,
            min_rr: 0.0,
            coin_blacklist: Vec::new(),
        };
        assert!(super::render_settings(&settings).contains("disabled"));
    }

    #[test]
    fn settings_keyboard_marks_active_mode() {
        use crate::config::LeverageMap;
        let leverage = LeverageMap { conservative: 5, moderate: 10, aggressive: 20 };
        let markup = super::settings_keyboard(EntryMode::FixedUsd, &leverage);
        let row = &markup.inline_keyboard[0];
        assert!(row[2].text.contains('✓')); // Fixed USD marked
        assert!(!row[0].text.contains('✓'));
    }

    #[test]
    fn entry_mode_callback_parses() {
        assert_eq!(super::entry_mode_from_callback(super::CB_MODE_PERCENT), Some(EntryMode::PercentBalance));
        assert_eq!(super::entry_mode_from_callback("nope"), None);
    }

    #[test]
    fn leverage_callback_parses_profile_and_delta() {
        use crate::sizing::RiskProfile;
        assert_eq!(super::leverage_step_from_callback("lev:moderate:inc"), Some((RiskProfile::Moderate, 1)));
        assert_eq!(super::leverage_step_from_callback("lev:conservative:dec"), Some((RiskProfile::Conservative, -1)));
        assert_eq!(super::leverage_step_from_callback("lev:aggressive:noop"), Some((RiskProfile::Aggressive, 0)));
        assert_eq!(super::leverage_step_from_callback("lev:bogus:inc"), None);
        assert_eq!(super::leverage_step_from_callback("entry_mode:risk"), None);
    }

    #[test]
    fn adjust_leverage_steps_and_clamps() {
        use crate::config::LeverageMap;
        use crate::sizing::RiskProfile;
        let base = LeverageMap { conservative: 5, moderate: 10, aggressive: 20 };

        let up = super::adjust_leverage(&base, RiskProfile::Moderate, 1);
        assert_eq!(up.moderate, 11);
        assert_eq!(up.conservative, 5); // others untouched

        let down = super::adjust_leverage(&base, RiskProfile::Conservative, -1);
        assert_eq!(down.conservative, 4);

        // clamp floor at 1
        let floor = LeverageMap { conservative: 1, moderate: 10, aggressive: 20 };
        assert_eq!(super::adjust_leverage(&floor, RiskProfile::Conservative, -1).conservative, 1);

        // clamp ceiling at 50
        let ceil = LeverageMap { conservative: 5, moderate: 10, aggressive: 50 };
        assert_eq!(super::adjust_leverage(&ceil, RiskProfile::Aggressive, 1).aggressive, 50);
    }

    #[test]
    fn settings_keyboard_includes_leverage_callbacks() {
        use crate::config::LeverageMap;
        use teloxide::types::InlineKeyboardButtonKind;
        let leverage = LeverageMap { conservative: 5, moderate: 10, aggressive: 20 };
        let keyboard = super::settings_keyboard(EntryMode::RiskBased, &leverage);
        let has_lev_button = keyboard.inline_keyboard.iter().flatten().any(|button| {
            matches!(&button.kind, InlineKeyboardButtonKind::CallbackData(data) if data.starts_with(super::CB_LEV_PREFIX))
        });
        assert!(has_lev_button, "settings keyboard must include leverage stepper buttons");
    }

    fn sample_positions() -> Vec<crate::hyperliquid::OpenPosition> {
        vec![
            crate::hyperliquid::OpenPosition {
                coin: "BTC".into(), direction: "long".into(), size: 0.05, entry_px: 61200.0,
                mark_px: 61500.0, unrealized_pnl: 45.2, leverage: 3.0, notional: 3075.0,
            },
            crate::hyperliquid::OpenPosition {
                coin: "ETH".into(), direction: "short".into(), size: 1.2, entry_px: 3410.0,
                mark_px: 3400.0, unrealized_pnl: -12.8, leverage: 2.0, notional: 4080.0,
            },
        ]
    }

    #[test]
    fn account_card_lists_positions_and_cap() {
        let text = super::render_account(1234.56, &sample_positions(), 12.3, Some(5.0));
        assert!(text.contains("Equity: $1234.56"));
        assert!(text.contains("Daily risk used:"));
        assert!(text.contains("Open positions (2):"));
        assert!(text.contains("BTC"));
        assert!(text.contains("LONG"));
        assert!(text.contains("SHORT"));
        assert!(text.contains("uPnL $+45.20")); // positive sign
        assert!(text.contains("uPnL $-12.80")); // negative sign
        assert!(text.contains("mark $61500.00")); // mark price shown
        assert!(text.contains("3x")); // leverage rendered without decimals
    }

    #[test]
    fn account_card_flat_when_no_positions() {
        let text = super::render_account(1000.0, &[], 0.0, Some(5.0));
        assert!(text.contains("Flat"));
        assert!(!text.contains("Open positions"));
    }

    #[test]
    fn account_card_omits_daily_risk_when_cap_disabled() {
        let text = super::render_account(1000.0, &sample_positions(), 0.0, None);
        assert!(!text.contains("Daily risk used"));
    }

    use crate::hyperliquid::mock::MockExchange;
    use crate::sizing::AssetMeta;

    #[derive(Default)]
    struct RecordingReporter {
        events: std::sync::Mutex<Vec<super::ExecutionEvent>>,
    }

    #[async_trait::async_trait]
    impl super::ProgressReporter for RecordingReporter {
        async fn report(&self, event: super::ExecutionEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn execute_plan_sets_leverage_then_entry_then_brackets() {
        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            ..Default::default()
        };
        let plan = plan(); // long, size 666.6, 2 TPs
        let reporter = RecordingReporter::default();
        super::execute_plan(&exchange, &plan, false, 1, std::sync::Arc::new(tokio::sync::Notify::new()), &reporter).await.unwrap();

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
        let reporter = RecordingReporter::default();
        super::execute_plan(&exchange, &plan, true, 0, std::sync::Arc::new(tokio::sync::Notify::new()), &reporter).await.unwrap();

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

    #[tokio::test]
    async fn execute_plan_reports_market_event_sequence() {
        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            ..Default::default()
        };
        let plan = plan();
        let reporter = RecordingReporter::default();
        super::execute_plan(&exchange, &plan, false, 1, std::sync::Arc::new(tokio::sync::Notify::new()), &reporter).await.unwrap();

        let events = reporter.events.lock().unwrap();
        assert!(matches!(events.first(), Some(super::ExecutionEvent::EntrySubmitted { limit: false, .. })));
        assert!(events.iter().any(|e| matches!(e, super::ExecutionEvent::Filled { partial: false, .. })));
        assert!(matches!(events.last(), Some(super::ExecutionEvent::BracketArmed { take_profits: 2, .. })));
    }

    #[test]
    fn format_execution_event_renders_indonesian_copy() {
        let armed = super::ExecutionEvent::BracketArmed { stop_loss: 280.0, take_profits: 2 };
        let text = super::format_execution_event("TAO", 300, &armed);
        assert!(text.contains("TAO"));
        assert!(text.contains("SL/TP"));
    }

    #[test]
    fn format_execution_event_renders_entry_cancelled_copy() {
        let cancelled = super::ExecutionEvent::EntryCancelled { cancelled: true };
        let text = super::format_execution_event("AERO", 300, &cancelled);
        assert!(text.contains("AERO"));
        assert!(text.contains("dibatalkan"));
        assert!(text.contains("tidak ada posisi"));

        let uncertain = super::ExecutionEvent::EntryCancelled { cancelled: false };
        let warn = super::format_execution_event("AERO", 300, &uncertain);
        assert!(warn.contains("AERO"));
        assert!(warn.contains("TIDAK terkonfirmasi"));
    }

    #[tokio::test]
    async fn arm_bracket_places_sl_and_scaled_tps() {
        let exchange = MockExchange { meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }), ..Default::default() };
        let tps = vec![BracketLeg { price: 1.70, size: 60.0 }, BracketLeg { price: 2.00, size: 40.0 }];
        // planned 100, effective 100 → no scaling
        super::arm_bracket(&exchange, "PENDLE", Direction::Long, 100.0, 100.0, 1.25, &tps).await.unwrap();
        let triggers = exchange.triggers.lock().unwrap();
        assert_eq!(triggers.len(), 3);
        assert!(triggers.iter().all(|t| !t.is_buy)); // closing a long => sell
        let sl = triggers.iter().find(|t| !t.is_take_profit).unwrap();
        assert!((sl.size - 100.0).abs() < 1e-6);
    }

    #[test]
    fn trigger_fires_immediately_detects_wrong_side() {
        use crate::parser::Direction;
        // Long: fires immediately if trigger <= mark (price already at/above trigger).
        assert!(super::trigger_fires_immediately(Direction::Long, 68.53, 69.00));
        assert!(!super::trigger_fires_immediately(Direction::Long, 68.53, 68.00));
        // Short: fires immediately if trigger >= mark.
        assert!(super::trigger_fires_immediately(Direction::Short, 68.53, 68.00));
        assert!(!super::trigger_fires_immediately(Direction::Short, 68.53, 69.00));
    }

    #[test]
    fn render_pnl_summary_totals_unrealized() {
        // sample_positions(): BTC uPnL=45.20, ETH uPnL=-12.80 => total=+32.40
        let text = super::render_pnl_summary(1234.56, &sample_positions());
        assert!(text.contains("Total uPnL: $+32.40"));
        assert!(text.contains("BTC"));
        assert!(text.contains("ETH"));
    }

    #[test]
    fn account_card_shows_total_upnl_when_positions_present() {
        // sample_positions(): BTC uPnL=45.20, ETH uPnL=-12.80 => total=+32.40
        let text = super::render_account(1234.56, &sample_positions(), 0.0, Some(10.0));
        assert!(text.contains("Total uPnL: $+32.40"));
    }

    #[test]
    fn fill_wait_keyboard_has_single_cancel_button_for_coin() {
        let keyboard = super::fill_wait_keyboard("AERO");
        let rows = &keyboard.inline_keyboard;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        let button = &rows[0][0];
        match &button.kind {
            teloxide::types::InlineKeyboardButtonKind::CallbackData(data) => {
                assert_eq!(data, "cancel_fill:AERO");
            }
            other => panic!("expected callback button, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn user_cancel_at_zero_fill_cancels_order_and_arms_no_bracket() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            simulated_position: std::sync::Mutex::new(Some(0.0)), // resting, unfilled
            ..Default::default()
        };
        let plan = plan(); // long, size 666.6
        let cancel = Arc::new(Notify::new());
        cancel.notify_one(); // store a permit so the first select fires immediately
        let reporter = RecordingReporter::default();

        // Long timeout (100s) so only the cancel signal — not a timeout — ends the wait.
        super::execute_plan(&exchange, &plan, true, 100, cancel, &reporter).await.unwrap();

        assert_eq!(exchange.cancels.lock().unwrap().len(), 1, "resting order must be cancelled");
        assert_eq!(exchange.triggers.lock().unwrap().len(), 0, "no bracket when nothing filled");
        let events = reporter.events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(e, super::ExecutionEvent::EntryCancelled { cancelled: true })));
    }

    #[tokio::test]
    async fn user_cancel_at_partial_fill_cancels_remainder_and_brackets_held() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            simulated_position: std::sync::Mutex::new(Some(400.0)), // partial of 666.6
            ..Default::default()
        };
        let plan = plan();
        let cancel = Arc::new(Notify::new());
        cancel.notify_one();
        let reporter = RecordingReporter::default();

        super::execute_plan(&exchange, &plan, true, 100, cancel, &reporter).await.unwrap();

        assert_eq!(exchange.cancels.lock().unwrap().len(), 1, "resting remainder cancelled");
        let triggers = exchange.triggers.lock().unwrap();
        assert_eq!(triggers.len(), 3, "SL + 2 TP armed on the held size");
        let stop_loss = triggers.iter().find(|t| !t.is_take_profit).unwrap();
        assert!((stop_loss.size - 400.0).abs() < 1e-6);
    }

    #[test]
    fn cancel_fill_callback_round_trips_coin() {
        let data = format!("{}{}", super::CB_CANCEL_FILL_PREFIX, "AERO");
        assert_eq!(data.strip_prefix(super::CB_CANCEL_FILL_PREFIX), Some("AERO"));
    }

    #[tokio::test]
    async fn cancel_after_full_fill_is_a_no_op() {
        use std::sync::Arc;
        use tokio::sync::Notify;

        let exchange = MockExchange {
            equity: 10_000.0,
            meta: Some(AssetMeta { sz_decimals: 1, max_leverage: 10 }),
            simulated_position: std::sync::Mutex::new(Some(666.6)), // already fully filled
            ..Default::default()
        };
        let plan = plan();
        let cancel = Arc::new(Notify::new());
        cancel.notify_one();
        let reporter = RecordingReporter::default();

        super::execute_plan(&exchange, &plan, true, 100, cancel, &reporter).await.unwrap();

        assert_eq!(exchange.cancels.lock().unwrap().len(), 0, "nothing to cancel after a full fill");
        assert_eq!(exchange.triggers.lock().unwrap().len(), 3, "full bracket armed");
        let events = reporter.events.lock().unwrap();
        assert!(!events.iter().any(|e| matches!(e, super::ExecutionEvent::EntryCancelled { .. })),
            "must not report a cancel when the order filled");
    }

    #[test]
    fn close_all_prompt_lists_each_position_and_total() {
        let text = super::render_close_all_prompt(&sample_positions());
        // sample_positions has BTC and ETH (see account tests)
        assert!(text.contains("BTC"));
        assert!(text.contains("ETH"));
        assert!(text.to_lowercase().contains("total"));
    }

    #[test]
    fn close_one_prompt_names_the_coin() {
        let position = sample_positions().into_iter().next().unwrap();
        let text = super::render_close_one_prompt(&position);
        assert!(text.contains(&position.coin));
    }

    #[test]
    fn close_result_marks_success_and_failure() {
        let outcomes = vec![
            super::CloseOutcome { coin: "BTC".into(), ok: true, error: None },
            super::CloseOutcome { coin: "SOL".into(), ok: false, error: Some("boom".into()) },
        ];
        let text = super::render_close_result(&outcomes);
        assert!(text.contains("BTC"));
        assert!(text.contains("SOL"));
        assert!(text.contains("✅"));
        assert!(text.contains("⚠️"));
    }

    #[tokio::test]
    async fn execute_close_cancels_then_closes_each_target() {
        use crate::hyperliquid::testing::MockExchange;
        let exchange = MockExchange::default();
        exchange.open_orders.lock().unwrap().push("BTC".to_string()); // an orphan SL/TP
        let targets = sample_positions(); // BTC, ETH
        let outcomes = super::execute_close(&exchange, &targets).await;
        assert!(outcomes.iter().all(|o| o.ok));
        // BTC's resting order was cancelled
        assert_eq!(exchange.cancels.lock().unwrap().len(), 1);
        // both positions were closed
        assert_eq!(exchange.closes.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn execute_close_is_best_effort_on_failure() {
        use crate::hyperliquid::testing::MockExchange;
        let exchange = MockExchange::default();
        exchange.fail_close_coins.lock().unwrap().push("BTC".to_string());
        let targets = sample_positions(); // BTC (fails), ETH (ok)
        let outcomes = super::execute_close(&exchange, &targets).await;
        let btc = outcomes.iter().find(|o| o.coin == "BTC").unwrap();
        let eth = outcomes.iter().find(|o| o.coin == "ETH").unwrap();
        assert!(!btc.ok);
        assert!(eth.ok);
    }

    #[test]
    fn add_coin_normalizes_and_dedupes() {
        let list = vec!["BTC".to_string()];
        assert_eq!(super::add_coin(&list, "eth"), vec!["BTC".to_string(), "ETH".to_string()]);
        assert_eq!(super::add_coin(&list, "btc"), vec!["BTC".to_string()]); // dedupe, case-insensitive
    }

    #[test]
    fn remove_coin_is_case_insensitive() {
        let list = vec!["BTC".to_string(), "ETH".to_string()];
        assert_eq!(super::remove_coin(&list, "btc"), vec!["ETH".to_string()]);
        assert_eq!(super::remove_coin(&list, "sol"), list); // absent → unchanged
    }

    #[test]
    fn render_watch_shows_coins_and_switch() {
        let mut s = crate::settings::sample();
        s.watchlist = vec!["BTC".into(), "ETH".into()];
        s.auto_scalp_enabled = true;
        let text = super::render_watch(&s);
        assert!(text.contains("BTC") && text.contains("ETH"));
        assert!(text.to_lowercase().contains("auto"));
    }
}
