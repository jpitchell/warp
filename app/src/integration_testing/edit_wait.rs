//! Integration-test helpers for the `warp --wait` external-editor back-channel.
//!
//! These let a test drive the real flow end to end: open a file in Warp's code
//! editor via the `open_file_editor` URI action carrying a `&wait=<addr>`
//! back-channel, close that editor tab, and observe the status byte the app
//! writes back when the tab closes — proving the full chain
//! `register` -> `CodeViewEvent::FileClosed` -> `notify_closed` -> `signal`
//! works over a real local socket.
//!
//! Compiled natively only; the back-channel relies on `crate::edit_wait::cli`
//! and `registry`, which are themselves `#[cfg(not(target_family = "wasm"))]`.
#![cfg(not(target_family = "wasm"))]

use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use warpui::{App, WindowId};

use super::view_getters::pane_group_view;
use crate::edit_wait::{build_wait_url, cli, WaitAddr};
use crate::pane_group::PaneGroup;

/// Status byte the app writes back when the watched tab closes normally.
pub use crate::edit_wait::registry::STATUS_CLOSED_OK;

/// A bound back-channel listener standing in for a blocked `warp --wait`
/// process. Binds a real local socket at a trusted [`cli::fresh_addr`] and
/// records the first status bytes the app writes when the watched tab closes.
pub struct WaitProbe {
    addr: WaitAddr,
    received: Arc<Mutex<Option<Vec<u8>>>>,
    _accept: JoinHandle<()>,
}

impl WaitProbe {
    /// Bind the listener and start accepting in the background. Must happen
    /// before the URI is dispatched so the app can always connect.
    pub fn bind() -> std::io::Result<Self> {
        let addr = cli::fresh_addr();
        let listener = cli::bind(&addr)?;
        let received = Arc::new(Mutex::new(None));
        let sink = received.clone();
        let accept = std::thread::spawn(move || {
            if let Ok(mut conn) = listener.accept() {
                let mut buf = Vec::new();
                let _ = conn.read_to_end(&mut buf);
                *sink.lock().unwrap() = Some(buf);
            }
        });
        Ok(Self {
            addr,
            received,
            _accept: accept,
        })
    }

    /// The trusted back-channel address to place in the URL's `wait` param.
    pub fn wait_addr(&self) -> &WaitAddr {
        &self.addr
    }

    /// The status bytes received so far (`None` until the app signals).
    pub fn received(&self) -> Option<Vec<u8>> {
        self.received.lock().unwrap().clone()
    }
}

/// Dispatch the real `open_file_editor` URI action for `path`, carrying the
/// probe's `wait` back-channel address — exactly the URL a forwarded
/// `warp --wait <path>` would deliver to the running app.
pub fn dispatch_open_file_editor_wait(app: &mut App, path: &Path, wait: &WaitAddr) {
    use warp_core::channel::ChannelState;

    let url_str = build_wait_url(ChannelState::url_scheme(), path, None, None, wait);
    let url = url::Url::parse(&url_str).expect("wait URL should parse");
    app.update(|ctx| crate::uri::handle_incoming_uri(&url, ctx));
}

/// Number of open code-editor views in the window's first tab.
pub fn open_code_view_count(app: &App, window_id: WindowId) -> usize {
    let pane_group = pane_group_view(app, window_id, 0);
    pane_group.read(app, |pane_group: &PaneGroup, ctx| {
        pane_group.code_views(ctx).len()
    })
}

/// Close the first open code-editor tab via the real `RemoveTabAtIndex` action,
/// the same path the close-tab keybinding takes. In the integration channel this
/// never raises a save dialog, so it routes straight through `remove_tab_data_index`
/// and emits `CodeViewEvent::FileClosed`.
pub fn close_first_code_tab(app: &mut App, window_id: WindowId) {
    let pane_group = pane_group_view(app, window_id, 0);
    let code_view = pane_group
        .read(app, |pane_group: &PaneGroup, ctx| {
            pane_group.code_views(ctx).into_iter().next()
        })
        .expect("a code-editor view should be open");
    app.update(|ctx| {
        ctx.dispatch_typed_action_for_view(
            window_id,
            code_view.id(),
            &crate::code::view::CodeViewAction::RemoveTabAtIndex { index: 0 },
        );
    });
}
