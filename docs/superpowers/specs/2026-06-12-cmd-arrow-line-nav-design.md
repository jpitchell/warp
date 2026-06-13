# Configurable `cmd+←` / `cmd+→` line navigation in running programs

**Date:** 2026-06-12
**Status:** Implemented (PR #7). Includes an alt-screen root-cause correction found during dogfooding — see "Root cause (corrected during implementation)".
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

### Root cause (corrected during implementation)

The Ctrl-A/Ctrl-E-vs-Home/End story above is real, but it turned out to be only part of
the picture — and not the dominant reason `cmd+←/→` did nothing in Claude Code. The
`cmd`-arrow bindings were gated on the context predicate
`id!("Terminal") & !id!("IMEOpen") & id!("LongRunningCommand")`. But in
`app/src/terminal/view.rs` (`keymap_context`, ~line 28183) Warp sets `LongRunningCommand`
**only when not in the alternate screen** — `LongRunningCommand` and `AltScreen` are
mutually exclusive:

```rust
if model_lock.is_alt_screen_active() { context.set.insert("AltScreen"); }
if active_block.is_active_and_long_running() {
    if !model_lock.is_alt_screen_active() { context.set.insert("LongRunningCommand"); }
}
```

**Claude Code runs in the alternate screen.** So it gets `AltScreen`, never
`LongRunningCommand`, and the `cmd`-arrow binding never matched — the keystroke fell through
to the plain `left`/`right` arrow binding (gated only on `Terminal`), moving one character.
That is why, before the fix, all three setting values behaved identically (the handler never
ran) and why the *physical* Home/End keys worked (their bindings are not
`LongRunningCommand`-gated). The fix gates the bindings on
`(id!("LongRunningCommand") | id!("AltScreen"))`, mirroring the existing `shift-enter`
binding. (`is_long_running()` itself does not check alt-screen, so `move_home`/`move_end`
still emit to the PTY correctly in alt-screen.)

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
    Auto,        // Home/End for full-screen (alt-screen) apps OR CLI agents; Ctrl-A/Ctrl-E otherwise
    LineEditing, // always Ctrl-A / Ctrl-E (SOH / ENQ) — historical behavior
    HomeEnd,     // always Home/End escape sequences (ESC[H / ESC[F)
}
```

| Variant | Behavior when a program is running |
|---|---|
| `Auto` *(default)* | `HomeEnd` when the app is in the alternate screen **or** a CLI agent is active; else `LineEditing` |
| `LineEditing` | Always `SOH` / `ENQ` (historical behavior) |
| `HomeEnd` | Always `ESC[H` / `ESC[F` (cursor-mode-aware) |

`Auto` reflects the principled distinction between **visual/full-screen apps** (vim, less,
Claude Code — which want Home/End escape sequences) and **line-based readline shells** (bash,
zsh, python REPL — which want Ctrl-A/Ctrl-E). It changes behavior only in full-screen apps
and CLI-agent sessions — where `cmd+arrow` was previously a no-op — so it is a strict
improvement with no regression for line-based shells.

### 2. Decision logic (isolated, unit-testable)

A pure method on the enum, with no view/UI dependencies:

```rust
impl CmdArrowLineNav {
    fn resolve(self, prefer_home_end: bool, edge: LineEdge) -> CmdArrowResolution
}

enum LineEdge { Start, End }
enum CmdArrowResolution { ControlByte(u8), HomeEnd }
```

`prefer_home_end` is the caller's signal that this is a Home/End-preferring context
(alternate screen or a CLI agent). `Auto` resolves to `HomeEnd` when `prefer_home_end` is
true, else a control byte (`0x01` for Start, `0x05` for End); `LineEditing` is always the
control byte; `HomeEnd` is always `HomeEnd`. The caller then either reuses
`move_home`/`move_end` (the cursor-mode-aware escape-sequence path — so DECCKM `ESC OH`/`OF`
vs `ESC[H`/`[F` is handled there, not in the resolver) or writes the control byte. Fully
unit-tested without constructing a `TerminalView`.

### 3. Wiring

`app/src/terminal/view/init.rs` (the two `executing_command_move_cursor_*` bindings): repoint
the existing `EditableBinding`s from the baked-in `ControlSequence([SOH])` / `[ENQ])` to two
new actions `TerminalAction::CmdArrowLineStart` / `CmdArrowLineEnd`. Preserve:

- the `cmd-left` / `cmd-right` Mac key bindings,
- editability (users can still rebind these),

and **broaden the context** from `… & id!("LongRunningCommand")` to
`… & (id!("LongRunningCommand") | id!("AltScreen"))` so the bindings also fire in full-screen
apps (see the alt-screen root cause above).

The action handler `cmd_arrow_line_nav` in `view.rs` reads the setting via
`*TerminalSettings::as_ref(ctx).cmd_arrow_line_nav`, computes
`prefer_home_end = has_active_cli_agent_session(ctx) || model.is_alt_screen_active()`, calls
`resolve`, then delegates to `move_home`/`move_end` (the `HomeEnd` path) or
`control_sequence_on_terminal(&[byte], ctx)` (the control-byte path). Routing the control
byte through `control_sequence_on_terminal` — the same helper the original binding used —
keeps the line-editing path byte-for-byte identical to before.

The bindings only fire while a program owns the screen (`LongRunningCommand | AltScreen`), so
the handler knows a program is running; the existing `is_long_running()` guard inside
`move_home`/`move_end` and `control_sequence_on_terminal` remains.

### 4. Settings UI

`app/src/settings_view/features_page.rs`: a dropdown widget modeled on the OSC-52 widget
(`SettingsWidget` impl + dropdown with `set_selected_by_name`). Label: *"cmd+← / cmd+→ in
running programs"*. Options: **Auto**, **Line-editing (Ctrl-A / Ctrl-E)**, **Home & End**.
Emits the existing `TerminalSettingsChangedEvent` pattern so the change applies live.

## Testing & validation

- **Unit:** `CmdArrowLineNav::resolve` across all `setting × prefer_home_end × edge`
  combinations, plus the default == `Auto`.
- **Integration** (`crates/integration`, Mac-gated):
  - `LineEditing` → `cmd-left`/`cmd-right` emit `0x01` / `0x05`.
  - `HomeEnd` → emit `ESC[H` / `ESC[F`.
  - `Auto` + alternate screen → emit `ESC[H` / `ESC[F` (regression test for the alt-screen
    root cause; reproduces the original "one character" bug, which captured `ESC[D` before
    the fix). Uses a `read_keys_alt_screen.py` asset that enters the alternate screen, and
    asserts against the alt-screen grid.
- **Manual (done):** built the OSS DMG and confirmed `cmd+←/→` jump line start/end in Claude
  Code under `Auto`, still work at the shell prompt, and that the Features dropdown switches
  behavior.

### Pre-flight de-risk — CONFIRMED ✅

Confirmed (2026-06-12): the **physical Home/End keys** and **fn+←/→** already jump to line
start/end *inside Claude Code today*. Those keys send `ESC[H`/`ESC[F` via the existing
`Home`/`End` actions, so this confirms the escape sequence is exactly what Claude's input
expects. The `HomeEnd` path therefore just needs to reuse `move_home`/`move_end`; no
`ESC[1~`/`ESC[4~` fallback is required.

## Scope / non-goals

- Only `cmd+←` / `cmd+→` (Mac). Word movement (`alt+←/→`) and physical Home/End are
  unchanged — `alt+←/→` was verified to already work correctly in Claude Code, so it is left
  alone.
- No change to Warp's own command-editor behavior at the shell prompt.
- Linux/Windows unaffected (these bindings are Mac-only).

## Key files

| File | Change |
|---|---|
| `app/src/terminal/settings.rs` | new `CmdArrowLineNav` enum + `resolve()` + `TerminalSettings` field |
| `app/src/terminal/view/init.rs` | repoint `cmd-left`/`cmd-right` bindings to new actions; broaden context to `(LongRunningCommand \| AltScreen)` |
| `app/src/terminal/view.rs` | new `TerminalAction` variants + `cmd_arrow_line_nav` handler (`prefer_home_end = agent \|\| alt-screen`) |
| `app/src/settings_view/features_page.rs` | dropdown widget for the setting |
| `app/src/terminal/settings_tests.rs` + `crates/integration/*` | resolver unit tests + integration escape-sequence tests (incl. alt-screen) |
