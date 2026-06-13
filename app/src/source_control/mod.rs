//! Backend data layer for the Source Control panel.
//!
//! - [`status`]: pure porcelain-v2 / worktree / stash / log parsers.
//! - [`git_ops`]: thin async git CLI wrappers (staging, discard, branches,
//!   stashes, worktrees, history).
//! - [`model`]: [`SourceControlModel`] (one per repo root) and the singleton
//!   [`SourceControlCacheModel`] that caches them, driven by the repository
//!   file watcher.

pub mod git_ops;
pub mod model;
pub mod status;

// Re-exported for the Source Control view layer (built concurrently in
// `workspace/view/source_control/`); not all are consumed within this crate yet.
#[allow(unused_imports)]
pub use model::{
    GitOpKind, OperationState, SourceControlCacheModel, SourceControlEvent, SourceControlModel,
};
#[allow(unused_imports)]
pub use status::{BranchStatus, CommitEntry, FileChange, RepoStatus, StashEntry, WorktreeEntry};
