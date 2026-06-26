# Close Positions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `/closeall` and `/close <COIN>` Telegram commands that flatten open Hyperliquid positions behind a confirmation button, cancelling any orphaned SL/TP orders.

**Architecture:** Add two methods to the `Exchange` trait (`close_position` via the SDK's `market_close`, and `cancel_orders_for_coin`). Wire two manual commands in `on_message` that show a confirmation keyboard, and two callback handlers in `on_callback` that run a shared best-effort `execute_close` helper. Pure render functions are unit-tested; execution is tested against `MockExchange`.

**Tech Stack:** Rust, `teloxide` (Telegram), `hyperliquid_rust_sdk` 0.6.0, `tokio`, `anyhow`, `async-trait`.

## Global Constraints

- The `Exchange` trait is the ONLY network-touching seam; every method has a `MockExchange` counterpart (`src/hyperliquid/mod.rs`).
- Commands are matched manually by their first word in `on_message` (no `BotCommands` enum), cf. `/account` at `src/telegram.rs:654`.
- Callbacks are matched in `on_callback` by `data` string/prefix, cf. `CB_CANCEL_FILL_PREFIX` at `src/telegram.rs:1042`.
- User messages are Indonesian; match the existing copy style (e.g. "Tidak ada posisi terbuka.").
- Plain-text messages use no `parse_mode` (avoid MarkdownV2 escaping), cf. the `/account` handler.
- Closing always flattens the FULL position (no partial closes).
- Run `cargo test` after each task; the repo's tests are colocated `#[cfg(test)]` modules.

---

### Task 1: `Exchange` trait — `close_position` + `cancel_orders_for_coin` (mock first)

**Files:**
- Modify: `src/hyperliquid/mod.rs` — trait (`~line 96-122`), `MockExchange` struct (`~line 150-174`) + impl (`~line 206-295`), tests (`~line 322+`).

**Interfaces:**
- Produces:
  - `async fn close_position(&self, coin: &str, size: f64) -> anyhow::Result<OrderResult>` — market-closes the full `size` for `coin`; the SDK auto-detects the side.
  - `async fn cancel_orders_for_coin(&self, coin: &str) -> anyhow::Result<usize>` — cancels every resting order for `coin`; returns the count cancelled.
  - `MockExchange.closes: Mutex<Vec<(String, f64)>>` — records `close_position` calls.
  - `MockExchange.fail_close_coins: Mutex<Vec<String>>` — coins for which `close_position` returns `Err` (test hook for best-effort behaviour).

- [ ] **Step 1: Add the two trait methods**

In `src/hyperliquid/mod.rs`, inside `pub trait Exchange`, after `position_size` (line ~106):

```rust
    /// Market-closes (flattens) the full open position `size` for `coin`.
    /// The exchange auto-detects the closing side from the live position.
    async fn close_position(&self, coin: &str, size: f64) -> anyhow::Result<OrderResult>;
    /// Cancels every resting order for `coin` (e.g. orphaned SL/TP triggers).
    /// Returns the number of orders cancelled. Best-effort: a per-order failure
    /// is logged and skipped, not propagated.
    async fn cancel_orders_for_coin(&self, coin: &str) -> anyhow::Result<usize>;
```

- [ ] **Step 2: Add mock fields**

In the `MockExchange` struct, after the `trigger_entries` field (line ~173):

```rust
        /// Records every `close_position` call as `(coin, size)`.
        pub closes: Mutex<Vec<(String, f64)>>,
        /// Coins for which `close_position` returns Err (best-effort test hook).
        pub fail_close_coins: Mutex<Vec<String>>,
```

- [ ] **Step 3: Write the failing mock tests**

In the `#[cfg(test)] mod mock` tests block (after `mock_records_trigger_entry`, line ~384):

```rust
    #[tokio::test]
    async fn mock_records_close_position() {
        let exchange = MockExchange::default();
        let result = exchange.close_position("BTC", 0.5).await.unwrap();
        assert!(result.filled);
        let closes = exchange.closes.lock().unwrap();
        assert_eq!(closes.len(), 1);
        assert_eq!(closes[0], ("BTC".to_string(), 0.5));
    }

    #[tokio::test]
    async fn mock_close_position_can_be_forced_to_fail() {
        let exchange = MockExchange::default();
        exchange.fail_close_coins.lock().unwrap().push("SOL".to_string());
        assert!(exchange.close_position("SOL", 1.0).await.is_err());
        assert!(exchange.close_position("BTC", 1.0).await.is_ok());
    }

    #[tokio::test]
    async fn mock_cancel_orders_for_coin_counts_matching() {
        let exchange = MockExchange::default();
        exchange.open_orders.lock().unwrap().push("ETH".to_string());
        exchange.open_orders.lock().unwrap().push("eth".to_string());
        exchange.open_orders.lock().unwrap().push("BTC".to_string());
        assert_eq!(exchange.cancel_orders_for_coin("ETH").await.unwrap(), 2);
        // each cancelled order is also recorded in `cancels`
        assert_eq!(exchange.cancels.lock().unwrap().len(), 2);
    }
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --lib mock_records_close_position mock_close_position_can_be_forced_to_fail mock_cancel_orders_for_coin_counts_matching`
Expected: FAIL — `no method named close_position` / `cancel_orders_for_coin` on `MockExchange`.

- [ ] **Step 5: Implement the mock methods**

In `impl Exchange for MockExchange`, after the `position_size` impl (line ~260):

```rust
    async fn close_position(&self, coin: &str, size: f64) -> anyhow::Result<OrderResult> {
        if self.fail_close_coins.lock().unwrap().iter().any(|c| c.eq_ignore_ascii_case(coin)) {
            return Err(anyhow::anyhow!("simulated close failure for {coin}"));
        }
        self.closes.lock().unwrap().push((coin.to_string(), size));
        Ok(OrderResult { order_id: Some(99), filled: true, avg_price: Some(100.0) })
    }

    async fn cancel_orders_for_coin(&self, coin: &str) -> anyhow::Result<usize> {
        let matching: Vec<String> = self
            .open_orders
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.eq_ignore_ascii_case(coin))
            .cloned()
            .collect();
        for _ in &matching {
            self.cancels.lock().unwrap().push((coin.to_string(), 0));
        }
        Ok(matching.len())
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib mock_records_close_position mock_close_position_can_be_forced_to_fail mock_cancel_orders_for_coin_counts_matching`
Expected: PASS (3 tests).

- [ ] **Step 7: Commit**

```bash
git add src/hyperliquid/mod.rs
git commit -m "feat(exchange): add close_position + cancel_orders_for_coin to trait + mock"
```

---

### Task 2: Real `HyperliquidExchange` implementation

**Files:**
- Modify: `src/hyperliquid/mod.rs` — SDK import (`~line 418`), `impl Exchange for HyperliquidExchange` (after `position_size`, `~line 748`).

**Interfaces:**
- Consumes: `close_position`, `cancel_orders_for_coin` signatures from Task 1.
- Produces: live implementations (no unit test — network code, verified by `cargo build` only, consistent with the rest of this impl block).

- [ ] **Step 1: Add `MarketCloseParams` to the SDK import**

In the `use hyperliquid_rust_sdk::{...}` block (line ~418), add `MarketCloseParams` to the import list (alphabetical, after `ExchangeResponseStatus`):

```rust
use hyperliquid_rust_sdk::{
    BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ClientTrigger,
    ExchangeClient, ExchangeDataStatus, ExchangeResponseStatus, InfoClient, MarketCloseParams,
    MarketOrderParams, OpenOrdersResponse,
};
```

- [ ] **Step 2: Implement `close_position`**

In `impl Exchange for HyperliquidExchange`, after the `position_size` method (line ~748):

```rust
    /// Market-closes the full `size` for `coin` using the SDK's `market_close`,
    /// which fetches the mid-price, applies 1% slippage, and auto-selects the
    /// reduce-only side from the live position.
    async fn close_position(&self, coin: &str, size: f64) -> anyhow::Result<OrderResult> {
        let response = self
            .exchange
            .market_close(MarketCloseParams {
                asset: coin,
                sz: Some(size),
                px: None,
                slippage: Some(0.01),
                cloid: None,
                wallet: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("close_position failed: {e}"))?;
        parse_order_response(response)
    }
```

- [ ] **Step 3: Implement `cancel_orders_for_coin`**

Immediately after `close_position`:

```rust
    /// Cancels every resting order for `coin`. Queries the full open-order list
    /// (as `open_order_count` does), filters by coin, and cancels each by oid.
    /// A per-order cancel failure is logged and skipped so one bad order does
    /// not abort the rest.
    async fn cancel_orders_for_coin(&self, coin: &str) -> anyhow::Result<usize> {
        let orders: Vec<OpenOrdersResponse> = self.info.open_orders(self.address).await?;
        let mut cancelled = 0usize;
        for order in orders.iter().filter(|order| order.coin.eq_ignore_ascii_case(coin)) {
            match self
                .exchange
                .cancel(ClientCancelRequest { asset: coin.to_string(), oid: order.oid }, None)
                .await
            {
                Ok(_) => cancelled += 1,
                Err(error) => tracing::warn!(
                    "cancel_orders_for_coin: oid {} for {coin} failed: {error}", order.oid
                ),
            }
        }
        Ok(cancelled)
    }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: builds with no errors (warnings about unused are acceptable until wired in Task 4-5).

- [ ] **Step 5: Commit**

```bash
git add src/hyperliquid/mod.rs
git commit -m "feat(exchange): implement close_position + cancel_orders_for_coin for Hyperliquid"
```

---

### Task 3: Telegram — `CloseOutcome`, render functions, `execute_close`

**Files:**
- Modify: `src/telegram.rs` — callback constants (`~line 20`), new structs/functions (near `render_account`, `~line 142`), tests block (`~line 1490+`).

**Interfaces:**
- Consumes: `Exchange::close_position`, `Exchange::cancel_orders_for_coin` (Task 1); `OpenPosition` (fields `coin`, `direction`, `size`, `unrealized_pnl` — `src/hyperliquid/mod.rs:82`).
- Produces:
  - `pub const CB_CLOSE_ALL: &str = "close_all";`
  - `pub const CB_CLOSE_ONE_PREFIX: &str = "close_one:";`
  - `struct CloseOutcome { coin: String, ok: bool, error: Option<String> }`
  - `fn render_close_all_prompt(positions: &[OpenPosition]) -> String`
  - `fn render_close_one_prompt(position: &OpenPosition) -> String`
  - `fn render_close_result(outcomes: &[CloseOutcome]) -> String`
  - `fn close_confirm_keyboard(close_data: &str, button_label: &str) -> InlineKeyboardMarkup`
  - `async fn execute_close<E: Exchange>(exchange: &E, targets: &[OpenPosition]) -> Vec<CloseOutcome>`

- [ ] **Step 1: Add callback constants**

After `CB_CANCEL_FILL_PREFIX` (line ~20):

```rust
pub const CB_CLOSE_ALL: &str = "close_all";
pub const CB_CLOSE_ONE_PREFIX: &str = "close_one:";
```

- [ ] **Step 2: Write the failing tests**

In the `#[cfg(test)] mod tests` block at the bottom of `src/telegram.rs` (after an existing test, ~line 1490). Add a helper if `sample_positions` is not already accessible in scope — reuse the existing `sample_positions()` test helper (defined near `account_card_lists_positions_and_cap`, line ~1512).

```rust
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
```

> Note: if `sample_positions()` returns coins other than BTC/ETH, adjust the asserted coin names to match that helper. Verify by reading it before writing the test.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib close_all_prompt close_one_prompt close_result execute_close`
Expected: FAIL — unknown `render_close_all_prompt` / `CloseOutcome` / `execute_close`.

- [ ] **Step 4: Implement the struct, render functions, keyboard, and helper**

Near the other render functions (after `render_account`, ~line 230). The `format_pnl` style (`+$12.30`) mirrors `monitor::format_pnl`; inline it here to avoid a cross-module dependency:

```rust
/// Outcome of attempting to close one position.
pub struct CloseOutcome {
    pub coin: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Formats a signed USD value as `+$12.30` / `-$8.10`.
fn fmt_signed_usd(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "+" };
    format!("{sign}${:.2}", value.abs())
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
            fmt_signed_usd(position.unrealized_pnl),
        ));
    }
    out.push_str(&format!("\nTotal uPnL: {}", fmt_signed_usd(total)));
    out
}

/// Confirmation prompt for closing a single position.
pub fn render_close_one_prompt(position: &OpenPosition) -> String {
    format!(
        "⚠️ Tutup posisi {} {} {} — uPnL {}?",
        position.coin,
        position.direction,
        position.size,
        fmt_signed_usd(position.unrealized_pnl),
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
/// callback fired by the confirm button (CB_CLOSE_ALL or CB_CLOSE_ONE_PREFIX+coin).
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
```

> If `OpenPosition` is not already imported at the top of `src/telegram.rs`, add it to the `use crate::hyperliquid::{...}` line. (`render_pnl_summary`/`render_account` already use it, so it is almost certainly in scope.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib close_all_prompt close_one_prompt close_result execute_close`
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): close prompts, result render, and execute_close helper"
```

---

### Task 4: `/closeall` and `/close <COIN>` command handlers

**Files:**
- Modify: `src/telegram.rs` — `on_message` (after the `/account` handler, ~line 686).

**Interfaces:**
- Consumes: `render_close_all_prompt`, `render_close_one_prompt`, `close_confirm_keyboard`, `CB_CLOSE_ALL`, `CB_CLOSE_ONE_PREFIX` (Task 3); `context.exchange.positions()`; `first_command_word` (`src/telegram.rs:534`).
- Produces: two reachable commands that send a confirmation message with a keyboard.

- [ ] **Step 1: Add the `/closeall` handler**

In `on_message`, after the `/account` block ends (line ~686), before the `/settings` block:

```rust
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
```

- [ ] **Step 2: Add the `/close <COIN>` handler**

Immediately after the `/closeall` block:

```rust
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
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: builds cleanly (the new callbacks are handled in Task 5; until then tapping the button does nothing, which is fine for this checkpoint).

- [ ] **Step 4: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): /closeall and /close <COIN> commands with confirm keyboard"
```

---

### Task 5: Callback handlers for the confirm buttons

**Files:**
- Modify: `src/telegram.rs` — `on_callback` (after the `CB_CANCEL_FILL_PREFIX` block, ~line 1060).

**Interfaces:**
- Consumes: `CB_CLOSE_ALL`, `CB_CLOSE_ONE_PREFIX`, `execute_close`, `render_close_result` (Task 3); `context.exchange.positions()`.
- Produces: tapping `✅ Tutup semua` / `✅ Tutup <COIN>` closes positions and edits the message with the result. (`❌ Batal` already handled by the `CB_CANCEL` branch at `src/telegram.rs:1033`.)

- [ ] **Step 1: Add the `CB_CLOSE_ALL` handler**

In `on_callback`, after the `CB_CANCEL_FILL_PREFIX` block (line ~1060), before the trade-confirmation logic:

```rust
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
        let outcomes = execute_close(context.exchange.as_ref(), &positions).await;
        bot.edit_message_text(message.chat.id, message.id, render_close_result(&outcomes)).await?;
        return Ok(());
    }
```

- [ ] **Step 2: Add the `CB_CLOSE_ONE_PREFIX` handler**

Immediately after:

```rust
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
                let outcomes = execute_close(context.exchange.as_ref(), &[position]).await;
                bot.edit_message_text(message.chat.id, message.id, render_close_result(&outcomes)).await?;
            }
            None => {
                bot.edit_message_text(message.chat.id, message.id, format!("Posisi {coin} sudah tidak ada.")).await?;
            }
        }
        return Ok(());
    }
```

> `context.exchange` is `Arc<E>`; `execute_close` takes `&E`, so pass `context.exchange.as_ref()`. Confirm the field name/type by checking `struct BotContext` in `src/telegram.rs` (search `exchange:`).

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: builds cleanly.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): wire close-all / close-one confirm callbacks"
```

---

### Task 6: Document the commands in help text

**Files:**
- Modify: `src/telegram.rs` — `WELCOME_TEXT` (line 11) and the `/help` command output (search `command_response` / the help string).

**Interfaces:**
- Consumes: nothing.
- Produces: user-visible documentation of `/closeall` and `/close <COIN>`.

- [ ] **Step 1: Update the command list**

In `WELCOME_TEXT` (line 11), extend the trailing `Commands: ...` list to include the two new commands:

```
Commands: /start, /help, /stats, /account, /closeall, /close <COIN>, /settings, /set <key> <value>
```

Find any other place listing commands (e.g. a `/help` branch in `command_response`) and add the same two entries with a one-line description each, e.g.:

```
/closeall — tutup semua posisi
/close <COIN> — tutup satu posisi (contoh: /close BTC)
```

- [ ] **Step 2: Verify it compiles and tests pass**

Run: `cargo test`
Expected: all tests pass (string-only change).

- [ ] **Step 3: Commit**

```bash
git add src/telegram.rs
git commit -m "docs(telegram): document /closeall and /close in help text"
```

---

## Self-Review Notes

- **Spec coverage:** trait methods (Task 1-2), `/closeall` + `/close` commands (Task 4), confirmation buttons (Task 4-5), orphan SL/TP cancellation (Task 1-3 `cancel_orders_for_coin` + `execute_close`), best-effort per-position execution + result summary (Task 3, 5), help docs (Task 6). All spec sections mapped.
- **Out-of-scope items** (auto-close, partial close, spot) are not implemented, per spec.
- **Re-fetch at confirm** (spec "Callback wiring") implemented in Task 5 by calling `positions()` again inside both handlers.
- **Type consistency:** `close_position(coin, size)` and `cancel_orders_for_coin(coin) -> usize` used identically across Tasks 1-5; `CloseOutcome { coin, ok, error }` consistent; constants `CB_CLOSE_ALL` / `CB_CLOSE_ONE_PREFIX` consistent.
