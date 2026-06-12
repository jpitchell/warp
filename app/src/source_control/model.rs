//! `SourceControlModel` — one per repo root — plus the singleton
//! `SourceControlCacheModel` that caches/factories them.
//!
//! Mirrors the structure of `code_review::git_status_update`
//! ([`GitStatusUpdateModel`] / [`GitRepoStatusModel`]): a singleton cache keyed
//! by repo path, a per-repo model owning a filesystem watcher, and a throttled
//! refresh. The differences from that template are documented inline.

#[cfg(feature = "local_fs")]
use std::path::{Path, PathBuf};

#[cfg(feature = "local_fs")]
use warpui::ModelContext;
use warpui::{Entity, SingletonEntity};
#[cfg(feature = "local_fs")]
use {
    crate::source_control::git_ops,
    crate::source_control::status::{
        parse_porcelain_v2, CommitEntry, RepoStatus, StashEntry, WorktreeEntry,
    },
    crate::throttle::throttle,
    crate::util::git::{get_all_branches, git_operation_in_progress, BranchEntry},
    async_channel::Sender,
    repo_metadata::{
        repositories::DetectedRepositories,
        repository::{RepositorySubscriber, SubscriberId},
        Repository, RepositoryUpdate,
    },
    std::collections::HashMap,
    std::time::Duration,
    warp_util::git::run_git_command,
    warpui::r#async::SpawnedFutureHandle,
    warpui::{ModelHandle, WeakModelHandle},
};

/// Identifies which kind of mutating git operation is in flight, so the view
/// can render an appropriate spinner / disabled state and the
/// `OperationFinished` event can be routed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitOpKind {
    Stage,
    Unstage,
    Discard,
    StageAll,
    UnstageAll,
    SwitchBranch,
    CreateBranch,
    DeleteBranch,
    Pull,
    StashPush,
    StashApply,
    StashPop,
    StashDrop,
    WorktreeAdd,
    WorktreeRemove,
    Amend,
}

/// The mutating-operation state machine. At most one mutating op runs at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OperationState {
    #[default]
    Idle,
    Running {
        kind: GitOpKind,
    },
}

/// Events emitted by [`SourceControlModel`].
#[derive(Clone, Debug)]
pub enum SourceControlEvent {
    /// Emitted whenever the cached status/stashes/worktrees/history change.
    StatusChanged,
    /// Emitted when a mutating operation completes.
    OperationFinished {
        kind: GitOpKind,
        result: Result<(), String>,
    },
}

// ── SourceControlCacheModel (singleton) ──────────────────────────────────────

/// Singleton cache / factory for per-repo [`SourceControlModel`] instances.
/// Multiple views in the same repo share a single sub-model; when the last
/// strong handle is dropped the model (and its watcher) is torn down.
pub struct SourceControlCacheModel {
    #[cfg(feature = "local_fs")]
    models: HashMap<PathBuf, WeakModelHandle<SourceControlModel>>,
}

#[cfg(not(feature = "local_fs"))]
#[allow(dead_code)]
impl SourceControlCacheModel {
    pub fn new() -> Self {
        Self {}
    }
}

#[cfg(feature = "local_fs")]
impl SourceControlCacheModel {
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
        }
    }

    /// Get or create a per-repo source-control model for `repo_path`.
    pub fn subscribe(
        &mut self,
        repo_path: &Path,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<ModelHandle<SourceControlModel>> {
        let repo_path_buf = repo_path.to_path_buf();

        if let Some(weak) = self.models.get(&repo_path_buf) {
            if let Some(handle) = weak.upgrade(ctx) {
                return Ok(handle);
            }
        }

        let Some(repository_model) =
            DetectedRepositories::as_ref(ctx).get_local_watched_repo_for_path(repo_path, ctx)
        else {
            anyhow::bail!(
                "No watched repository found for path: {}",
                repo_path.display()
            );
        };

        let handle = ctx
            .add_model(|ctx| SourceControlModel::new(repo_path_buf.clone(), repository_model, ctx));
        self.models.insert(repo_path_buf, handle.downgrade());
        Ok(handle)
    }
}

impl Entity for SourceControlCacheModel {
    type Event = ();
}

impl SingletonEntity for SourceControlCacheModel {}

// ── SourceControlModel ───────────────────────────────────────────────────────

#[cfg(not(feature = "local_fs"))]
#[allow(dead_code)]
pub struct SourceControlModel;

#[cfg(not(feature = "local_fs"))]
impl Entity for SourceControlModel {
    type Event = SourceControlEvent;
}

/// Per-repository source-control state: working-tree status, stashes, worktrees,
/// commit history, branches, and the mutating-operation state machine.
#[cfg(feature = "local_fs")]
pub struct SourceControlModel {
    repo_path: PathBuf,
    repository: ModelHandle<Repository>,
    subscriber_id: Option<SubscriberId>,

    status: Option<RepoStatus>,
    stashes: Vec<StashEntry>,
    worktrees: Vec<WorktreeEntry>,
    history: Vec<CommitEntry>,
    branches: Vec<BranchEntry>,

    op_state: OperationState,
    last_error: Option<String>,

    /// When true, `refresh` also loads `history`.
    history_enabled: bool,
    /// When true, `refresh` includes per-worktree dirty checks.
    worktree_details_enabled: bool,
    /// Number of commits fetched for history.
    history_limit: usize,

    refresh_abort_handle: Option<SpawnedFutureHandle>,
}

#[cfg(feature = "local_fs")]
impl Entity for SourceControlModel {
    type Event = SourceControlEvent;
}

/// Snapshot of the data loaded by a single async `refresh`.
#[cfg(feature = "local_fs")]
#[derive(Default)]
struct RefreshResult {
    status: Option<RepoStatus>,
    stashes: Vec<StashEntry>,
    worktrees: Vec<WorktreeEntry>,
    history: Vec<CommitEntry>,
    branches: Vec<BranchEntry>,
}

#[cfg(feature = "local_fs")]
const DEFAULT_HISTORY_LIMIT: usize = 50;

/// Refresh debounce — finer than the git chip's 5 s throttle because this is an
/// interactive panel.
#[cfg(feature = "local_fs")]
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(500);

#[cfg(feature = "local_fs")]
impl SourceControlModel {
    fn new(
        repo_path: PathBuf,
        repository_model: ModelHandle<Repository>,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let mut model = Self {
            repo_path: repo_path.clone(),
            repository: repository_model.clone(),
            subscriber_id: None,
            status: None,
            stashes: Vec::new(),
            worktrees: Vec::new(),
            history: Vec::new(),
            branches: Vec::new(),
            op_state: OperationState::Idle,
            last_error: None,
            history_enabled: false,
            worktree_details_enabled: false,
            history_limit: DEFAULT_HISTORY_LIMIT,
            refresh_abort_handle: None,
        };

        model.refresh(ctx);

        // Start watching for filesystem changes (worktree-aware in the watcher).
        let (repository_update_tx, repository_update_rx) = async_channel::unbounded();
        let (throttled_tx, throttled_rx) = async_channel::unbounded();
        let start = repository_model.update(ctx, |repo, ctx| {
            repo.start_watching(
                Box::new(SourceControlRepositorySubscriber {
                    repository_update_tx,
                }),
                ctx,
            )
        });
        model.subscriber_id = Some(start.subscriber_id);

        ctx.spawn(start.registration_future, |me, result, ctx| {
            if let Err(err) = result {
                log::warn!("SourceControlModel: watcher registration failed: {err}");
                if let Some(subscriber_id) = me.subscriber_id.take() {
                    me.repository.update(ctx, |repo, ctx| {
                        repo.stop_watching(subscriber_id, ctx);
                    });
                }
            }
        });

        {
            let throttled_tx_clone = throttled_tx;
            ctx.spawn_stream_local(
                repository_update_rx,
                move |_me, update: RepositoryUpdate, _ctx| {
                    if Self::should_refresh(&update) {
                        let _ = throttled_tx_clone.try_send(());
                    }
                },
                |_, _| {},
            );
        }

        ctx.spawn_stream_local(
            throttle(REFRESH_DEBOUNCE, throttled_rx),
            |me, _, ctx| {
                me.refresh(ctx);
            },
            |_, _| {},
        );

        model
    }

    // ── Read accessors ───────────────────────────────────────────────

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    pub fn status(&self) -> Option<&RepoStatus> {
        self.status.as_ref()
    }

    pub fn stashes(&self) -> &[StashEntry] {
        &self.stashes
    }

    pub fn worktrees(&self) -> &[WorktreeEntry] {
        &self.worktrees
    }

    pub fn history(&self) -> &[CommitEntry] {
        &self.history
    }

    pub fn branches(&self) -> &[BranchEntry] {
        &self.branches
    }

    pub fn op_state(&self) -> OperationState {
        self.op_state
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    // ── Lazy-section toggles ─────────────────────────────────────────

    /// Enables / disables loading of commit history. The view calls this when
    /// the history section is expanded / collapsed.
    pub fn set_history_enabled(&mut self, enabled: bool, ctx: &mut ModelContext<Self>) {
        if self.history_enabled != enabled {
            self.history_enabled = enabled;
            if enabled {
                self.refresh(ctx);
            }
        }
    }

    /// Enables / disables per-worktree dirty checks.
    pub fn set_worktree_details_enabled(&mut self, enabled: bool, ctx: &mut ModelContext<Self>) {
        if self.worktree_details_enabled != enabled {
            self.worktree_details_enabled = enabled;
            if enabled {
                self.refresh(ctx);
            }
        }
    }

    /// Sets the number of commits loaded for history.
    pub fn set_history_limit(&mut self, limit: usize, ctx: &mut ModelContext<Self>) {
        if self.history_limit != limit {
            self.history_limit = limit;
            if self.history_enabled {
                self.refresh(ctx);
            }
        }
    }

    // ── Refresh ──────────────────────────────────────────────────────

    /// Aborts any in-flight refresh and spawns a fresh load of status, stashes,
    /// and worktrees; history is loaded only while its section is expanded.
    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        if let Some(handle) = self.refresh_abort_handle.take() {
            handle.abort();
        }
        let repo_path = self.repo_path.clone();
        let history_enabled = self.history_enabled;
        let history_limit = self.history_limit;
        self.refresh_abort_handle = Some(ctx.spawn(
            async move { Self::load(repo_path, history_enabled, history_limit).await },
            |me, result, ctx| {
                me.apply_refresh(result, ctx);
            },
        ));
    }

    async fn load(
        repo_path: PathBuf,
        history_enabled: bool,
        history_limit: usize,
    ) -> RefreshResult {
        let mut result = RefreshResult::default();

        match run_git_command(&repo_path, &["status", "--porcelain=v2", "--branch", "-z"]).await {
            Ok(output) => result.status = Some(parse_porcelain_v2(&output)),
            Err(err) => log::warn!("SourceControlModel: git status failed: {err}"),
        }

        result.stashes = git_ops::stash_list(&repo_path).await.unwrap_or_default();
        result.worktrees = git_ops::worktree_list(&repo_path, Some(&repo_path))
            .await
            .unwrap_or_default();
        result.branches = get_all_branches(&repo_path, None, false)
            .await
            .unwrap_or_default();

        if history_enabled {
            result.history = git_ops::log_recent(&repo_path, history_limit)
                .await
                .unwrap_or_default();
        }

        result
    }

    fn apply_refresh(&mut self, result: RefreshResult, ctx: &mut ModelContext<Self>) {
        self.status = result.status;
        self.stashes = result.stashes;
        self.worktrees = result.worktrees;
        self.branches = result.branches;
        if self.history_enabled {
            self.history = result.history;
        }
        ctx.emit(SourceControlEvent::StatusChanged);
    }

    /// Decide whether a `RepositoryUpdate` warrants a refresh. Mirrors
    /// `GitRepoStatusModel::should_refresh_metadata`.
    fn should_refresh(update: &RepositoryUpdate) -> bool {
        if update.is_empty() {
            return false;
        }
        if update.commit_updated || update.index_lock_detected || update.remote_ref_updated {
            return true;
        }
        let changed_count = update
            .added
            .iter()
            .chain(&update.modified)
            .chain(&update.deleted)
            .chain(update.moved.keys())
            .chain(update.moved.values())
            .filter(|f| !f.is_ignored)
            .count();
        changed_count > 0
    }

    // ── Mutating operations ──────────────────────────────────────────

    /// Returns true when a mutating op may begin: nothing else running here and
    /// no merge/rebase/lock in progress on disk.
    fn can_start_op(&self) -> bool {
        self.op_state == OperationState::Idle && !git_operation_in_progress(&self.repo_path)
    }

    /// Runs a mutating op behind the operation state machine: refuses when busy,
    /// flips to `Running { kind }`, runs `op`, then on completion returns to
    /// `Idle`, emits `OperationFinished`, and refreshes immediately.
    fn run_op<F>(&mut self, kind: GitOpKind, op: F, ctx: &mut ModelContext<Self>)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        if !self.can_start_op() {
            let result = Err("A git operation is already in progress".to_string());
            ctx.emit(SourceControlEvent::OperationFinished { kind, result });
            return;
        }
        self.op_state = OperationState::Running { kind };
        self.last_error = None;
        ctx.emit(SourceControlEvent::StatusChanged);

        ctx.spawn(op, move |me, result, ctx| {
            me.op_state = OperationState::Idle;
            let result = result.map_err(|e| e.to_string());
            if let Err(err) = &result {
                me.last_error = Some(err.clone());
            }
            ctx.emit(SourceControlEvent::OperationFinished {
                kind,
                result: result.clone(),
            });
            me.refresh(ctx);
        });
    }

    pub fn stage(&mut self, paths: Vec<String>, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::Stage,
            async move { git_ops::stage_paths(&repo, &paths).await },
            ctx,
        );
    }

    pub fn unstage(&mut self, paths: Vec<String>, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::Unstage,
            async move { git_ops::unstage_paths(&repo, &paths).await },
            ctx,
        );
    }

    pub fn stage_all(&mut self, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::StageAll,
            async move { git_ops::stage_all(&repo).await },
            ctx,
        );
    }

    pub fn unstage_all(&mut self, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::UnstageAll,
            async move { git_ops::unstage_all(&repo).await },
            ctx,
        );
    }

    /// Discards changes to tracked files (restore + clean) and/or untracked
    /// files (clean). `untracked` paths are cleaned; `tracked` paths are
    /// restored from HEAD.
    pub fn discard(
        &mut self,
        tracked: Vec<String>,
        untracked: Vec<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::Discard,
            async move {
                if !tracked.is_empty() {
                    git_ops::git_restore_and_clean(&repo, &tracked, "HEAD").await?;
                }
                if !untracked.is_empty() {
                    git_ops::discard_untracked(&repo, &untracked).await?;
                }
                Ok(())
            },
            ctx,
        );
    }

    pub fn switch_branch(&mut self, branch: String, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::SwitchBranch,
            async move { git_ops::switch_branch(&repo, &branch).await },
            ctx,
        );
    }

    pub fn create_branch(
        &mut self,
        name: String,
        base: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::CreateBranch,
            async move { git_ops::create_branch(&repo, &name, base.as_deref()).await },
            ctx,
        );
    }

    pub fn delete_branch(&mut self, name: String, force: bool, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::DeleteBranch,
            async move { git_ops::delete_branch(&repo, &name, force).await },
            ctx,
        );
    }

    pub fn pull(&mut self, path_env: Option<String>, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::Pull,
            async move { git_ops::pull(&repo, path_env.as_deref()).await },
            ctx,
        );
    }

    pub fn stash_push(
        &mut self,
        message: Option<String>,
        include_untracked: bool,
        staged_only: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::StashPush,
            async move {
                git_ops::stash_push(&repo, message.as_deref(), include_untracked, staged_only).await
            },
            ctx,
        );
    }

    pub fn stash_apply(&mut self, index: usize, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::StashApply,
            async move { git_ops::stash_apply(&repo, index).await },
            ctx,
        );
    }

    pub fn stash_pop(&mut self, index: usize, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::StashPop,
            async move { git_ops::stash_pop(&repo, index).await },
            ctx,
        );
    }

    pub fn stash_drop(&mut self, index: usize, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::StashDrop,
            async move { git_ops::stash_drop(&repo, index).await },
            ctx,
        );
    }

    pub fn worktree_add(
        &mut self,
        path: PathBuf,
        branch: git_ops::WorktreeBranch,
        ctx: &mut ModelContext<Self>,
    ) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::WorktreeAdd,
            async move { git_ops::worktree_add(&repo, &path, branch).await },
            ctx,
        );
    }

    pub fn worktree_remove(&mut self, path: PathBuf, force: bool, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::WorktreeRemove,
            async move { git_ops::worktree_remove(&repo, &path, force).await },
            ctx,
        );
    }

    pub fn amend(&mut self, message: Option<String>, ctx: &mut ModelContext<Self>) {
        let repo = self.repo_path.clone();
        self.run_op(
            GitOpKind::Amend,
            async move {
                git_ops::run_commit_amend(&repo, message.as_deref())
                    .await
                    .map(|_| ())
            },
            ctx,
        );
    }
}

#[cfg(feature = "local_fs")]
impl Drop for SourceControlModel {
    fn drop(&mut self) {
        if let Some(handle) = self.refresh_abort_handle.take() {
            handle.abort();
        }
    }
}

// ── Repository subscriber adapter ────────────────────────────────────────────

#[cfg(feature = "local_fs")]
struct SourceControlRepositorySubscriber {
    repository_update_tx: Sender<RepositoryUpdate>,
}

#[cfg(feature = "local_fs")]
impl RepositorySubscriber for SourceControlRepositorySubscriber {
    fn on_scan(
        &mut self,
        _repository: &Repository,
        _ctx: &mut ModelContext<Repository>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(async {})
    }

    fn on_files_updated(
        &mut self,
        repository: &Repository,
        update: &RepositoryUpdate,
        _ctx: &mut ModelContext<Repository>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let tx = self.repository_update_tx.clone();
        let update = update.clone();
        let index_lock_path = repository.git_dir().join("index.lock");
        Box::pin(async move {
            // Suppress commit_updated events while the index is locked to avoid
            // reacting to stale intermediate state during git operations.
            if update.commit_updated && async_fs::metadata(&index_lock_path).await.is_ok() {
                return;
            }
            let _ = tx.send(update).await;
        })
    }
}
