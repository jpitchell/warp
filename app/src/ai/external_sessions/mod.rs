//! Surfaces externally-run Claude Code / Codex sessions (scanned from `~/.claude`
//! and `~/.codex`) so they can appear in the Agent Conversations pane alongside
//! Warp's own conversations.
//!
//! [`ExternalSessionsModel`] owns a lightweight on-disk index (see [`index`]).
//! Scanning is cheap (stat + head-read) and runs off the main thread. Full
//! transcript parsing for the read-only preview is the lazy tier in [`transcript`].

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod index;
pub(crate) mod transcript;

use std::collections::HashMap;

use warp_core::features::FeatureFlag;
use warpui::{Entity, ModelContext, SingletonEntity};

use self::claude::ClaudeSessionReader;
use self::codex::CodexSessionReader;
use self::index::ExternalSessionReader;
pub use self::index::{ExternalAgentKind, ExternalSessionId, ExternalSessionIndexEntry};

/// Singleton model holding the scanned index of external agent sessions.
pub struct ExternalSessionsModel {
    entries: Vec<ExternalSessionIndexEntry>,
    by_id: HashMap<ExternalSessionId, ExternalSessionIndexEntry>,
    /// Guards against overlapping scans.
    refreshing: bool,
}

#[derive(Debug, Clone)]
pub enum ExternalSessionsModelEvent {
    /// The index was refreshed (entries may have been added, removed, or updated).
    Updated,
}

impl Entity for ExternalSessionsModel {
    type Event = ExternalSessionsModelEvent;
}

impl SingletonEntity for ExternalSessionsModel {}

impl ExternalSessionsModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let mut model = Self {
            entries: Vec::new(),
            by_id: HashMap::new(),
            refreshing: false,
        };
        model.refresh(ctx);
        model
    }

    /// All indexed sessions, newest first.
    pub fn entries(&self) -> &[ExternalSessionIndexEntry] {
        &self.entries
    }

    pub fn get(&self, id: &ExternalSessionId) -> Option<&ExternalSessionIndexEntry> {
        self.by_id.get(id)
    }

    /// Kick off an off-thread rescan of both agents' session directories. The
    /// result is applied on the main thread. No-op when the feature is disabled
    /// or a scan is already in flight.
    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        if !FeatureFlag::ExternalAgentSessionsInConversations.is_enabled() || self.refreshing {
            return;
        }
        self.refreshing = true;
        // `ctx.spawn` runs the future on a background thread, so the blocking
        // filesystem scan here never touches the UI thread; the callback then
        // applies the result back on the main thread.
        ctx.spawn(async move { scan_all() }, |me, entries, ctx| {
            me.apply_scan(entries, ctx)
        });
    }

    fn apply_scan(
        &mut self,
        mut entries: Vec<ExternalSessionIndexEntry>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.refreshing = false;
        entries.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));

        if entries == self.entries {
            return;
        }
        self.by_id = entries
            .iter()
            .map(|entry| (entry.id, entry.clone()))
            .collect();
        self.entries = entries;
        ctx.emit(ExternalSessionsModelEvent::Updated);
    }
}

/// Scan both supported agents. Runs on a background thread.
fn scan_all() -> Vec<ExternalSessionIndexEntry> {
    let readers: [&dyn ExternalSessionReader; 2] = [&ClaudeSessionReader, &CodexSessionReader];
    let mut entries = Vec::new();
    for reader in readers {
        entries.extend(reader.scan_index());
    }
    entries
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
