//! End-to-end integration tests for the `cmd+left` / `cmd+right` line-navigation
//! keybindings (Mac only).
//!
//! These tests drive a real Warp instance, run a long-running command that reads
//! raw PTY bytes, send the `cmd-left` / `cmd-right` keystrokes, and assert on the
//! bytes the child process actually receives.
//!
//! The `cmd-left` / `cmd-right` bindings are registered with `with_mac_key_binding`
//! and gated by the `id!("Terminal") & !id!("IMEOpen") & id!("LongRunningCommand")`
//! context, so these tests only run on macOS while a long-running command owns the
//! terminal.
//!
//! Coverage notes:
//! - `LineEditing` → Ctrl-A (`0x01`) for cmd-left, Ctrl-E (`0x05`) for cmd-right.
//! - `HomeEnd` → the Home/End escape sequence built by `move_home` / `move_end`.
//!   In the default (non-application-cursor) CSI mode this is `ESC [ H` / `ESC [ F`,
//!   i.e. bytes `0x1b 0x5b 0x48` and `0x1b 0x5b 0x46`.
//! - The `Auto` collapse (Home/End when a CLI-agent session is active, else
//!   Ctrl-A/Ctrl-E) is covered at the unit level by
//!   `app/src/terminal/settings_tests.rs` via `CmdArrowLineNav::resolve`. The
//!   harness has no easy way to mark a `CLIAgentSessionsModel` session active for a
//!   terminal view, so we deliberately do not re-test the agent branch end-to-end.
//!   The default `Auto`-with-no-agent case is equivalent to `LineEditing`
//!   (`0x01`/`0x05`), which the `LineEditing` test below exercises.

use std::time::Duration;

use settings::Setting as _;
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::{
    assert_long_running_block_executing_for_single_terminal_in_tab,
    wait_until_bootstrapped_single_pane_for_tab,
};
use warp::integration_testing::view_getters::single_terminal_view_for_tab;
use warp::terminal::settings::{CmdArrowLineNav, TerminalSettings};
use warpui_core::integration::TestStep;
use warpui_core::{async_assert, SingletonEntity};

use super::new_builder;
use crate::Builder;

/// Builds a step that sets `TerminalSettings.cmd_arrow_line_nav` to `value`.
fn set_cmd_arrow_line_nav(value: CmdArrowLineNav) -> TestStep {
    new_step_with_default_assertions(&format!("Set cmd_arrow_line_nav = {value:?}")).with_action(
        move |app, _, _| {
            TerminalSettings::handle(app).update(app, |settings, ctx| {
                settings
                    .cmd_arrow_line_nav
                    .set_value(value, ctx)
                    .expect("Could not set cmd_arrow_line_nav");
            });
        },
    )
}

/// Builds a step that launches `read_keys.py` as a long-running command and waits
/// for it to report that it is ready to receive input.
fn run_read_keys_script() -> TestStep {
    TestStep::new("Execute read_keys.py")
        .with_typed_characters(&["python3 ~/read_keys.py"])
        .with_keystrokes(&["enter"])
        .add_assertion(assert_long_running_block_executing_for_single_terminal_in_tab(true, 0))
}

/// Builds a step that waits until the script prints "Ready".
fn wait_for_ready() -> TestStep {
    TestStep::new("Wait for script to be ready")
        .add_assertion(|app, window_id| {
            let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
            terminal_view.read(app, |view, _ctx| {
                let model = view.model.lock();
                let output = model.block_list().active_block().output_to_string();
                async_assert!(
                    output.contains("Ready"),
                    "Script should be ready, but output was: {output}"
                )
            })
        })
        .set_timeout(Duration::from_secs(5))
}

/// Builds a step that sends `keystroke` then asserts the script's output contains
/// each of the `expected_bytes` hex strings (e.g. `"0x01"`).
fn send_and_assert_bytes(
    description: &'static str,
    keystroke: &'static str,
    expected_bytes: &'static [&'static str],
) -> TestStep {
    TestStep::new(description)
        .with_keystrokes(&[keystroke])
        .set_timeout(Duration::from_secs(5))
        .add_assertion(move |app, window_id| {
            let terminal_view = single_terminal_view_for_tab(app, window_id, 0);
            terminal_view.read(app, |view, _ctx| {
                let model = view.model.lock();
                let output = model.block_list().active_block().output_to_string();
                let all_present = expected_bytes.iter().all(|b| output.contains(b));
                async_assert!(
                    all_present,
                    "{description}: expected output to contain bytes {expected_bytes:?}, but \
                     output was: {output}"
                )
            })
        })
}

/// `cmd_arrow_line_nav = LineEditing`: cmd-left sends Ctrl-A (0x01),
/// cmd-right sends Ctrl-E (0x05), regardless of CLI-agent state.
pub fn test_cmd_arrow_line_nav_line_editing() -> Builder {
    new_builder()
        .with_setup(setup_python_script!(
            "read_keys.py",
            "../../assets/read_keys.py"
        ))
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_cmd_arrow_line_nav(CmdArrowLineNav::LineEditing))
        .with_step(run_read_keys_script())
        .with_step(wait_for_ready())
        // cmd-left → Ctrl-A (0x01)
        .with_step(send_and_assert_bytes(
            "Send cmd-left (expect Ctrl-A / 0x01)",
            "cmd-left",
            &["0x01"],
        ))
        // cmd-right → Ctrl-E (0x05)
        .with_step(send_and_assert_bytes(
            "Send cmd-right (expect Ctrl-E / 0x05)",
            "cmd-right",
            &["0x05"],
        ))
        .with_step(
            new_step_with_default_assertions("Send Ctrl+C to exit").with_keystrokes(&["ctrl-c"]),
        )
}

/// `cmd_arrow_line_nav = HomeEnd`: cmd-left sends the Home escape sequence,
/// cmd-right sends the End escape sequence. In the default CSI cursor mode these
/// are `ESC [ H` (0x1b 0x5b 0x48) and `ESC [ F` (0x1b 0x5b 0x46), matching what
/// `move_home` / `move_end` build via `EscCodes::build_escape_sequence(.., b"H"/b"F")`.
///
/// If the running program had enabled application-cursor mode (DECCKM), move_home /
/// move_end would emit ESC O H / ESC O F instead; this test covers the default CSI mode.
pub fn test_cmd_arrow_line_nav_home_end() -> Builder {
    new_builder()
        .with_setup(setup_python_script!(
            "read_keys.py",
            "../../assets/read_keys.py"
        ))
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_cmd_arrow_line_nav(CmdArrowLineNav::HomeEnd))
        .with_step(run_read_keys_script())
        .with_step(wait_for_ready())
        // cmd-left → Home sequence ESC [ H (0x1b 0x5b 0x48).
        // 0x1b/0x5b appear in any CSI sequence; 0x48 (H) / 0x46 (F) is the distinguishing byte.
        .with_step(send_and_assert_bytes(
            "Send cmd-left (expect Home sequence ESC [ H)",
            "cmd-left",
            &["0x1b", "0x5b", "0x48"],
        ))
        // cmd-right → End sequence ESC [ F (0x1b 0x5b 0x46).
        // 0x1b/0x5b appear in any CSI sequence; 0x48 (H) / 0x46 (F) is the distinguishing byte.
        .with_step(send_and_assert_bytes(
            "Send cmd-right (expect End sequence ESC [ F)",
            "cmd-right",
            &["0x1b", "0x5b", "0x46"],
        ))
        .with_step(
            new_step_with_default_assertions("Send Ctrl+C to exit").with_keystrokes(&["ctrl-c"]),
        )
}
