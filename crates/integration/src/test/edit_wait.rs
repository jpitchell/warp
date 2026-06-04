//! End-to-end coverage for the `warp --wait` external-editor back-channel.
//!
//! Drives the real path a forwarded `warp --wait <file>` takes: dispatch the
//! `open_file_editor` URI action with a trusted `&wait=<addr>` back-channel,
//! assert a code-editor tab opens, close that tab, then assert the app signalled
//! the waiting socket with `STATUS_CLOSED_OK`. This exercises register ->
//! `CodeViewEvent::FileClosed` -> `notify_closed` -> `signal` over a real socket.

use std::sync::Arc;

use warp::integration_testing::edit_wait::{
    close_first_code_tab, dispatch_open_file_editor_wait, open_code_view_count, WaitProbe,
    STATUS_CLOSED_OK,
};
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;
use warpui::{async_assert, async_assert_eq};

use super::{new_builder, Builder};

/// Closing a `--wait` editor tab signals the blocked caller's back-channel.
pub fn test_edit_wait_tab_close_signals_back_channel() -> Builder {
    // Bind the back-channel BEFORE the app runs, mimicking a `warp --wait`
    // process that is already listening when the URL is forwarded.
    let probe = Arc::new(WaitProbe::bind().expect("should bind edit-wait back-channel socket"));

    // A real on-disk file for the editor to open.
    let id: u64 = rand::random();
    let file = std::env::temp_dir().join(format!("warp-edit-wait-it-{id}.txt"));
    std::fs::write(&file, "edit me\n").expect("should write test file");

    let probe_for_open = probe.clone();
    let file_for_open = file.clone();
    let probe_for_assert = probe.clone();

    new_builder()
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(
            new_step_with_default_assertions("Open file via open_file_editor wait URI")
                .with_action(move |app, _window_id, _| {
                    dispatch_open_file_editor_wait(app, &file_for_open, probe_for_open.wait_addr());
                })
                .add_named_assertion("code editor tab opened", |app, window_id| {
                    async_assert!(
                        open_code_view_count(app, window_id) >= 1,
                        "expected a code-editor view to open for the wait request"
                    )
                }),
        )
        .with_step(
            new_step_with_default_assertions("Close the editor tab")
                .with_action(|app, window_id, _| close_first_code_tab(app, window_id))
                // Polled until the byte arrives: the close emits FileClosed, the
                // pane bridge calls notify_closed, and the registry connects to
                // our probe socket and writes STATUS_CLOSED_OK.
                .add_named_assertion(
                    "back-channel received STATUS_CLOSED_OK",
                    move |_app, _window_id| {
                        async_assert_eq!(
                            probe_for_assert.received(),
                            Some(vec![STATUS_CLOSED_OK]),
                            "closing the tab should signal the waiter with STATUS_CLOSED_OK"
                        )
                    },
                ),
        )
}
