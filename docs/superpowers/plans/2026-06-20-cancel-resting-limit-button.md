# Cancel-Resting-Limit Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Telegram button that cancels an unfilled resting limit entry while it waits to fill, releasing the per-coin resting-order guard so a new setup can be re-sent.

**Architecture:** A shared `pending_fills` registry on `BotContext` maps each coin to a `tokio::sync::Notify`. The fill-wait loop inside `execute_plan` (a spawned task) selects on that signal; when the button is pressed, `on_callback` fires the signal and the loop — which alone holds the resting `order_id` — performs the actual `cancel_order`. One canceller, no double-cancel race with the timeout path.

**Tech Stack:** Rust, tokio (`sync` feature already enabled), teloxide 0.13.

## Global Constraints

- No new crate dependency — use `tokio::sync::Notify` (the `sync` feature is already enabled in `Cargo.toml`).
- User-facing copy is Indonesian, consistent with `format_execution_event`.
- Registry `Mutex` is locked with `.unwrap()`, matching existing `std::sync::Mutex` usage in `src/telegram.rs`.
- Partial-fill-on-cancel behaviour: cancel the resting remainder, arm the bracket on the held size (position is never left without a stop-loss).
- The button lives only on the limit "menunggu fill…" message; the market path has no button.
- Registry key is the coin upper-cased (`coin.to_uppercase()`), applied consistently at insert, lookup, and removal.

---

### Task 1: `EntryCancelled` execution event + Indonesian copy

**Files:**
- Modify: `src/telegram.rs` — `ExecutionEvent` enum (around line 263-276), `format_execution_event` (around line 286-310)
- Test: `src/telegram.rs` `mod tests` (alongside `format_execution_event_renders_indonesian_copy`, ~line 1524)

**Interfaces:**
- Produces: `ExecutionEvent::EntryCancelled { cancelled: bool }` — emitted by `execute_plan` (Task 3) when the user cancels a resting limit before it fills.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/telegram.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib format_execution_event_renders_entry_cancelled_copy`
Expected: FAIL to compile — `no variant named EntryCancelled`.

- [ ] **Step 3: Add the enum variant**

In the `ExecutionEvent` enum, after the `FillTimeout { cancelled: bool }` variant:

```rust
    /// A resting limit entry was cancelled by the user before it filled.
    /// `cancelled` notes the best-effort cancel of the resting order succeeded.
    EntryCancelled { cancelled: bool },
```

- [ ] **Step 4: Add the formatting arms**

In `format_execution_event`'s `match`, after the `FillTimeout { cancelled: false }` arm:

```rust
        ExecutionEvent::EntryCancelled { cancelled: true } => {
            format!("❌ Order {coin} dibatalkan — tidak ada posisi.")
        }
        ExecutionEvent::EntryCancelled { cancelled: false } => {
            format!("⚠️ Pembatalan order {coin} TIDAK terkonfirmasi — cek manual, mungkin masih ada order tersisa.")
        }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib format_execution_event`
Expected: PASS (both the new test and the existing `format_execution_event_renders_indonesian_copy`).

- [ ] **Step 6: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): add EntryCancelled execution event + copy"
```

---

### Task 2: Cancel-button callback constant + `fill_wait_keyboard`

**Files:**
- Modify: `src/telegram.rs` — callback constants (around line 13-22), a new keyboard helper near `confirmation_keyboard` (around line 105-124)
- Test: `src/telegram.rs` `mod tests`

**Interfaces:**
- Produces:
  - `pub const CB_CANCEL_FILL_PREFIX: &str = "cancel_fill:"`
  - `fn fill_wait_keyboard(coin: &str) -> InlineKeyboardMarkup` — one button, callback data `format!("{CB_CANCEL_FILL_PREFIX}{coin}")`. Consumed by the reporter (Task 4) and parsed by `on_callback` (Task 4).

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/telegram.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib fill_wait_keyboard_has_single_cancel_button_for_coin`
Expected: FAIL to compile — `cannot find function fill_wait_keyboard`.

- [ ] **Step 3: Add the constant**

Next to the other `CB_*` constants (after `pub const CB_CANCEL: &str = "cancel";`):

```rust
pub const CB_CANCEL_FILL_PREFIX: &str = "cancel_fill:";
```

- [ ] **Step 4: Add the keyboard helper**

After `confirmation_keyboard` (around line 124):

```rust
/// One-button keyboard attached to the "menunggu fill…" message so the user can
/// cancel a resting limit entry before it fills. The callback data carries the
/// coin so `on_callback` can look up the right in-flight wait.
fn fill_wait_keyboard(coin: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "❌ Batalkan order",
        format!("{CB_CANCEL_FILL_PREFIX}{coin}"),
    )]])
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib fill_wait_keyboard_has_single_cancel_button_for_coin`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): add cancel-fill callback constant + keyboard"
```

---

### Task 3: `execute_plan` accepts a cancel signal and selects on it

**Files:**
- Modify: `src/telegram.rs` — `execute_plan` signature + fill-wait loop (lines 375-450)
- Modify: `src/telegram.rs` — existing `execute_plan` call sites in `mod tests` (lines ~1447, ~1475, ~1516) to pass the new argument
- Test: `src/telegram.rs` `mod tests` (new cancel tests using `MockExchange` + `RecordingReporter`)

**Interfaces:**
- Consumes: `ExecutionEvent::EntryCancelled` (Task 1).
- Produces: new signature
  ```rust
  pub async fn execute_plan<E: Exchange>(
      exchange: &E,
      plan: &ExecutionPlan,
      use_limit: bool,
      fill_timeout_secs: u64,
      cancel: std::sync::Arc<tokio::sync::Notify>,
      reporter: &dyn ProgressReporter,
  ) -> anyhow::Result<()>
  ```
  The spawn wiring (Task 4) passes the registered `Notify`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/telegram.rs` (near the other `execute_plan_*` tests). Note `MockExchange::position_size` sums placed entry sizes when `simulated_position` is `None`, so these tests set `simulated_position` explicitly to control the held size. Firing `notify_one()` before the call stores a permit, so the very first loop iteration's `notified()` is ready — deterministic, no sleeps:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib user_cancel_at_zero_fill_cancels_order_and_arms_no_bracket`
Expected: FAIL to compile — `execute_plan` takes 5 arguments, not 6.

- [ ] **Step 3: Update the `execute_plan` signature**

Change the signature (line 375) to add the `cancel` parameter before `reporter`:

```rust
pub async fn execute_plan<E: Exchange>(
    exchange: &E,
    plan: &ExecutionPlan,
    use_limit: bool,
    fill_timeout_secs: u64,
    cancel: std::sync::Arc<tokio::sync::Notify>,
    reporter: &dyn ProgressReporter,
) -> anyhow::Result<()> {
```

- [ ] **Step 4: Replace the fill-wait loop body**

Replace the whole `let effective_size = if use_limit && !entry_result.filled { ... } else { ... };` block (lines 399-431) with the version below. It selects on the cancel signal, and folds the timeout and user-cancel paths into one shared wind-down. The user-cancel-at-zero path `return Ok(())` (no bracket, no spurious "Execution failed"); the timeout-at-zero path keeps the existing `bail!`:

```rust
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
```

- [ ] **Step 5: Update the three existing test call sites**

In `mod tests`, add `use std::sync::Arc; use tokio::sync::Notify;` where needed and pass a fresh `Notify` to each existing `execute_plan` call:

- `execute_plan_sets_leverage_then_entry_then_brackets` (~1447):
  ```rust
  super::execute_plan(&exchange, &plan, false, 1, std::sync::Arc::new(tokio::sync::Notify::new()), &reporter).await.unwrap();
  ```
- `partial_fill_timeout_cancels_remainder_and_brackets_actual_size` (~1475):
  ```rust
  super::execute_plan(&exchange, &plan, true, 0, std::sync::Arc::new(tokio::sync::Notify::new()), &reporter).await.unwrap();
  ```
- `execute_plan_reports_market_event_sequence` (~1516):
  ```rust
  super::execute_plan(&exchange, &plan, false, 1, std::sync::Arc::new(tokio::sync::Notify::new()), &reporter).await.unwrap();
  ```

- [ ] **Step 6: Run the full execute_plan test set**

Run: `cargo test --lib execute_plan; cargo test --lib user_cancel; cargo test --lib cancel_after_full_fill; cargo test --lib partial_fill_timeout`
Expected: PASS for all (3 new cancel tests + 3 existing tests still green).

- [ ] **Step 7: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): execute_plan selects on a cancel signal"
```

---

### Task 4: Registry, reporter keyboard, callback handler, and spawn wiring

**Files:**
- Modify: `src/telegram.rs` — `BotContext` struct (line 460-469), `run()` context construction (line 1176-1185), `TelegramReporter` (line 314-329), `on_callback` (insert handler near line 972), spawn block (line 1129-1157)
- Test: `src/telegram.rs` `mod tests` — a parsing round-trip test for the callback data

**Interfaces:**
- Consumes: `CB_CANCEL_FILL_PREFIX`, `fill_wait_keyboard` (Task 2); `EntryCancelled` (Task 1); `execute_plan`'s new `cancel` parameter (Task 3).
- Produces: `BotContext.pending_fills: Arc<Mutex<HashMap<String, Arc<Notify>>>>`.

- [ ] **Step 1: Write the failing test**

The callback parsing is the one piece with pure logic worth a direct test. Add to `mod tests`:

```rust
#[test]
fn cancel_fill_callback_round_trips_coin() {
    let data = format!("{}{}", super::CB_CANCEL_FILL_PREFIX, "AERO");
    assert_eq!(data.strip_prefix(super::CB_CANCEL_FILL_PREFIX), Some("AERO"));
}
```

- [ ] **Step 2: Run test to verify it passes once the constant exists**

Run: `cargo test --lib cancel_fill_callback_round_trips_coin`
Expected: PASS (the constant already exists from Task 2; this test pins the contract the handler relies on).

- [ ] **Step 3: Add imports and the `pending_fills` field to `BotContext`**

At the top of `src/telegram.rs`, ensure these are in scope (add what is missing):

```rust
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::Notify;
```

In the `BotContext<E>` struct (after `http`):

```rust
    /// Coin (upper-cased) → cancel signal for an in-flight resting-limit fill-wait.
    /// Lets the cancel button signal the `execute_plan` loop that owns the order id.
    pub pending_fills: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
```

- [ ] **Step 4: Initialise the field in `run()`**

In the `BotContext { ... }` literal (line 1176-1185), after `http,`:

```rust
        pending_fills: Arc::new(Mutex::new(HashMap::new())),
```

- [ ] **Step 5: Attach the keyboard on the limit "menunggu fill" event**

Replace `TelegramReporter::report` (line 322-329) so the resting-limit submission carries the cancel button while every other event stays a plain message:

```rust
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
```

- [ ] **Step 6: Handle the cancel button in `on_callback`**

Immediately after the existing `if data == CB_CANCEL { ... }` block (line 972-976), add:

```rust
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
```

- [ ] **Step 7: Register the signal before spawning and clean up after**

Replace the spawn block (line 1129-1157) so it registers a `Notify`, passes it to `execute_plan`, and always removes the registry entry on completion:

```rust
    let use_limit = matches!(confirm, Confirm::Limit);
    let task_context = context.clone();
    let task_bot = bot.clone();

    let cancel = Arc::new(Notify::new());
    let coin_key = trade.plan.coin.to_uppercase();
    context
        .pending_fills
        .lock()
        .unwrap()
        .insert(coin_key.clone(), cancel.clone());

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

        // Always clear the registry entry, whatever the outcome.
        task_context.pending_fills.lock().unwrap().remove(&coin_key);

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
```

- [ ] **Step 8: Build and run the full suite**

Run: `cargo build && cargo test --lib`
Expected: clean build (no unused-import or arity warnings) and all tests pass, including Tasks 1-3 and `cancel_fill_callback_round_trips_coin`.

- [ ] **Step 9: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): cancel-resting-limit button end-to-end wiring"
```

---

## Manual verification (after Task 4)

Automated tests cannot exercise the live Telegram round-trip. After the suite is green, do a manual smoke test against the bot (testnet or small size):

1. Paste a setup whose limit entry will rest unfilled (entry far from mark). Confirm Limit.
2. Confirm the "⏳ Limit … menunggu fill…" message shows a **❌ Batalkan order** button.
3. Tap it → message becomes "⏳ Membatalkan order …" → then "❌ Order … dibatalkan — tidak ada posisi."
4. Re-send a different setup for the **same coin** → it is accepted (no "Already have 1 open order(s)" rejection), proving the guard was released.
5. Tap a stale button (after it already filled/cancelled) → toast "Order … sudah tidak aktif." and the button disappears.

## Self-Review notes

- **Spec coverage:** registry (Task 4), loop select (Task 3), partial-fill safety (Task 3), `EntryCancelled` copy (Task 1), keyboard on limit submit (Task 4), button handler + stale-tap + edit-message (Task 4), full-fill-at-tap race (Task 3 test), `cancel_order` failure → `cancelled: false` (Task 3 loop / Task 1 copy) — all covered.
- **Divergence from spec, intentional:** user-cancel-at-zero returns `Ok(())` rather than `bail!`, to avoid a spurious "❌ Execution failed" follow-up message. Documented in Task 3 Step 4.
