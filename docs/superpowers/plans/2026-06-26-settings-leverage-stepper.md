# Settings Leverage Stepper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user adjust per-profile leverage from the `/settings` inline keyboard via −/+ stepper buttons, without typing `/set`.

**Architecture:** Extend `settings_keyboard` to render three stepper rows (Conservative/Moderate/Aggressive, each `[−] Nx [+]`). A pure `adjust_leverage` function applies a clamped ±1 step. A new prefix-based callback (`lev:<profile>:<dir>`) updates the in-memory `Settings`, persists via `SettingsStore`, and edits the message in place.

**Tech Stack:** Rust, `teloxide`, `tokio`, `anyhow`.

## Global Constraints

- Leverage values clamp to `[1, 50]` (1 = SDK floor; 50 = sane ceiling — the exchange enforces the real per-asset cap at order time, cf. `src/hyperliquid/mod.rs:619`).
- Step size is ±1x per tap (per design decision: "Stepper −/+ per profile").
- Settings changes persist through `context.settings_store.persist(&next)` (`src/settings.rs:174`), matching the existing entry-mode callback (`src/telegram.rs:985-998`).
- `settings_keyboard` is called in two places — both must pass the new argument: `on_message` `/settings` handler (`src/telegram.rs:692`) and the entry-mode callback (`src/telegram.rs:995`).
- Run `cargo test` after each task.

---

### Task 1: `adjust_leverage` + extend `settings_keyboard`

**Files:**
- Modify: `src/telegram.rs` — callback constants (`~line 23`), `settings_keyboard` (`line 234`), tests block (`~line 1490+`).
- Reference: `LeverageMap` (`src/config.rs:12`, fields `conservative`/`moderate`/`aggressive: u32`); `RiskProfile` (already imported in `src/telegram.rs`); `label` helper (`src/telegram.rs:102`).

**Interfaces:**
- Produces:
  - `pub const CB_LEV_PREFIX: &str = "lev:";`
  - `fn adjust_leverage(map: &LeverageMap, profile: RiskProfile, delta: i32) -> LeverageMap` — returns a new map with `profile`'s leverage stepped by `delta`, clamped to `[1, 50]`.
  - `settings_keyboard(active: EntryMode, leverage: &LeverageMap) -> InlineKeyboardMarkup` — now also renders three stepper rows.

- [ ] **Step 1: Add callback constants**

After `CB_MODE_FIXED` (line ~23):

```rust
pub const CB_LEV_PREFIX: &str = "lev:";
```

- [ ] **Step 2: Write the failing tests**

In the `#[cfg(test)] mod tests` block (~line 1490):

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib adjust_leverage_steps_and_clamps settings_keyboard_includes_leverage_callbacks`
Expected: FAIL — unknown `adjust_leverage`; `settings_keyboard` arity mismatch.

- [ ] **Step 4: Implement `adjust_leverage` and update `settings_keyboard`**

Add `adjust_leverage` just above `settings_keyboard` (line ~233):

```rust
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
```

> `LeverageMap` derives `Copy` (`src/config.rs:11` — verify; it is a plain `u32`-field struct). If it is not `Copy`, replace `let mut next = *map;` with `let mut next = map.clone();`.

Replace `settings_keyboard` (lines 234-243) with:

```rust
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
```

> The middle button uses a `:noop` callback so a tap on the value label is a harmless no-op (handled in Task 2).

- [ ] **Step 5: Fix the two `settings_keyboard` call sites**

The signature changed, so both callers must pass `&leverage`.

In the `/settings` handler (`src/telegram.rs:692`):

```rust
        bot.send_message(message.chat.id, render_settings(&settings))
            .reply_markup(settings_keyboard(settings.entry_mode, &settings.leverage))
            .await?;
```

In the entry-mode callback (`src/telegram.rs:994-996`):

```rust
        bot.edit_message_text(message.chat.id, message.id, render_settings(&next))
            .reply_markup(settings_keyboard(mode, &next.leverage))
            .await?;
```

- [ ] **Step 6: Run tests + build**

Run: `cargo test --lib adjust_leverage_steps_and_clamps settings_keyboard_includes_leverage_callbacks && cargo build`
Expected: 2 tests PASS; build clean.

- [ ] **Step 7: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): leverage stepper buttons in /settings keyboard"
```

---

### Task 2: Leverage stepper callback handler

**Files:**
- Modify: `src/telegram.rs` — `on_callback`, inside the existing entry-mode-callback area (after the `entry_mode_from_callback` block, ~line 999).

**Interfaces:**
- Consumes: `CB_LEV_PREFIX`, `adjust_leverage`, `settings_keyboard` (Task 1); `render_settings`; `context.settings` (`Arc<Mutex<Settings>>`); `context.settings_store.persist` (`src/settings.rs:174`).
- Produces: tapping `➖`/`➕` updates and persists the leverage and re-renders the settings message; `:noop` answers silently.

- [ ] **Step 1: Add a parser for the leverage callback**

Near `entry_mode_from_callback` (line ~245), add:

```rust
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
```

- [ ] **Step 2: Write the failing test**

In the tests block:

```rust
    #[test]
    fn leverage_callback_parses_profile_and_delta() {
        use crate::sizing::RiskProfile;
        assert_eq!(super::leverage_step_from_callback("lev:moderate:inc"), Some((RiskProfile::Moderate, 1)));
        assert_eq!(super::leverage_step_from_callback("lev:conservative:dec"), Some((RiskProfile::Conservative, -1)));
        assert_eq!(super::leverage_step_from_callback("lev:aggressive:noop"), Some((RiskProfile::Aggressive, 0)));
        assert_eq!(super::leverage_step_from_callback("lev:bogus:inc"), None);
        assert_eq!(super::leverage_step_from_callback("entry_mode:risk"), None);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib leverage_callback_parses_profile_and_delta`
Expected: FAIL — unknown `leverage_step_from_callback`.

- [ ] **Step 4: Add the callback handler in `on_callback`**

In `on_callback`, after the entry-mode-switch block (the `if let Some(mode) = entry_mode_from_callback(&data)` block ends ~line 999), add:

```rust
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
```

> Place this BEFORE the `profile_from_callback` block: `lev:conservative:*` must not be mistaken for a trade risk-profile switch. `profile_from_callback` matches `profile:conservative` (different prefix), so there is no real collision, but ordering keeps intent clear.

- [ ] **Step 5: Run tests + build**

Run: `cargo test && cargo build`
Expected: all tests pass; build clean.

- [ ] **Step 6: Commit**

```bash
git add src/telegram.rs
git commit -m "feat(telegram): handle leverage stepper callbacks with persistence"
```

---

## Self-Review Notes

- **Design coverage:** stepper rows rendered (Task 1), ±1 clamped step (Task 1 `adjust_leverage`), callback updates + persists + re-renders (Task 2), both `settings_keyboard` call sites updated (Task 1 Step 5).
- **Edge cases:** clamp `[1, 50]` tested; `:noop` label tap is harmless; bogus callback data returns `None`.
- **Type consistency:** `adjust_leverage(&LeverageMap, RiskProfile, i32) -> LeverageMap`, `settings_keyboard(EntryMode, &LeverageMap)`, `leverage_step_from_callback(&str) -> Option<(RiskProfile, i32)>`, `CB_LEV_PREFIX` used consistently across both tasks.
- **No leverage cap conflict:** values above an asset's real max are rejected by the exchange at order time (documented constraint), so the UI ceiling of 50 is safe.
