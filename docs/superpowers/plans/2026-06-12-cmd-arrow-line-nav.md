# Configurable cmd+arrow Line Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users choose what `cmd+←` / `cmd+→` send while a program is running, defaulting to `Auto` so the keys jump line start/end inside Claude Code (and other CLI agents) without changing shell behavior.

**Architecture:** Add a `CmdArrowLineNav` enum setting to `TerminalSettings` with a pure, unit-tested resolver. Two new `TerminalAction` variants (`CmdArrowLineStart`/`CmdArrowLineEnd`) replace the hard-coded `ControlSequence([SOH/ENQ])` payloads on the existing `cmd-left`/`cmd-right` bindings. At dispatch time the handler reads the setting + CLI-agent state and either reuses `move_home`/`move_end` (sends `ESC[H`/`ESC[F`) or writes the `Ctrl-A`/`Ctrl-E` control byte. A settings-page dropdown mirrors the existing OSC-52 widget.

**Tech Stack:** Rust, Warp's `define_settings_group!` macro, `settings_value::SettingsValue`, `schemars`, Warp keybinding registration (`FixedBinding`/`EditableBinding`), the `crates/integration` test framework.

**Reference:** `docs/superpowers/specs/2026-06-12-cmd-arrow-line-nav-design.md`

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `app/src/terminal/settings.rs` | Setting enum + resolver | Add `CmdArrowLineNav`, `LineEdge`, `CmdArrowResolution`, `resolve()`; add field to `TerminalSettings` group |
| `app/src/terminal/settings_tests.rs` | Resolver unit tests | Add tests for `resolve()` |
| `app/src/terminal/view/action.rs` | `TerminalAction` enum + Display impl | Add `CmdArrowLineStart`/`CmdArrowLineEnd` variants + Display arms |
| `app/src/terminal/view.rs` | Action dispatch + handler | Add dispatch arms, `cmd_arrow_line_nav` handler, accessibility-match arms |
| `app/src/terminal/view/init.rs` | Keybinding registration | Repoint `cmd-left`/`cmd-right` bindings to new actions |
| `app/src/settings_view/features_page.rs` | Settings UI | Dropdown widget mirroring OSC-52 |
| `crates/integration/tests/integration/...` | E2E escape-sequence test | New test asserting emitted bytes per mode |

---

## Task 1: Setting enum + pure resolver (TDD)

**Files:**
- Modify: `app/src/terminal/settings.rs` (add enum + impl near the other enums, before `define_settings_group!` at line 132)
- Test: `app/src/terminal/settings_tests.rs`

- [ ] **Step 1: Write the failing test**

Add to `app/src/terminal/settings_tests.rs`:

```rust
#[test]
fn cmd_arrow_line_nav_resolves_correctly() {
    use crate::terminal::settings::{CmdArrowLineNav, CmdArrowResolution, LineEdge};

    // LineEditing: always control bytes, regardless of agent.
    assert_eq!(
        CmdArrowLineNav::LineEditing.resolve(true, LineEdge::Start),
        CmdArrowResolution::ControlByte(0x01) // Ctrl-A / SOH
    );
    assert_eq!(
        CmdArrowLineNav::LineEditing.resolve(false, LineEdge::End),
        CmdArrowResolution::ControlByte(0x05) // Ctrl-E / ENQ
    );

    // HomeEnd: always Home/End escape path, regardless of agent.
    assert_eq!(
        CmdArrowLineNav::HomeEnd.resolve(false, LineEdge::Start),
        CmdArrowResolution::HomeEnd
    );
    assert_eq!(
        CmdArrowLineNav::HomeEnd.resolve(true, LineEdge::End),
        CmdArrowResolution::HomeEnd
    );

    // Auto: Home/End when a CLI agent owns the session, control bytes otherwise.
    assert_eq!(
        CmdArrowLineNav::Auto.resolve(true, LineEdge::Start),
        CmdArrowResolution::HomeEnd
    );
    assert_eq!(
        CmdArrowLineNav::Auto.resolve(false, LineEdge::Start),
        CmdArrowResolution::ControlByte(0x01)
    );
    assert_eq!(
        CmdArrowLineNav::Auto.resolve(false, LineEdge::End),
        CmdArrowResolution::ControlByte(0x05)
    );

    // Default is Auto.
    assert_eq!(CmdArrowLineNav::default(), CmdArrowLineNav::Auto);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p warp --lib terminal::settings_tests::cmd_arrow_line_nav_resolves_correctly`
Expected: FAIL — `CmdArrowLineNav`, `LineEdge`, `CmdArrowResolution` not found.

(If the package name `warp` is wrong, find it with `grep '^name' app/Cargo.toml`.)

- [ ] **Step 3: Add the enum, supporting types, and resolver**

In `app/src/terminal/settings.rs`, immediately before `define_settings_group!` (line 132), add:

```rust
/// Which line edge a `cmd`-arrow press targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEdge {
    Start,
    End,
}

/// What a resolved `cmd`-arrow press should emit to the PTY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdArrowResolution {
    /// Send a single control byte (Ctrl-A `0x01` / Ctrl-E `0x05`).
    ControlByte(u8),
    /// Send the cursor-mode-aware Home/End escape sequence
    /// (handled by `move_home` / `move_end`).
    HomeEnd,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "What cmd+left / cmd+right send while a program is running.",
    rename_all = "snake_case"
)]
pub enum CmdArrowLineNav {
    /// Home/End escape sequences when a CLI agent (e.g. Claude Code) owns the
    /// session; Ctrl-A / Ctrl-E otherwise.
    #[default]
    #[schemars(description = "Smart: Home/End for CLI agents, Ctrl-A/Ctrl-E for shells.")]
    Auto,
    /// Always send Ctrl-A / Ctrl-E (the historical behavior).
    #[schemars(description = "Always send Ctrl-A / Ctrl-E line-editing control characters.")]
    LineEditing,
    /// Always send Home / End escape sequences.
    #[schemars(description = "Always send Home / End escape sequences.")]
    HomeEnd,
}

impl CmdArrowLineNav {
    /// Resolve this setting (collapsing `Auto`) into the bytes to emit.
    pub fn resolve(self, is_cli_agent: bool, edge: LineEdge) -> CmdArrowResolution {
        let use_home_end = match self {
            CmdArrowLineNav::Auto => is_cli_agent,
            CmdArrowLineNav::LineEditing => false,
            CmdArrowLineNav::HomeEnd => true,
        };
        if use_home_end {
            CmdArrowResolution::HomeEnd
        } else {
            CmdArrowResolution::ControlByte(match edge {
                LineEdge::Start => 0x01, // SOH / Ctrl-A
                LineEdge::End => 0x05,   // ENQ / Ctrl-E
            })
        }
    }

    pub fn as_dropdown_label(self) -> &'static str {
        match self {
            CmdArrowLineNav::Auto => "Auto",
            CmdArrowLineNav::LineEditing => "Line-editing keys (Ctrl-A / Ctrl-E)",
            CmdArrowLineNav::HomeEnd => "Home & End keys",
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p warp --lib terminal::settings_tests::cmd_arrow_line_nav_resolves_correctly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/src/terminal/settings.rs app/src/terminal/settings_tests.rs
git commit -m "Add CmdArrowLineNav setting enum and resolver"
```

---

## Task 2: Register the setting in the TerminalSettings group

**Files:**
- Modify: `app/src/terminal/settings.rs:181-189` (inside `define_settings_group!`, after the `osc52_clipboard_access` entry)

- [ ] **Step 1: Add the settings field**

In `define_settings_group!(TerminalSettings, settings: [ ... ])`, add a new entry after the `osc52_clipboard_access` block (after line 189, before the `async_find_enabled` entry):

```rust
    cmd_arrow_line_nav: CmdArrowLineNavSetting {
        type: CmdArrowLineNav,
        default: CmdArrowLineNav::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        private: false,
        toml_path: "terminal.cmd_arrow_line_nav",
        description: "What cmd+left / cmd+right send while a program is running. Options: auto (default), line_editing, home_end.",
    },
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p warp`
Expected: builds. The macro generates `CmdArrowLineNavSetting` and the `cmd_arrow_line_nav` field, readable as `*TerminalSettings::as_ref(ctx).cmd_arrow_line_nav`.

- [ ] **Step 3: Commit**

```bash
git add app/src/terminal/settings.rs
git commit -m "Register terminal.cmd_arrow_line_nav setting"
```

---

## Task 3: Add TerminalAction variants + dispatch handler

**Files:**
- Modify: `app/src/terminal/view/action.rs:185-186` (enum variants) and `:560` (Display impl)
- Modify: `app/src/terminal/view.rs` — dispatch arms (`:26573-26574`), accessibility match (`:26206-26207`), and a new handler method (near `move_home`/`move_end` at `:22565-22581`)

- [ ] **Step 1: Add the enum variants**

In `app/src/terminal/view/action.rs`, after the `End,` variant (line 186), add:

```rust
    /// `cmd+left` in a running program: jump to line start (setting-dependent).
    CmdArrowLineStart,
    /// `cmd+right` in a running program: jump to line end (setting-dependent).
    CmdArrowLineEnd,
```

- [ ] **Step 2: Add Display arms**

In the same file's Display/Debug `match` impl, after the `End => f.write_str("End"),` arm (line 560), add:

```rust
            CmdArrowLineStart => f.write_str("CmdArrowLineStart"),
            CmdArrowLineEnd => f.write_str("CmdArrowLineEnd"),
```

- [ ] **Step 3: Add the handler method in view.rs**

In `app/src/terminal/view.rs`, immediately after `move_end` (after line 22581), add:

```rust
    fn cmd_arrow_line_nav(&mut self, edge: LineEdge, ctx: &mut ViewContext<Self>) {
        let setting = *TerminalSettings::as_ref(ctx).cmd_arrow_line_nav;
        let is_cli_agent = self.has_active_cli_agent_session(ctx);
        match setting.resolve(is_cli_agent, edge) {
            CmdArrowResolution::HomeEnd => match edge {
                LineEdge::Start => self.move_home(ctx),
                LineEdge::End => self.move_end(ctx),
            },
            CmdArrowResolution::ControlByte(byte) => {
                self.write_user_bytes_to_pty(vec![byte], ctx);
            }
        }
    }
```

Add the import near the top of `view.rs` (with the other `crate::terminal::settings` / `TerminalSettings` imports — search for `TerminalSettings` to find the existing `use`):

```rust
use crate::terminal::settings::{CmdArrowResolution, LineEdge};
```

(If `TerminalSettings` is imported via a glob or a grouped `use crate::terminal::settings::{...}`, add `CmdArrowResolution` and `LineEdge` to that existing group instead of adding a new line.)

- [ ] **Step 4: Add dispatch arms**

In `app/src/terminal/view.rs`, after `End => self.move_end(ctx),` (line 26574), add:

```rust
            CmdArrowLineStart => self.cmd_arrow_line_nav(LineEdge::Start, ctx),
            CmdArrowLineEnd => self.cmd_arrow_line_nav(LineEdge::End, ctx),
```

- [ ] **Step 5: Add accessibility-match arms**

In the exhaustive accessibility `match` (the large or-pattern arm containing `| Home | End` at line 26206-26207), add the two new variants to that same arm so they share the no-extra-announcement behavior:

```rust
            | Home
            | End
            | CmdArrowLineStart
            | CmdArrowLineEnd
```

- [ ] **Step 6: Build (rustc enforces remaining matches)**

Run: `cargo build -p warp`
Expected: builds. If rustc reports any *other* non-exhaustive `match` over `TerminalAction`, add `CmdArrowLineStart`/`CmdArrowLineEnd` to it following the `Home`/`End` precedent in that same match, then rebuild.

- [ ] **Step 7: Commit**

```bash
git add app/src/terminal/view/action.rs app/src/terminal/view.rs
git commit -m "Add CmdArrowLineStart/End actions and dispatch handler"
```

---

## Task 4: Repoint the cmd-left / cmd-right bindings

**Files:**
- Modify: `app/src/terminal/view/init.rs:514-529`

- [ ] **Step 1: Change the two binding actions**

In `app/src/terminal/view/init.rs`, the `terminal:executing_command_move_cursor_home` binding (lines 514-522) currently uses `TerminalAction::ControlSequence(vec![escape_sequences::C0::SOH])`. Replace that action with `TerminalAction::CmdArrowLineStart`. Likewise the `terminal:executing_command_move_cursor_end` binding (lines 523-529) currently uses `TerminalAction::ControlSequence(vec![escape_sequences::C0::ENQ])` — replace with `TerminalAction::CmdArrowLineEnd`.

The two `EditableBinding`s become:

```rust
        EditableBinding::new(
            "terminal:executing_command_move_cursor_home",
            "Move cursor home within an executing command",
            TerminalAction::CmdArrowLineStart,
        )
        .with_mac_key_binding("cmd-left")
        .with_context_predicate(id!("Terminal") & !id!("IMEOpen") & id!("LongRunningCommand")),
        EditableBinding::new(
            "terminal:executing_command_move_cursor_end",
            "Move cursor end within an executing command",
            TerminalAction::CmdArrowLineEnd,
        )
        .with_mac_key_binding("cmd-right")
        .with_context_predicate(id!("Terminal") & !id!("IMEOpen") & id!("LongRunningCommand")),
```

Keep the binding name strings, key bindings, and context predicate exactly as-is (binding names are stable identifiers; changing them would reset users' custom keybindings). Remove the now-stale comment on lines 519-520 ("We already have bindings for home/end ... that send the correct control sequence to the PTY") since the action no longer hard-codes a control sequence.

- [ ] **Step 2: Build**

Run: `cargo build -p warp`
Expected: builds. `escape_sequences::C0::SOH/ENQ` may now be unused in this file — if rustc warns about an unused import, remove it.

- [ ] **Step 3: Verify pty-compliance validator still accepts the bindings**

These bindings are registered for `TerminalView`, which runs `is_binding_pty_compliant` (`init.rs:98`). `cmd-left`/`cmd-right` are not control-character keystrokes, so they remain valid. Confirm no validator panic/log at startup in Step 4 of Task 7.

- [ ] **Step 4: Commit**

```bash
git add app/src/terminal/view/init.rs
git commit -m "Route cmd-left/right to CmdArrowLineStart/End actions"
```

---

## Task 5: Settings-page dropdown

**Files:**
- Modify: `app/src/settings_view/features_page.rs` (10 sites mirroring `Osc52ClipboardAccess`)

This task mirrors the existing OSC-52 dropdown exactly. Each edit point below references the OSC-52 line to copy from.

- [ ] **Step 1: Import the setting types**

At the `use crate::terminal::settings::{...}` group containing `Osc52ClipboardAccess, Osc52ClipboardAccessSetting` (line 99), add `CmdArrowLineNav, CmdArrowLineNavSetting`:

```rust
    AsyncFindEnabled, CmdArrowLineNav, CmdArrowLineNavSetting, MaximumGridSize,
    Osc52ClipboardAccess, Osc52ClipboardAccessSetting,
```

- [ ] **Step 2: Add the page-action variant**

In the `FeaturesPageAction` enum, after `SetOsc52ClipboardAccess(Osc52ClipboardAccess)` (line 799), add:

```rust
    SetCmdArrowLineNav(CmdArrowLineNav),
```

- [ ] **Step 3: Add telemetry mapping**

After the `Self::SetOsc52ClipboardAccess(access) => ...` telemetry arm (line 1216-1217), add:

```rust
            Self::SetCmdArrowLineNav(nav) => TelemetryEvent::FeaturesPageAction {
                action: "SetCmdArrowLineNav".to_string(),
            },
```

(Match the exact field shape of the `SetOsc52ClipboardAccess` arm you are copying — if it includes additional fields, copy them verbatim.)

- [ ] **Step 4: Add the action handler**

After the `SetOsc52ClipboardAccess(access) => { ... }` handler (lines 1944-1950), add:

```rust
            SetCmdArrowLineNav(nav) => {
                TerminalSettings::handle(ctx).update(ctx, |terminal_settings, ctx| {
                    report_if_error!(terminal_settings
                        .cmd_arrow_line_nav
                        .set_value(*nav, ctx));
                });
            }
```

- [ ] **Step 5: Add the dropdown field**

After `osc52_clipboard_access_dropdown: ViewHandle<Dropdown<FeaturesPageAction>>,` (line 1431), add:

```rust
    cmd_arrow_line_nav_dropdown: ViewHandle<Dropdown<FeaturesPageAction>>,
```

- [ ] **Step 6: Subscribe to setting changes**

In the `TerminalSettingsChangedEvent` match (around line 2294-2298), after the `Osc52ClipboardAccessSetting { .. } => Self::update_osc52_clipboard_access_dropdown(...)` arm, add an arm for `CmdArrowLineNavSetting { .. }` that calls `Self::update_cmd_arrow_line_nav_dropdown(me.cmd_arrow_line_nav_dropdown.clone(), ctx)`. Copy the exact structure of the OSC-52 arm (lines 2294-2297), substituting names.

- [ ] **Step 7: Initialize the dropdown**

After the OSC-52 init (lines 2381-2382), add:

```rust
        let cmd_arrow_line_nav_dropdown = ctx.add_typed_action_view(Dropdown::new);
        Self::update_cmd_arrow_line_nav_dropdown(cmd_arrow_line_nav_dropdown.clone(), ctx);
```

- [ ] **Step 8: Add the field to the struct initializer**

After `osc52_clipboard_access_dropdown,` (line 2667), add:

```rust
            cmd_arrow_line_nav_dropdown,
```

- [ ] **Step 9: Push the widget into the terminal widgets list**

After `terminal_widgets.push(Box::new(Osc52ClipboardAccessWidget::default()));` (line 2996), add:

```rust
        terminal_widgets.push(Box::new(CmdArrowLineNavWidget::default()));
```

- [ ] **Step 10: Add the `update_*_dropdown` fn**

After `update_osc52_clipboard_access_dropdown` (ends line 3654), add:

```rust
    fn update_cmd_arrow_line_nav_dropdown(
        dropdown: ViewHandle<Dropdown<FeaturesPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        dropdown.update(ctx, |dropdown, ctx| {
            let values = vec![
                CmdArrowLineNav::Auto,
                CmdArrowLineNav::LineEditing,
                CmdArrowLineNav::HomeEnd,
            ];
            let current_value = *TerminalSettings::as_ref(ctx).cmd_arrow_line_nav;

            let selected_index = values
                .iter()
                .position(|val| *val == current_value)
                .unwrap_or(0);

            dropdown.set_items(
                values
                    .into_iter()
                    .map(|val| {
                        DropdownItem::new(
                            val.as_dropdown_label(),
                            FeaturesPageAction::SetCmdArrowLineNav(val),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown.set_selected_by_index(selected_index, ctx);
        });
    }
```

- [ ] **Step 11: Add the widget struct + impl**

After the `Osc52ClipboardAccessWidget` impl (ends line 7369), add:

```rust
#[derive(Default)]
struct CmdArrowLineNavWidget {}

impl SettingsWidget for CmdArrowLineNavWidget {
    type View = FeaturesPageView;

    fn search_terms(&self) -> &str {
        "cmd arrow left right home end line start cli agent claude code navigation"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        render_dropdown_item(
            appearance,
            "cmd+\u{2190} / cmd+\u{2192} in running programs",
            Some("What cmd+left and cmd+right send while a program is running. Auto sends Home/End to CLI agents like Claude Code and Ctrl-A/Ctrl-E to shells."),
            None,
            LocalOnlyIconState::for_setting(
                CmdArrowLineNavSetting::storage_key(),
                CmdArrowLineNavSetting::sync_to_cloud(),
                &mut view
                    .button_mouse_states
                    .local_only_icon_tooltip_states
                    .borrow_mut(),
                app,
            ),
            None,
            &view.cmd_arrow_line_nav_dropdown,
        )
    }
}
```

Note: `Osc52ClipboardAccessWidget` is declared `struct Osc52ClipboardAccessWidget {}` without a `#[derive(Default)]` on the line I sampled, yet is constructed via `::default()`. Confirm how it derives `Default` (it may have a derive on a line not shown) and match that exact form for `CmdArrowLineNavWidget`.

- [ ] **Step 12: Build**

Run: `cargo build -p warp`
Expected: builds. If the telemetry arm (Step 3) had an unused-variable warning for `nav`, prefix with `_` or include it per the OSC-52 arm's shape.

- [ ] **Step 13: Commit**

```bash
git add app/src/settings_view/features_page.rs
git commit -m "Add settings dropdown for cmd-arrow line navigation"
```

---

## Task 6: Integration test for emitted escape sequences

**Files:**
- Create: `crates/integration/tests/integration/cmd_arrow_line_nav_tests.rs` (or add to the existing keyboard test module — see Step 1)
- Modify: the integration test module registration (`mod` declaration) and, if used, the manual runner

- [ ] **Step 1: Locate the keyboard-protocol test to copy the harness from**

Run: `sed -n '1,80p' crates/integration/src/test/keyboard_protocol.rs`
Expected: shows how a test starts a session, runs a long-running command, sends a keystroke, and asserts on bytes written to the PTY. Use its setup (Builder/TestStep, sending `cmd-left`, asserting output) as the template. Find where its `mod` is registered: `grep -rn "keyboard_protocol" crates/integration`.

- [ ] **Step 2: Write the failing test**

Create a test that, with a CLI-agent session simulated active and the setting at `Auto`, sends `cmd-left` and asserts the PTY receives the Home sequence (`\x1b[H` or `\x1bOH` per cursor mode), and with `LineEditing` asserts it receives `\x01`. Model the assertions and session setup on `keyboard_protocol.rs`. Concrete shape (adapt names to the harness):

```rust
// With Auto + active CLI agent: cmd-left -> Home escape sequence.
test.set_terminal_setting_cmd_arrow_line_nav(CmdArrowLineNav::Auto);
test.start_cli_agent_session();      // makes has_active_cli_agent_session() true
test.run_long_running_command();
test.send_keystroke("cmd-left");
test.assert_pty_received_one_of(&[b"\x1b[H".to_vec(), b"\x1bOH".to_vec()]);

// With LineEditing: cmd-left -> Ctrl-A (SOH).
test.set_terminal_setting_cmd_arrow_line_nav(CmdArrowLineNav::LineEditing);
test.send_keystroke("cmd-left");
test.assert_pty_received(b"\x01");
```

If the harness has no helper to mark a CLI-agent session active, assert the simpler, fully-supported case first: with `LineEditing` and `HomeEnd` (which don't depend on agent state) `cmd-left` emits `\x01` vs the Home escape sequence respectively. Cover the `Auto`+agent case only if the harness exposes agent-session setup; otherwise rely on Task 1's unit test for the `Auto` collapse logic and note the gap in the commit message.

- [ ] **Step 3: Run to verify it fails (before this branch's wiring is present), then passes with it**

Run: `cargo nextest run -p integration cmd_arrow_line_nav` (or the project's documented integration runner — check `crates/integration/README` or `grep -rn "nextest\|integration" justfile Makefile 2>/dev/null`).
Expected: PASS on this branch (the wiring from Tasks 1-4 is in place). If it fails, the emitted bytes in the assertion are the source of truth — reconcile against `move_home`/`move_end` output.

- [ ] **Step 4: Manual smoke test**

```bash
./script/macos/bundle --selfsign --nouniversal --channel oss
```
Launch the built app. Verify:
1. In a shell prompt running a long command's REPL (e.g. `python3`), `cmd+←/→` jump line start/end (Ctrl-A/Ctrl-E behavior) — unchanged.
2. In Claude Code, `cmd+←/→` jump line start/end (now works under default `Auto`).
3. Settings → Features → the new dropdown shows Auto/Line-editing/Home & End and switching to `Line-editing` makes cmd+←/→ stop working in Claude Code (proving the setting is wired).
4. No keybinding-validator errors in logs at startup.

- [ ] **Step 5: Commit**

```bash
git add crates/integration
git commit -m "Add integration test for cmd-arrow line navigation"
```

---

## Final verification

- [ ] Run `cargo build -p warp` — clean build.
- [ ] Run `cargo clippy -p warp` and `cargo fmt` — no new warnings; formatted.
- [ ] Run `cargo test -p warp --lib terminal::settings_tests` — resolver tests pass.
- [ ] Run the integration test from Task 6 — passes.
- [ ] Manual smoke test (Task 6 Step 4) confirms all three behaviors.
