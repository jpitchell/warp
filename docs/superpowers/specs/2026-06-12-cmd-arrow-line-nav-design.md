# Configurable `cmd+←` / `cmd+→` line navigation in running programs

**Date:** 2026-06-12
**Status:** Approved design, pending implementation plan
**Platform:** macOS (the affected bindings are Mac-only)

## Problem

In Warp, `cmd+←` / `cmd+→` jump to line start/end in Warp's own command editor (at the
shell prompt). When a program is running, they also work in a normal shell. But inside
Claude Code (and other CLI agents) they do nothing useful — unlike Ghostty and VS Code's
integrated terminal, where the same keys work.

### Root cause

This is **not** a missing binding. `app/src/terminal/view/init.rs:514–529` deliberately
binds, for long-running commands on Mac:

- `cmd+←` → `TerminalAction::ControlSequence([C0::SOH])` — i.e. **Ctrl-A** (`0x01`)
- `cmd+→` → `TerminalAction::ControlSequence([C0::ENQ])` — i.e. **Ctrl-E** (`0x05`)

Readline-based shells (bash/zsh) treat Ctrl-A/Ctrl-E as line start/end, so it works there.
Claude Code's input editor does **not** map Ctrl-A/Ctrl-E to line start/end — it expects the
real **Home/End escape sequences** (`ESC[H` / `ESC[F`, cursor-mode-aware). That is exactly
what Ghostty and VS Code send, and what Warp's *physical* Home/End keys already send via
`TerminalAction::Home`/`End` → `move_home`/`move_end` (`view.rs:22565–22581`).

So Warp has both behaviors available; the `cmd`-arrow bindings simply point at the
control-char one.

## Goal

Let users choose what `cmd+←` / `cmd+→` send while a program is running, defaulting to a
smart behavior that fixes CLI agents without changing shell behavior.

## Design

### 1. Setting

New enum in `app/src/terminal/settings.rs`, modeled exactly on the existing
`Osc52ClipboardAccess` (same derives: `SettingsValue`, `schemars::JsonSchema`, `Default`,
`as_dropdown_label`). Added as a field on the `TerminalSettings` group
(`define_settings_group!`, `settings.rs:132`), toml path `terminal.cmd_arrow_line_nav`,
default `Auto`.

```rust
pub enum CmdArrowLineNav {
    #[default]
    Auto,        // Home/End when a CLI agent owns the session; Ctrl-A/Ctrl-E otherwise
    LineEditing, // always Ctrl-A / Ctrl-E (SOH / ENQ) — today's behavior
    HomeEnd,     // always Home/End escape sequences (ESC[H / ESC[F)
}
```

| Variant | Behavior when a program is running |
|---|---|
| `Auto` *(default)* | `HomeEnd` when a CLI agent is active, else `LineEditing` |
| `LineEditing` | Always `SOH` / `ENQ` (current behavior) |
| `HomeEnd` | Always `ESC[H` / `ESC[F` (cursor-mode-aware) |

`Auto` only changes behavior **inside CLI-agent sessions** (where it is currently broken),
so it is a strict improvement with no shell regression.

### 2. Decision logic (isolated, unit-testable)

A pure function with no view/UI dependencies:

```rust
fn resolve_cmd_arrow_bytes(
    setting: CmdArrowLineNav,
    is_cli_agent: bool,
    edge: LineEdge,            // Start | End
    app_cursor_mode: bool,     // DECCKM, for ESC[H vs ESC OH
) -> CmdArrowOutput
```

`Auto` collapses to `HomeEnd` when `is_cli_agent`, otherwise `LineEditing`. The function
returns enough for the caller to either reuse `move_home`/`move_end` (the escape-sequence
path) or write the control char. The collapse + edge → bytes mapping is fully unit-tested
without constructing a `TerminalView`.

### 3. Wiring

`app/src/terminal/view/init.rs:514–529`: repoint the two existing `EditableBinding`s from
the baked-in `ControlSequence([SOH])` / `[ENQ])` to two new actions
`TerminalAction::CmdArrowLineStart` / `CmdArrowLineEnd`. Preserve:

- the `cmd-left` / `cmd-right` Mac key bindings,
- the `id!("Terminal") & !id!("IMEOpen") & id!("LongRunningCommand")` context,
- editability (users can still rebind these).

The action handlers in `view.rs` (dispatch match near `view.rs:26573`) read the setting via
`*TerminalSettings::as_ref(ctx).cmd_arrow_line_nav`, query CLI-agent state via the existing
`has_active_cli_agent_session(ctx)` (`view.rs:16631`), call `resolve_cmd_arrow_bytes`, then
either delegate to `move_home`/`move_end` or `write_user_bytes_to_pty(vec![SOH|ENQ])`.

Because the bindings already only fire in the `LongRunningCommand` context, the handler
knows a program is running; no extra `is_long_running()` guard is required for dispatch
(the existing guard inside `move_home`/`move_end` remains).

### 4. Settings UI

`app/src/settings_view/features_page.rs`: a dropdown widget modeled on the OSC-52 widget
(`SettingsWidget` impl + dropdown with `set_selected_by_name`). Label: *"cmd+← / cmd+→ in
running programs"*. Options: **Auto**, **Line-editing (Ctrl-A / Ctrl-E)**, **Home & End**.
Emits the existing `TerminalSettingsChangedEvent` pattern so the change applies live.

## Testing & validation

- **Unit:** `resolve_cmd_arrow_bytes` across all `setting × is_cli_agent × edge ×
  app_cursor_mode` combinations.
- **Integration:** an escape-sequence assertion test in the style of
  `crates/integration/tests/.../keyboard_protocol.rs` — with each setting (and agent
  active/inactive), `cmd-left`/`cmd-right` emit the expected bytes.
- **Manual:** build the OSS DMG (`./script/macos/bundle --selfsign --nouniversal --channel
  oss`), confirm `cmd+←/→` jump line start/end in Claude Code under `Auto`, and still work
  at the shell prompt; verify the three setting values behave as specified.

### Pre-flight de-risk — CONFIRMED ✅

Confirmed (2026-06-12): the **physical Home/End keys** and **fn+←/→** already jump to line
start/end *inside Claude Code today*. Those keys send `ESC[H`/`ESC[F` via the existing
`Home`/`End` actions, so this confirms the escape sequence is exactly what Claude's input
expects. The `HomeEnd` path therefore just needs to reuse `move_home`/`move_end`; no
`ESC[1~`/`ESC[4~` fallback is required.

## Scope / non-goals

- Only `cmd+←` / `cmd+→` (Mac). Word movement (`alt+←/→`) and physical Home/End are
  unchanged.
- No change to Warp's own command-editor behavior at the shell prompt.
- Linux/Windows unaffected (these bindings are Mac-only).

## Key files

| File | Change |
|---|---|
| `app/src/terminal/settings.rs` | new `CmdArrowLineNav` enum + `TerminalSettings` field |
| `app/src/terminal/view/init.rs` (514–529) | repoint `cmd-left`/`cmd-right` bindings to new actions |
| `app/src/terminal/view.rs` | new `TerminalAction` variants + handlers; resolver call |
| `app/src/settings_view/features_page.rs` | dropdown widget for the setting |
| resolver unit tests + integration escape-sequence test | new |
