# Design: Cancel-Resting-Limit Button

**Date:** 2026-06-20
**Status:** Approved (pending spec review)

## Problem

When a user confirms a limit entry, the bot places a resting limit order and waits
for it to fill for up to `entry_fill_timeout_secs` (currently configured to 3000s /
50 minutes). During this wait the user has **no way to cancel from Telegram** — the
only escape is the automatic timeout or cancelling manually in the Hyperliquid app.

This caused a concrete loss of opportunity: a long limit for AERO sat unfilled at
$0.4200, and a better short setup for the same coin was rejected with
`Already have 1 open order(s) for AERO — skipped` (per-coin resting-order guard at
`src/telegram.rs:783`). The user was effectively trapped behind the unfilled order.

## Goal

Let the user cancel a resting (unfilled) limit entry directly from Telegram, via a
button attached to the "menunggu fill…" message, so the per-coin order guard is
released and a new setup can be re-sent immediately.

## Non-goals (YAGNI)

- No `/cancel <coin>` text command (button only).
- No cancelling of already-armed brackets or open positions (this targets the
  resting-entry wait only).
- No two-tap / confirm-the-cancel step (cancelling a resting order is low-risk).
- No change to the existing automatic fill timeout behaviour.

## Chosen mechanism (Approach A — signal registry, loop owns the cancel)

The fill-wait loop runs inside a spawned tokio task (`tokio::spawn` at
`src/telegram.rs:1132`) and is the only holder of the resting order's `order_id`.
The button press is handled in `on_callback`, a different task. They are coordinated
by a shared registry of cancellation signals; **the wait loop performs the actual
`cancel_order` call** so there is exactly one canceller and no double-cancel race
with the timeout path.

Rejected alternatives:
- **B — callback cancels the exchange directly:** the loop would keep polling until
  the full timeout (the original 3000s problem returns), and both the callback and
  the timeout could call `cancel_order` (double-cancel race).
- **C — force an early timeout:** hacky; cannot distinguish a user cancel from a
  real timeout in messaging, and does not cleanly carry the `order_id`.

No new crate dependency: `tokio` already enables the `sync` feature, so
`tokio::sync::Notify` is available.

## Components & changes

### New shared state on `BotContext`

```rust
pending_fills: Arc<Mutex<HashMap<String, Arc<Notify>>>>,  // key = coin (uppercased)
```

Maps a coin to the cancellation signal for its in-flight fill-wait. Because the
per-coin guard (`open_order_count`) forbids a second resting order for the same coin,
the coin is a unique key for at most one in-flight wait at a time.

### Callback constant

```rust
pub const CB_CANCEL_FILL_PREFIX: &str = "cancel_fill:";   // e.g. "cancel_fill:AERO"
```

Distinct from the existing `CB_CANCEL = "cancel"` (which discards the *confirmation
card* before execution).

### `confirmation_keyboard` is unchanged; a new tiny keyboard helper

```rust
fn fill_wait_keyboard(coin: &str) -> InlineKeyboardMarkup  // one button: ❌ Batalkan order
```

### `ExecutionEvent` — new variant

```rust
/// A resting limit entry was cancelled by the user before filling.
/// `cancelled` notes the best-effort cancel of the resting order succeeded.
EntryCancelled { cancelled: bool },
```

Message text in `format_execution_event`:
- `EntryCancelled { cancelled: true }` → `"❌ Order {coin} dibatalkan — tidak ada posisi."`
- `EntryCancelled { cancelled: false }` → `"⚠️ Pembatalan order {coin} TIDAK terkonfirmasi — cek manual, mungkin masih ada order tersisa."`

### `EntrySubmitted` reporting carries the button

The "menunggu fill…" message (sent on `EntrySubmitted { limit: true, .. }`) must be
sent **with** `fill_wait_keyboard(coin)`. The market path
(`EntrySubmitted { limit: false, .. }`) stays button-less.

This requires the reporter to attach a keyboard only for that one event. Implement by
giving `TelegramReporter` an optional keyboard for the limit-submitted event (it
already knows `coin`), keeping the `ProgressReporter` trait otherwise unchanged.

### `execute_plan` — accept a cancel signal and select on it

Signature gains a parameter:

```rust
pub async fn execute_plan<E: Exchange>(
    exchange: &E,
    plan: &ExecutionPlan,
    use_limit: bool,
    fill_timeout_secs: u64,
    cancel: Arc<Notify>,          // NEW
    reporter: &dyn ProgressReporter,
) -> anyhow::Result<()>
```

In the fill-wait loop (`src/telegram.rs:399-427`), the per-iteration sleep becomes a
select between the 1s tick and the cancel signal:

```rust
let user_cancelled = tokio::select! {
    _ = tokio::time::sleep(Duration::from_secs(1)) => false,
    _ = cancel.notified()                          => true,
};
```

When `user_cancelled` (or the existing `elapsed >= fill_timeout_secs`), the loop runs
a single shared "wind-down" branch:

1. `cancel_order(order_id)` (best-effort) → `cancelled: bool`.
2. Re-read `position_size(coin)` → `held`.
3. Branch on `held`:
   - `held >= plan.size * 0.99` → treat as full fill: `Filled { full }`, then arm the
     full bracket (handles the race where the order fills at the instant of the tap).
   - `held > 0` → partial: `Filled { partial: true }`, arm bracket on `held` (existing
     safe path — the position is never left without a stop-loss).
   - `held <= 0` → no position. Report `EntryCancelled { cancelled }` if the wake was
     a user cancel, or the existing `FillTimeout { cancelled }` if it was the timeout.
     Bail without arming a bracket.

The only behavioural difference between a user cancel and a timeout at zero fill is the
reported event (`EntryCancelled` vs `FillTimeout`); the wind-down logic is shared.

### `on_callback` — handle the cancel button

Placed early (just after the entry-mode block / near the `CB_CANCEL` handler):

```rust
if let Some(coin) = data.strip_prefix(CB_CANCEL_FILL_PREFIX) {
    let signal = context.pending_fills.lock().unwrap().get(coin).cloned();
    match signal {
        Some(notify) => {
            notify.notify_one();
            bot.answer_callback_query(&query.id).text("Membatalkan…").await?;
            // The button is ON this very message → edit it to drop the keyboard.
            bot.edit_message_text(message.chat.id, message.id,
                format!("⏳ Membatalkan order {coin}…")).await?;
        }
        None => {
            bot.answer_callback_query(&query.id)
                .text(format!("Order {coin} sudah tidak aktif.")).await?;
            // Best-effort: remove the now-stale keyboard.
            let _ = bot.edit_message_reply_markup(message.chat.id, message.id).await;
        }
    }
    return Ok(());
}
```

We never need to store the "menunggu fill" message id: the button lives on that
message, so `query.regular_message()` already returns it.

### Spawn wrapper — register and clean up

At the spawn site (`src/telegram.rs:1129-1157`):

```rust
let cancel = Arc::new(Notify::new());
let coin_key = trade.plan.coin.to_uppercase();
context.pending_fills.lock().unwrap().insert(coin_key.clone(), cancel.clone());

tokio::spawn(async move {
    let outcome = execute_plan(.., cancel, &reporter).await;
    task_context.pending_fills.lock().unwrap().remove(&coin_key);  // always clean up
    // ... existing error reporting ...
});
```

Registration happens for both limit and market confirms; the market path simply never
shows a button and the entry is removed almost immediately. Cleanup runs on **every**
exit path (full fill, partial, user cancel, timeout, error), so no registry leak.

## Data flow summary

| State at cancel | Result |
|---|---|
| 0 fill | resting order cancelled, no position, registry cleared, guard released |
| Partial fill | resting remainder cancelled, bracket armed on filled portion |
| Fills fully at the instant of tap | treated as full fill, full bracket armed (no spurious "cancelled") |
| Stale tap (loop already ended) | toast "Order {coin} sudah tidak aktif.", keyboard removed |
| `cancel_order` fails | `EntryCancelled { cancelled: false }` warning, user told to check manually |

## Error handling

- `cancel_order` failure → `EntryCancelled { cancelled: false }` (mirrors the existing
  `FillTimeout { cancelled: false }` warning).
- `notify_one()` with no waiter (loop already exited) is a safe no-op for `Notify`.
- Registry `Mutex` is locked with `.unwrap()`, consistent with the existing `Mutex`
  usage in this file.
- A reporter send failure remains swallowed (logged) so execution never aborts.

## Testing

- **Unit:** `format_execution_event` renders `EntryCancelled { true }` and
  `{ false }` correctly.
- **Unit:** `fill_wait_keyboard("AERO")` yields one button whose callback data is
  exactly `cancel_fill:AERO`.
- **Integration (existing mock `Exchange`):**
  - Cancel at 0 fill → `cancel_order` called once, no bracket placed, registry entry
    removed.
  - Cancel at partial fill → bracket armed on the held size.
  - Full fill at tap time → full bracket armed, `EntryCancelled` NOT emitted.
  - Stale tap → callback path does not panic and emits the "tidak aktif" toast.

## Affected files

- `src/telegram.rs` — the new `pending_fills` field on `BotContext`
  (defined at `src/telegram.rs:460`), keyboard helper, `ExecutionEvent` variant +
  formatting, `execute_plan` signature + select loop, `on_callback` handler, spawn
  wrapper, and the `TelegramReporter` keyboard for the limit-submitted event.
- Tests alongside the above.
