//! The Source Control left-panel tab: changed-file sections with
//! stage/unstage/discard, a commit box (with AI message generation and a
//! Commit / Commit & Push / Amend split button), branch switching (worktree
//! aware), stashes, worktrees, and commit history.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use warpui::elements::{
    CrossAxisAlignment, Element, Flex, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    ParentElement, ScrollStateHandle, Text, UniformListState,
};
use warpui::fonts::{Properties, Weight};
use warpui::{
    AppContext, BlurContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext,
};

use super::commit_box::CommitBoxState;
use super::dialogs::{ActiveDialog, DialogState};
use super::header::HeaderState;
use super::item::{ItemState, Section, SourceControlListItem};
use crate::appearance::Appearance;
use crate::code::buffer_location::LocalOrRemotePath;
use crate::source_control::{FileChange, SourceControlModel};
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

#[cfg(feature = "local_fs")]
use {
    super::item::{
        build_list_items, render_commit_row, render_empty_hint, render_file_row,
        render_section_header, render_stash_row, render_worktree_row, RowAction,
    },
    super::telemetry::{SourceControlPanelAction, SourceControlTelemetryEvent},
    crate::code_review::diff_state::CommitChainMode,
    crate::code_review::git_actions,
    crate::server::server_api::ServerApiProvider,
    crate::settings::{AISettings, SourceControlSettings},
    crate::source_control::{
        git_ops, GitOpKind, OperationState, SourceControlCacheModel, SourceControlEvent,
    },
    crate::workspaces::user_workspaces::UserWorkspaces,
    repo_metadata::repositories::DetectedRepositories,
    warp_core::send_telemetry_from_ctx,
    warp_core::ui::Icon,
    warpui::elements::{
        AnchorPair, ChildAnchor, ChildView, Fill, OffsetPositioning, OffsetType, ParentAnchor,
        ParentOffsetBounds, PositionedElementOffsetBounds, PositioningAxis, SavePosition,
        Scrollable, ScrollbarWidth, Shrinkable, Stack, UniformList, XAxisAnchor, YAxisAnchor,
    },
    warpui::keymap::macros::*,
    warpui::keymap::FixedBinding,
};

/// Actions handled by the source control view.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceControlViewAction {
    // Keyboard navigation
    ArrowUp,
    ArrowDown,
    Activate,
    ToggleStageSelected,
    Escape,
    SetSelectedIndex(usize),
    ClearSelectedIndex,
    ToggleSection(Section),
    Refresh,
    // Header / branches
    SwitchBranch(String),
    /// A branch checked out in another worktree was picked: cd the current
    /// tab to that worktree instead of switching in place.
    OpenWorktreeForBranch(PathBuf),
    OpenCreateBranchDialog,
    Sync,
    // File rows (repo-relative paths)
    OpenFile(String),
    OpenDiff(String),
    Stage(String),
    Unstage(String),
    RequestDiscard {
        section: Section,
        change: FileChange,
    },
    // Section-header bulk actions
    StageSection(Section),
    UnstageAll,
    RequestDiscardAll,
    OpenStashDialog,
    OpenAddWorktreeDialog,
    // Commit box
    Commit,
    CommitAndPush,
    AmendLastCommit,
    ToggleCommitMenu,
    GenerateCommitMessage,
    // Stashes
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
    // Worktrees
    OpenWorktreeInNewTab(PathBuf),
    RequestRemoveWorktree(PathBuf),
    // Dialogs
    SelectWorktreeBranch(String),
    DialogConfirm,
    DialogConfirmAlt,
    DialogCancel,
}

/// Events emitted to the left panel / workspace.
pub enum Event {
    /// Open a file (absolute path) in the editor.
    OpenFile { path: PathBuf },
    /// Open the code review right panel for `repo_path` and select `file_path`
    /// (repo-relative).
    OpenDiff {
        repo_path: PathBuf,
        file_path: String,
    },
    /// Open a worktree directory in a new tab.
    OpenWorktreeInNewTab { path: PathBuf },
    /// Change the current tab's cwd (worktree-aware branch checkout).
    ChangeDirectory { path: PathBuf },
}

/// Which repository (if any) the panel is currently showing.
enum RepoTarget {
    /// The active session isn't inside a watched local git repository.
    None,
    /// The active session is remote (SSH/WSL) — unsupported in v1.
    RemoteUnsupported,
    /// A local repository tracked by `SourceControlModel`.
    Local(ModelHandle<SourceControlModel>),
}

/// The Source Control left-panel tab.
pub struct SourceControlView {
    repo: RepoTarget,
    /// Flat list rendered by the `UniformList`; rebuilt on model / collapse
    /// changes.
    list_items: Arc<Vec<SourceControlListItem>>,
    collapsed_sections: HashSet<Section>,
    selected_index: Option<usize>,
    item_states: HashMap<String, ItemState>,
    list_state: UniformListState,
    scroll_state: ScrollStateHandle,
    list_hover: MouseStateHandle,
    header: HeaderState,
    commit_box: CommitBoxState,
    dialog: DialogState,
    /// True while a view-driven sync (pull/push) chain is in flight.
    sync_in_flight: bool,
}

impl SourceControlView {
    pub fn init(_app: &mut AppContext) {
        #[cfg(feature = "local_fs")]
        _app.register_fixed_bindings([
            FixedBinding::new(
                "up",
                SourceControlViewAction::ArrowUp,
                id!(SourceControlView::ui_name()),
            ),
            FixedBinding::new(
                "down",
                SourceControlViewAction::ArrowDown,
                id!(SourceControlView::ui_name()),
            ),
            FixedBinding::new(
                "enter",
                SourceControlViewAction::Activate,
                id!(SourceControlView::ui_name()),
            ),
            // Space stages/unstages the selected file row. Scoped to this
            // view's focus id, so it can't shadow text inputs (the commit
            // editor and dialogs are separately focused child views).
            FixedBinding::new(
                "space",
                SourceControlViewAction::ToggleStageSelected,
                id!(SourceControlView::ui_name()),
            ),
            FixedBinding::new(
                "escape",
                SourceControlViewAction::Escape,
                id!(SourceControlView::ui_name()),
            ),
        ]);
    }

    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let header = HeaderState::new(ctx);
        let commit_box = CommitBoxState::new(ctx);
        let dialog = DialogState::new(ctx);

        ctx.subscribe_to_view(&commit_box.commit_menu, |me, _, event, ctx| match event {
            crate::menu::Event::Close { .. } | crate::menu::Event::ItemSelected => {
                me.commit_box.menu_open = false;
                ctx.notify();
            }
            crate::menu::Event::ItemHovered => {}
        });

        for input in [dialog.name_input.clone(), dialog.path_input.clone()] {
            ctx.subscribe_to_view(&input, |me, _, event, ctx| match event {
                crate::view_components::SubmittableTextInputEvent::Submit(_) => {
                    me.handle_dialog_confirm(false, ctx);
                }
                crate::view_components::SubmittableTextInputEvent::Escape => {
                    me.dialog.close(ctx);
                }
            });
        }

        let mut collapsed_sections = HashSet::new();
        // Commits is collapsed by default; history loads lazily on expand.
        collapsed_sections.insert(Section::Commits);

        Self {
            repo: RepoTarget::None,
            list_items: Arc::new(Vec::new()),
            collapsed_sections,
            selected_index: None,
            item_states: HashMap::new(),
            list_state: UniformListState::new(),
            scroll_state: Arc::new(Mutex::new(Default::default())),
            list_hover: MouseStateHandle::default(),
            header,
            commit_box,
            dialog,
            sync_in_flight: false,
        }
    }

    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
        #[cfg(feature = "local_fs")]
        send_telemetry_from_ctx!(
            SourceControlTelemetryEvent::PanelAction {
                action: SourceControlPanelAction::Opened,
            },
            ctx
        );
    }

    /// Updates the repository the panel follows from the active session's
    /// most recent working directory.
    #[cfg(feature = "local_fs")]
    pub fn set_active_directory(
        &mut self,
        directory: Option<LocalOrRemotePath>,
        ctx: &mut ViewContext<Self>,
    ) {
        let new_target = match directory {
            None => RepoTarget::None,
            Some(LocalOrRemotePath::Remote(_)) => RepoTarget::RemoteUnsupported,
            Some(local @ LocalOrRemotePath::Local(_)) => {
                let root = DetectedRepositories::as_ref(ctx)
                    .get_root_for_path(&local)
                    .and_then(|root| root.to_local_path().map(|p| p.to_path_buf()));
                match root {
                    None => RepoTarget::None,
                    Some(root) => {
                        if let RepoTarget::Local(model) = &self.repo {
                            if model.as_ref(ctx).repo_path() == root {
                                return;
                            }
                        }
                        let subscribed = SourceControlCacheModel::handle(ctx)
                            .update(ctx, |cache, ctx| cache.subscribe(&root, ctx));
                        match subscribed {
                            Ok(model) => RepoTarget::Local(model),
                            Err(err) => {
                                log::debug!(
                                    "SourceControlView: no source control model for {}: {err}",
                                    root.display()
                                );
                                RepoTarget::None
                            }
                        }
                    }
                }
            }
        };

        self.repo = new_target;
        self.selected_index = None;

        if let RepoTarget::Local(model) = &self.repo {
            let model = model.clone();
            let limit = *SourceControlSettings::as_ref(ctx).history_commit_limit.value();
            let history_enabled = !self.collapsed_sections.contains(&Section::Commits);
            let worktree_details = !self.collapsed_sections.contains(&Section::Worktrees);
            model.update(ctx, |m, ctx| {
                m.set_history_limit(limit, ctx);
                m.set_history_enabled(history_enabled, ctx);
                m.set_worktree_details_enabled(worktree_details, ctx);
            });
            // Subscriptions to previously tracked models stay alive while
            // another window keeps the model cached, so guard against stale
            // events by checking the emitting model is still ours.
            let model_id = model.id();
            ctx.subscribe_to_model(&model, move |me, _, event, ctx| {
                let is_current =
                    matches!(&me.repo, RepoTarget::Local(current) if current.id() == model_id);
                if is_current {
                    me.handle_model_event(event, ctx);
                }
            });
        }

        self.rebuild_list_items(ctx);
    }

    #[cfg(not(feature = "local_fs"))]
    pub fn set_active_directory(
        &mut self,
        _directory: Option<LocalOrRemotePath>,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    fn show_error_toast(&self, message: String, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
        });
    }

    fn show_success_toast(&self, message: String, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        ToastStack::handle(ctx).update(ctx, |stack, ctx| {
            stack.add_ephemeral_toast(DismissibleToast::success(message), window_id, ctx);
        });
    }
}

#[cfg(feature = "local_fs")]
impl SourceControlView {
    fn model(&self) -> Option<&ModelHandle<SourceControlModel>> {
        match &self.repo {
            RepoTarget::Local(model) => Some(model),
            _ => None,
        }
    }

    /// True while any mutating work is in flight (model op, commit chain, or
    /// sync) — mutating controls are disabled for the duration.
    fn busy(&self, app: &AppContext) -> bool {
        let op_running = self
            .model()
            .is_some_and(|m| m.as_ref(app).op_state() != OperationState::Idle);
        op_running || self.commit_box.committing || self.sync_in_flight
    }

    fn send_action_telemetry(action: SourceControlPanelAction, ctx: &mut ViewContext<Self>) {
        send_telemetry_from_ctx!(
            SourceControlTelemetryEvent::PanelAction { action },
            ctx
        );
    }

    fn handle_model_event(&mut self, event: &SourceControlEvent, ctx: &mut ViewContext<Self>) {
        match event {
            SourceControlEvent::StatusChanged => {
                self.rebuild_list_items(ctx);
            }
            SourceControlEvent::OperationFinished { kind, result } => {
                match result {
                    Err(err) => self.show_error_toast(err.clone(), ctx),
                    Ok(()) => {
                        // A successful amend consumed the typed message.
                        if *kind == GitOpKind::Amend {
                            self.clear_commit_message(ctx);
                        }
                    }
                }
                self.rebuild_list_items(ctx);
            }
        }
    }

    fn clear_commit_message(&self, ctx: &mut ViewContext<Self>) {
        self.commit_box.message_editor.update(ctx, |editor, ctx| {
            editor.system_reset_buffer_text("", ctx);
        });
    }

    /// Rebuilds the flat item list plus the header / commit-box control state
    /// derived from the model.
    fn rebuild_list_items(&mut self, ctx: &mut ViewContext<Self>) {
        let items = match &self.repo {
            RepoTarget::Local(model) => {
                let model = model.as_ref(ctx);
                build_list_items(
                    model.status(),
                    model.stashes(),
                    model.worktrees(),
                    model.history(),
                    &self.collapsed_sections,
                )
            }
            _ => Vec::new(),
        };

        let keys: HashSet<String> = items.iter().map(|item| item.state_key()).collect();
        self.item_states.retain(|key, _| keys.contains(key));
        for key in keys {
            self.item_states.entry(key).or_default();
        }
        self.list_items = Arc::new(items);

        // Clamp / fix the selection.
        if let Some(index) = self.selected_index {
            if index >= self.list_items.len() {
                self.selected_index = None;
            } else if !self.is_selectable(index) {
                self.selected_index =
                    (index..self.list_items.len()).find(|&i| self.is_selectable(i));
            }
        }

        self.refresh_controls(ctx);
        ctx.notify();
    }

    /// Syncs the branch picker items and commit-button enablement with the
    /// model state.
    fn refresh_controls(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(model) = self.model().cloned() else {
            return;
        };
        let (branches, worktrees, current_branch, has_staged) = {
            let model = model.as_ref(ctx);
            let current_branch = model.status().and_then(|status| {
                (!status.branch.detached).then(|| status.branch.head.clone())
            });
            (
                model.branches().to_vec(),
                model.worktrees().to_vec(),
                current_branch,
                model.status().is_some_and(|s| !s.staged.is_empty()),
            )
        };
        self.header
            .refresh_branch_items(&branches, &worktrees, current_branch.as_deref(), ctx);

        let busy = self.busy(ctx);
        // Disabling the split button also disables the Amend menu entry; an
        // accepted trade-off of the shared split-button component.
        let disabled = busy || !has_staged;
        self.commit_box.split_button.set_disabled(disabled, ctx);
        let tooltip = if busy {
            Some("A git operation is in progress".to_string())
        } else if !has_staged {
            Some("Stage changes to commit".to_string())
        } else {
            None
        };
        self.commit_box.split_button.set_tooltip(tooltip, ctx);
    }

    // ── Keyboard navigation ──────────────────────────────────────────

    fn is_selectable(&self, index: usize) -> bool {
        self.list_items
            .get(index)
            .is_some_and(|item| item.is_selectable())
    }

    fn move_selection(&mut self, delta_down: bool, ctx: &mut ViewContext<Self>) {
        let count = self.list_items.len();
        if count == 0 {
            return;
        }
        let next = if delta_down {
            let start = self.selected_index.map(|i| i + 1).unwrap_or(0);
            (start..count).find(|&i| self.is_selectable(i))
        } else {
            let end = self.selected_index.unwrap_or(count);
            (0..end).rev().find(|&i| self.is_selectable(i))
        };
        if let Some(index) = next {
            self.selected_index = Some(index);
            self.list_state.scroll_to(index);
            ctx.notify();
        }
    }

    fn activate_selected(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(item) = self
            .selected_index
            .and_then(|index| self.list_items.get(index))
            .cloned()
        else {
            return;
        };
        match item {
            SourceControlListItem::File { section, change } => {
                if section == Section::Untracked {
                    self.open_file(&change.path, ctx);
                } else {
                    self.open_diff(&change.path, ctx);
                }
            }
            SourceControlListItem::Stash(stash) => {
                self.run_on_model(ctx, |m, ctx| m.stash_apply(stash.index, ctx));
                Self::send_action_telemetry(SourceControlPanelAction::StashApply, ctx);
            }
            SourceControlListItem::Worktree(worktree) => {
                if !worktree.is_current {
                    Self::send_action_telemetry(SourceControlPanelAction::WorktreeOpen, ctx);
                    ctx.emit(Event::OpenWorktreeInNewTab {
                        path: worktree.path,
                    });
                }
            }
            SourceControlListItem::Commit(_)
            | SourceControlListItem::SectionHeader { .. }
            | SourceControlListItem::EmptyHint { .. } => {}
        }
    }

    fn toggle_stage_selected(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(SourceControlListItem::File { section, change }) = self
            .selected_index
            .and_then(|index| self.list_items.get(index))
            .cloned()
        else {
            return;
        };
        match section {
            Section::Staged => {
                Self::send_action_telemetry(SourceControlPanelAction::Unstage, ctx);
                self.run_on_model(ctx, |m, ctx| m.unstage(vec![change.path], ctx));
            }
            Section::Changes | Section::Untracked | Section::Conflicts => {
                Self::send_action_telemetry(SourceControlPanelAction::Stage, ctx);
                self.run_on_model(ctx, |m, ctx| m.stage(vec![change.path], ctx));
            }
            Section::Stashes | Section::Worktrees | Section::Commits => {}
        }
    }

    // ── Repo / file plumbing ─────────────────────────────────────────

    fn repo_path(&self, app: &AppContext) -> Option<PathBuf> {
        self.model().map(|m| m.as_ref(app).repo_path().to_path_buf())
    }

    fn open_file(&mut self, relative_path: &str, ctx: &mut ViewContext<Self>) {
        let Some(repo_path) = self.repo_path(ctx) else {
            return;
        };
        ctx.emit(Event::OpenFile {
            path: repo_path.join(relative_path),
        });
    }

    fn open_diff(&mut self, relative_path: &str, ctx: &mut ViewContext<Self>) {
        let Some(repo_path) = self.repo_path(ctx) else {
            return;
        };
        Self::send_action_telemetry(SourceControlPanelAction::OpenDiff, ctx);
        ctx.emit(Event::OpenDiff {
            repo_path,
            file_path: relative_path.to_string(),
        });
    }

    /// Runs a mutating model operation unless something is already in flight.
    fn run_on_model(
        &mut self,
        ctx: &mut ViewContext<Self>,
        op: impl FnOnce(&mut SourceControlModel, &mut warpui::ModelContext<SourceControlModel>),
    ) {
        if self.busy(ctx) {
            return;
        }
        if let Some(model) = self.model().cloned() {
            model.update(ctx, op);
        }
    }

    // ── Commit / sync / AI ───────────────────────────────────────────

    /// The user's interactive-shell PATH future, forwarded to git/gh so hooks
    /// resolve like an interactive shell. Mirrors
    /// `LocalDiffStateModel::interactive_path_future`.
    fn interactive_path_future(
        ctx: &mut ViewContext<Self>,
    ) -> futures::future::BoxFuture<'static, Option<String>> {
        #[cfg(feature = "local_tty")]
        {
            crate::terminal::local_shell::LocalShellState::handle(ctx)
                .update(ctx, |shell_state, ctx| {
                    shell_state.get_interactive_path_env_var(ctx)
                })
        }
        #[cfg(not(feature = "local_tty"))]
        {
            use futures::FutureExt;
            let _ = ctx;
            futures::future::ready(None).boxed()
        }
    }

    fn start_commit(&mut self, mode: CommitChainMode, ctx: &mut ViewContext<Self>) {
        if self.busy(ctx) {
            return;
        }
        let Some(model) = self.model().cloned() else {
            return;
        };
        let Some(message) = self.commit_box.message(ctx) else {
            self.show_error_toast("Enter a commit message".to_string(), ctx);
            return;
        };
        let (repo_path, branch) = {
            let model = model.as_ref(ctx);
            (
                model.repo_path().to_path_buf(),
                model
                    .status()
                    .map(|s| s.branch.head.clone())
                    .unwrap_or_default(),
            )
        };
        let telemetry_action = match mode {
            CommitChainMode::CommitOnly => SourceControlPanelAction::CommitOnly,
            CommitChainMode::CommitAndPush | CommitChainMode::CommitAndCreatePr => {
                SourceControlPanelAction::CommitAndPush
            }
        };
        Self::send_action_telemetry(telemetry_action, ctx);

        self.commit_box.committing = true;
        self.commit_box.message_editor.update(ctx, |editor, ctx| {
            editor.set_interaction_state(crate::editor::InteractionState::Disabled, ctx);
        });
        self.refresh_controls(ctx);
        ctx.notify();

        let path_future = Self::interactive_path_future(ctx);
        ctx.spawn(
            async move {
                let path_env = path_future.await;
                // The panel's staging UI is the source of truth, so the chain
                // never includes unstaged changes.
                git_actions::run_commit_chain(
                    &repo_path,
                    mode,
                    &message,
                    /* include_unstaged */ false,
                    &branch,
                    /* ai_client (PR-only) */ None,
                    path_env.as_deref(),
                )
                .await
                .map(|_| ())
            },
            move |me, result: anyhow::Result<()>, ctx| {
                me.commit_box.committing = false;
                me.commit_box.message_editor.update(ctx, |editor, ctx| {
                    editor.set_interaction_state(crate::editor::InteractionState::Editable, ctx);
                });
                match result {
                    Ok(()) => {
                        me.clear_commit_message(ctx);
                        let toast = match mode {
                            CommitChainMode::CommitOnly => "Changes committed.",
                            _ => "Changes committed and pushed.",
                        };
                        me.show_success_toast(toast.to_string(), ctx);
                    }
                    Err(err) => {
                        log::error!("Source control commit failed: {err}");
                        me.show_error_toast(err.to_string(), ctx);
                    }
                }
                if let Some(model) = me.model().cloned() {
                    model.update(ctx, |m, ctx| m.refresh(ctx));
                }
                me.refresh_controls(ctx);
                ctx.notify();
            },
        );
    }

    fn start_sync(&mut self, ctx: &mut ViewContext<Self>) {
        if self.busy(ctx) {
            return;
        }
        let Some(model) = self.model().cloned() else {
            return;
        };
        let Some((repo_path, branch, needs_pull, needs_push)) = ({
            let model = model.as_ref(ctx);
            model.status().map(|status| {
                (
                    model.repo_path().to_path_buf(),
                    status.branch.head.clone(),
                    status.branch.behind > 0,
                    status.branch.ahead > 0 || status.branch.upstream.is_none(),
                )
            })
        }) else {
            return;
        };
        if !needs_pull && !needs_push {
            return;
        }
        Self::send_action_telemetry(SourceControlPanelAction::Sync, ctx);
        self.sync_in_flight = true;
        self.refresh_controls(ctx);
        ctx.notify();

        let path_future = Self::interactive_path_future(ctx);
        ctx.spawn(
            async move {
                let path_env = path_future.await;
                if needs_pull {
                    git_ops::pull(&repo_path, path_env.as_deref()).await?;
                }
                if needs_push {
                    crate::util::git::run_push(&repo_path, &branch, path_env.as_deref()).await?;
                }
                Ok(())
            },
            |me, result: anyhow::Result<()>, ctx| {
                me.sync_in_flight = false;
                if let Err(err) = result {
                    log::error!("Source control sync failed: {err}");
                    me.show_error_toast(err.to_string(), ctx);
                }
                if let Some(model) = me.model().cloned() {
                    model.update(ctx, |m, ctx| m.refresh(ctx));
                }
                me.refresh_controls(ctx);
                ctx.notify();
            },
        );
    }

    /// Same consent gate as the code-review git dialog's ✨ features.
    fn git_ops_ai_consent(app: &AppContext) -> bool {
        AISettings::as_ref(app).is_git_operations_autogen_enabled(app)
            && UserWorkspaces::as_ref(app).is_git_operations_ai_enabled()
    }

    fn start_generate_message(&mut self, ctx: &mut ViewContext<Self>) {
        if self.commit_box.generating || self.commit_box.committing {
            return;
        }
        if !Self::git_ops_ai_consent(ctx) {
            self.show_error_toast(
                "AI commit message generation is disabled in settings.".to_string(),
                ctx,
            );
            return;
        }
        let Some(model) = self.model().cloned() else {
            return;
        };
        let (repo_path, branch) = {
            let model = model.as_ref(ctx);
            (
                model.repo_path().to_path_buf(),
                model
                    .status()
                    .map(|s| s.branch.head.clone())
                    .unwrap_or_default(),
            )
        };
        Self::send_action_telemetry(SourceControlPanelAction::AiMessage, ctx);
        self.commit_box.set_generating(true, ctx);
        let ai_client = ServerApiProvider::handle(ctx).as_ref(ctx).get_ai_client();
        ctx.spawn(
            async move {
                // Staged-only diff: that's what the panel commits.
                git_actions::generate_commit_message(&repo_path, &branch, false, ai_client.as_ref())
                    .await
            },
            |me, result: anyhow::Result<String>, ctx| {
                me.commit_box.set_generating(false, ctx);
                match result {
                    Ok(message) => {
                        // User input wins — don't clobber typed text.
                        if me.commit_box.message(ctx).is_none() {
                            me.commit_box.message_editor.update(ctx, |editor, ctx| {
                                editor.system_reset_buffer_text(message.trim(), ctx);
                            });
                        }
                        ctx.notify();
                    }
                    Err(err) => {
                        log::warn!("Source control AI message generation failed: {err}");
                        me.show_error_toast("Couldn't generate a commit message.".to_string(), ctx);
                    }
                }
            },
        );
    }

    // ── Dialogs ──────────────────────────────────────────────────────

    fn open_dialog(&mut self, dialog: ActiveDialog, ctx: &mut ViewContext<Self>) {
        let branches = self
            .model()
            .map(|m| m.as_ref(ctx).branches().to_vec())
            .unwrap_or_default();
        self.dialog.open(dialog, &branches, ctx);
    }

    fn handle_dialog_confirm(&mut self, alt: bool, ctx: &mut ViewContext<Self>) {
        let Some(active) = self.dialog.active.clone() else {
            return;
        };
        match active {
            ActiveDialog::CreateBranch => {
                let Some(name) = self.dialog.name_text(ctx) else {
                    return;
                };
                Self::send_action_telemetry(SourceControlPanelAction::BranchCreate, ctx);
                self.run_on_model(ctx, |m, ctx| m.create_branch(name, None, ctx));
                self.dialog.close(ctx);
            }
            ActiveDialog::StashPush => {
                let message = self.dialog.name_text(ctx);
                let staged_only = alt;
                Self::send_action_telemetry(SourceControlPanelAction::StashPush, ctx);
                self.run_on_model(ctx, |m, ctx| {
                    m.stash_push(message, !staged_only, staged_only, ctx)
                });
                self.dialog.close(ctx);
            }
            ActiveDialog::AddWorktree => {
                let new_branch = self.dialog.name_text(ctx);
                let existing = self.dialog.selected_worktree_branch.clone();
                let (branch, branch_name) = match (new_branch, existing) {
                    (Some(name), _) => (
                        git_ops::WorktreeBranch::New {
                            name: name.clone(),
                            base: None,
                        },
                        name,
                    ),
                    (None, Some(name)) => {
                        (git_ops::WorktreeBranch::Existing(name.clone()), name)
                    }
                    (None, None) => return,
                };
                let Some(repo_path) = self.repo_path(ctx) else {
                    return;
                };
                let path = self
                    .dialog
                    .path_text(ctx)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| git_ops::default_worktree_path(&repo_path, &branch_name));
                Self::send_action_telemetry(SourceControlPanelAction::WorktreeAdd, ctx);
                self.run_on_model(ctx, |m, ctx| m.worktree_add(path, branch, ctx));
                self.dialog.close(ctx);
            }
            ActiveDialog::ConfirmDiscardFile { section, change } => {
                Self::send_action_telemetry(SourceControlPanelAction::Discard, ctx);
                let (tracked, untracked) = if section == Section::Untracked {
                    (vec![], vec![change.path])
                } else {
                    (vec![change.path], vec![])
                };
                self.run_on_model(ctx, |m, ctx| m.discard(tracked, untracked, ctx));
                self.dialog.close(ctx);
            }
            ActiveDialog::ConfirmDiscardAll => {
                Self::send_action_telemetry(SourceControlPanelAction::Discard, ctx);
                let tracked = self
                    .model()
                    .and_then(|m| {
                        m.as_ref(ctx)
                            .status()
                            .map(|s| s.unstaged.iter().map(|c| c.path.clone()).collect())
                    })
                    .unwrap_or_default();
                self.run_on_model(ctx, |m, ctx| m.discard(tracked, vec![], ctx));
                self.dialog.close(ctx);
            }
            ActiveDialog::ConfirmRemoveWorktree { path } => {
                Self::send_action_telemetry(SourceControlPanelAction::WorktreeRemove, ctx);
                self.run_on_model(ctx, |m, ctx| m.worktree_remove(path, false, ctx));
                self.dialog.close(ctx);
            }
        }
    }

    // ── Action handling ──────────────────────────────────────────────

    fn handle_action_impl(
        &mut self,
        action: &SourceControlViewAction,
        ctx: &mut ViewContext<Self>,
    ) {
        use SourceControlViewAction as Action;
        match action {
            Action::ArrowUp => self.move_selection(false, ctx),
            Action::ArrowDown => self.move_selection(true, ctx),
            Action::Activate => self.activate_selected(ctx),
            Action::ToggleStageSelected => self.toggle_stage_selected(ctx),
            Action::Escape => {
                if self.dialog.active.is_some() {
                    self.dialog.close(ctx);
                } else if self.commit_box.menu_open {
                    self.commit_box.menu_open = false;
                    ctx.notify();
                } else {
                    self.selected_index = None;
                    ctx.notify();
                }
            }
            Action::SetSelectedIndex(index) => {
                self.selected_index = Some(*index);
                ctx.notify();
            }
            Action::ClearSelectedIndex => {
                self.selected_index = None;
                ctx.notify();
            }
            Action::ToggleSection(section) => {
                let now_collapsed = !self.collapsed_sections.remove(section);
                if now_collapsed {
                    self.collapsed_sections.insert(*section);
                }
                let expanded = !now_collapsed;
                match section {
                    Section::Commits => {
                        if let Some(model) = self.model().cloned() {
                            model.update(ctx, |m, ctx| m.set_history_enabled(expanded, ctx));
                        }
                    }
                    Section::Worktrees => {
                        if let Some(model) = self.model().cloned() {
                            model
                                .update(ctx, |m, ctx| m.set_worktree_details_enabled(expanded, ctx));
                        }
                    }
                    _ => {}
                }
                self.rebuild_list_items(ctx);
            }
            Action::Refresh => {
                if let Some(model) = self.model().cloned() {
                    model.update(ctx, |m, ctx| m.refresh(ctx));
                }
            }
            Action::SwitchBranch(branch) => {
                Self::send_action_telemetry(SourceControlPanelAction::BranchSwitch, ctx);
                let branch = branch.clone();
                self.run_on_model(ctx, |m, ctx| m.switch_branch(branch, ctx));
            }
            Action::OpenWorktreeForBranch(path) => {
                Self::send_action_telemetry(SourceControlPanelAction::BranchSwitch, ctx);
                ctx.emit(Event::ChangeDirectory { path: path.clone() });
            }
            Action::OpenCreateBranchDialog => {
                self.open_dialog(ActiveDialog::CreateBranch, ctx);
            }
            Action::Sync => self.start_sync(ctx),
            Action::OpenFile(path) => {
                let path = path.clone();
                self.open_file(&path, ctx);
            }
            Action::OpenDiff(path) => {
                let path = path.clone();
                self.open_diff(&path, ctx);
            }
            Action::Stage(path) => {
                Self::send_action_telemetry(SourceControlPanelAction::Stage, ctx);
                let path = path.clone();
                self.run_on_model(ctx, |m, ctx| m.stage(vec![path], ctx));
            }
            Action::Unstage(path) => {
                Self::send_action_telemetry(SourceControlPanelAction::Unstage, ctx);
                let path = path.clone();
                self.run_on_model(ctx, |m, ctx| m.unstage(vec![path], ctx));
            }
            Action::RequestDiscard { section, change } => {
                self.open_dialog(
                    ActiveDialog::ConfirmDiscardFile {
                        section: *section,
                        change: change.clone(),
                    },
                    ctx,
                );
            }
            Action::StageSection(section) => {
                Self::send_action_telemetry(SourceControlPanelAction::Stage, ctx);
                let paths: Vec<String> = self
                    .model()
                    .and_then(|m| {
                        let model = m.as_ref(ctx);
                        model.status().map(|status| {
                            let changes = match section {
                                Section::Conflicts => &status.conflicted,
                                Section::Changes => &status.unstaged,
                                Section::Untracked => &status.untracked,
                                _ => return Vec::new(),
                            };
                            changes.iter().map(|c| c.path.clone()).collect()
                        })
                    })
                    .unwrap_or_default();
                if !paths.is_empty() {
                    self.run_on_model(ctx, |m, ctx| m.stage(paths, ctx));
                }
            }
            Action::UnstageAll => {
                Self::send_action_telemetry(SourceControlPanelAction::Unstage, ctx);
                self.run_on_model(ctx, |m, ctx| m.unstage_all(ctx));
            }
            Action::RequestDiscardAll => {
                self.open_dialog(ActiveDialog::ConfirmDiscardAll, ctx);
            }
            Action::OpenStashDialog => {
                self.open_dialog(ActiveDialog::StashPush, ctx);
            }
            Action::OpenAddWorktreeDialog => {
                self.open_dialog(ActiveDialog::AddWorktree, ctx);
            }
            Action::Commit => self.start_commit(CommitChainMode::CommitOnly, ctx),
            Action::CommitAndPush => self.start_commit(CommitChainMode::CommitAndPush, ctx),
            Action::AmendLastCommit => {
                if self.busy(ctx) {
                    return;
                }
                Self::send_action_telemetry(SourceControlPanelAction::Amend, ctx);
                // Empty input amends with `--no-edit`.
                let message = self.commit_box.message(ctx);
                self.run_on_model(ctx, |m, ctx| m.amend(message, ctx));
            }
            Action::ToggleCommitMenu => {
                self.commit_box.menu_open = !self.commit_box.menu_open;
                ctx.notify();
            }
            Action::GenerateCommitMessage => self.start_generate_message(ctx),
            Action::StashApply(index) => {
                Self::send_action_telemetry(SourceControlPanelAction::StashApply, ctx);
                let index = *index;
                self.run_on_model(ctx, |m, ctx| m.stash_apply(index, ctx));
            }
            Action::StashPop(index) => {
                Self::send_action_telemetry(SourceControlPanelAction::StashPop, ctx);
                let index = *index;
                self.run_on_model(ctx, |m, ctx| m.stash_pop(index, ctx));
            }
            Action::StashDrop(index) => {
                Self::send_action_telemetry(SourceControlPanelAction::StashDrop, ctx);
                let index = *index;
                self.run_on_model(ctx, |m, ctx| m.stash_drop(index, ctx));
            }
            Action::OpenWorktreeInNewTab(path) => {
                Self::send_action_telemetry(SourceControlPanelAction::WorktreeOpen, ctx);
                ctx.emit(Event::OpenWorktreeInNewTab { path: path.clone() });
            }
            Action::RequestRemoveWorktree(path) => {
                self.open_dialog(
                    ActiveDialog::ConfirmRemoveWorktree { path: path.clone() },
                    ctx,
                );
            }
            Action::SelectWorktreeBranch(branch) => {
                self.dialog.selected_worktree_branch = Some(branch.clone());
            }
            Action::DialogConfirm => self.handle_dialog_confirm(false, ctx),
            Action::DialogConfirmAlt => self.handle_dialog_confirm(true, ctx),
            Action::DialogCancel => self.dialog.close(ctx),
        }
    }

    // ── Rendering ────────────────────────────────────────────────────

    fn section_header_actions(section: Section, busy: bool) -> Vec<RowAction> {
        if busy {
            return Vec::new();
        }
        match section {
            Section::Conflicts => vec![RowAction {
                icon: Icon::Plus,
                tooltip: "Stage all (mark resolved)",
                action: SourceControlViewAction::StageSection(Section::Conflicts),
            }],
            Section::Staged => vec![RowAction {
                icon: Icon::Minus,
                tooltip: "Unstage all",
                action: SourceControlViewAction::UnstageAll,
            }],
            Section::Changes => vec![
                RowAction {
                    icon: Icon::ReverseLeft,
                    tooltip: "Discard all changes",
                    action: SourceControlViewAction::RequestDiscardAll,
                },
                RowAction {
                    icon: Icon::Plus,
                    tooltip: "Stage all",
                    action: SourceControlViewAction::StageSection(Section::Changes),
                },
            ],
            Section::Untracked => vec![RowAction {
                icon: Icon::Plus,
                tooltip: "Stage all untracked",
                action: SourceControlViewAction::StageSection(Section::Untracked),
            }],
            Section::Stashes => vec![RowAction {
                icon: Icon::Plus,
                tooltip: "Stash changes",
                action: SourceControlViewAction::OpenStashDialog,
            }],
            Section::Worktrees => vec![RowAction {
                icon: Icon::Plus,
                tooltip: "Add worktree",
                action: SourceControlViewAction::OpenAddWorktreeDialog,
            }],
            Section::Commits => Vec::new(),
        }
    }

    fn file_row_actions(section: Section, change: &FileChange, busy: bool) -> Vec<RowAction> {
        let path = change.path.clone();
        let mut actions = vec![RowAction {
            icon: Icon::File,
            tooltip: "Open file",
            action: SourceControlViewAction::OpenFile(path.clone()),
        }];
        if section != Section::Untracked {
            actions.push(RowAction {
                icon: Icon::GitBranch,
                tooltip: "Open diff",
                action: SourceControlViewAction::OpenDiff(path.clone()),
            });
        }
        if busy {
            return actions;
        }
        match section {
            Section::Conflicts => actions.push(RowAction {
                icon: Icon::Plus,
                tooltip: "Mark resolved (stage)",
                action: SourceControlViewAction::Stage(path),
            }),
            Section::Staged => actions.push(RowAction {
                icon: Icon::Minus,
                tooltip: "Unstage",
                action: SourceControlViewAction::Unstage(path),
            }),
            Section::Changes => {
                actions.push(RowAction {
                    icon: Icon::ReverseLeft,
                    tooltip: "Discard changes",
                    action: SourceControlViewAction::RequestDiscard {
                        section,
                        change: change.clone(),
                    },
                });
                actions.push(RowAction {
                    icon: Icon::Plus,
                    tooltip: "Stage",
                    action: SourceControlViewAction::Stage(path),
                });
            }
            Section::Untracked => actions.push(RowAction {
                icon: Icon::Plus,
                tooltip: "Stage",
                action: SourceControlViewAction::Stage(path),
            }),
            Section::Stashes | Section::Worktrees | Section::Commits => {}
        }
        actions
    }

    fn render_panel(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let Some(model) = self.model() else {
            return render_empty_state(
                "No repository",
                "Open a git repository to use source control.",
                app,
            );
        };
        let busy = self.busy(app);
        let branch_status = model.as_ref(app).status().map(|s| s.branch.clone());

        let header = self.header.render(branch_status.as_ref(), busy, appearance, app);
        let commit_box = self.commit_box.render(appearance, app);

        let list_items = self.list_items.clone();
        let item_states = self.item_states.clone();
        let selected_index = self.selected_index;
        let collapsed = self.collapsed_sections.clone();
        let list = UniformList::new(
            self.list_state.clone(),
            list_items.len(),
            move |range: std::ops::Range<usize>, app: &AppContext| {
                let appearance = Appearance::as_ref(app);
                range
                    .filter_map(|index| {
                        let item = list_items.get(index)?;
                        let state = item_states.get(&item.state_key())?;
                        let is_selected = selected_index == Some(index);
                        Some(match item {
                            SourceControlListItem::SectionHeader { section, count } => {
                                render_section_header(
                                    *section,
                                    *count,
                                    collapsed.contains(section),
                                    Self::section_header_actions(*section, busy),
                                    state,
                                    appearance,
                                    app,
                                )
                            }
                            SourceControlListItem::File { section, change } => {
                                let on_click = if *section == Section::Untracked {
                                    SourceControlViewAction::OpenFile(change.path.clone())
                                } else {
                                    SourceControlViewAction::OpenDiff(change.path.clone())
                                };
                                render_file_row(
                                    change,
                                    Self::file_row_actions(*section, change, busy),
                                    index,
                                    is_selected,
                                    state,
                                    on_click,
                                    appearance,
                                    app,
                                )
                            }
                            SourceControlListItem::Stash(stash) => {
                                let actions = if busy {
                                    Vec::new()
                                } else {
                                    vec![
                                        RowAction {
                                            icon: Icon::Download,
                                            tooltip: "Apply",
                                            action: SourceControlViewAction::StashApply(
                                                stash.index,
                                            ),
                                        },
                                        RowAction {
                                            icon: Icon::Check,
                                            tooltip: "Pop (apply and drop)",
                                            action: SourceControlViewAction::StashPop(stash.index),
                                        },
                                        RowAction {
                                            icon: Icon::Trash,
                                            tooltip: "Drop",
                                            action: SourceControlViewAction::StashDrop(
                                                stash.index,
                                            ),
                                        },
                                    ]
                                };
                                render_stash_row(
                                    stash, actions, index, is_selected, state, appearance, app,
                                )
                            }
                            SourceControlListItem::Worktree(worktree) => {
                                let actions = if busy || worktree.is_current {
                                    Vec::new()
                                } else {
                                    let mut actions = vec![RowAction {
                                        icon: Icon::LinkExternal,
                                        tooltip: "Open in new tab",
                                        action: SourceControlViewAction::OpenWorktreeInNewTab(
                                            worktree.path.clone(),
                                        ),
                                    }];
                                    if !worktree.is_main {
                                        actions.push(RowAction {
                                            icon: Icon::Trash,
                                            tooltip: "Remove worktree",
                                            action: SourceControlViewAction::RequestRemoveWorktree(
                                                worktree.path.clone(),
                                            ),
                                        });
                                    }
                                    actions
                                };
                                render_worktree_row(
                                    worktree, actions, index, is_selected, state, appearance, app,
                                )
                            }
                            SourceControlListItem::Commit(commit) => render_commit_row(
                                commit, index, is_selected, state, appearance, app,
                            ),
                            SourceControlListItem::EmptyHint { text, .. } => {
                                render_empty_hint(text, appearance)
                            }
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
            },
        )
        .finish_scrollable();

        let scrollable = Scrollable::vertical(
            self.scroll_state.clone(),
            list,
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        let column = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(commit_box)
            .with_child(Shrinkable::new(1.0, scrollable).finish())
            .finish();

        let mut stack = Stack::new().with_child(column);

        if self.commit_box.menu_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.commit_box.commit_menu).finish(),
                OffsetPositioning::from_axes(
                    PositioningAxis::relative_to_stack_child(
                        &self.commit_box.save_position_id,
                        PositionedElementOffsetBounds::WindowBySize,
                        OffsetType::Pixel(0.),
                        AnchorPair::new(XAxisAnchor::Left, XAxisAnchor::Left),
                    ),
                    PositioningAxis::relative_to_stack_child(
                        &self.commit_box.save_position_id,
                        PositionedElementOffsetBounds::WindowBySize,
                        OffsetType::Pixel(4.),
                        AnchorPair::new(YAxisAnchor::Bottom, YAxisAnchor::Top),
                    ),
                ),
            );
        }

        if let Some(dialog) = self.dialog.render(appearance, app) {
            stack.add_positioned_overlay_child(
                dialog,
                OffsetPositioning::offset_from_parent(
                    pathfinder_geometry::vector::vec2f(0., 0.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::Center,
                    ChildAnchor::Center,
                ),
            );
        }

        SavePosition::new(
            stack.finish(),
            &format!("source_control_panel_{}", self.commit_box.save_position_id),
        )
        .finish()
    }
}

fn render_empty_state(title: &str, subtitle: &str, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    let title_and_subtitle = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.)
        .with_child(
            Text::new(title, appearance.ui_font_family(), 14.)
                .with_color(theme.main_text_color(theme.background()).into_solid())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        )
        .with_child(
            Text::new(subtitle, appearance.ui_font_family(), 14.)
                .with_color(theme.disabled_ui_text_color().into_solid())
                .finish(),
        )
        .finish();

    Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_main_axis_alignment(MainAxisAlignment::Center)
        .with_child(
            Flex::column()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::Center)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(title_and_subtitle)
                .finish(),
        )
        .finish()
}

impl Entity for SourceControlView {
    type Event = Event;
}

impl TypedActionView for SourceControlView {
    type Action = SourceControlViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        #[cfg(feature = "local_fs")]
        self.handle_action_impl(action, ctx);
        #[cfg(not(feature = "local_fs"))]
        let _ = (action, ctx);
    }
}

impl View for SourceControlView {
    fn ui_name() -> &'static str {
        "SourceControlView"
    }

    fn on_blur(&mut self, _: &BlurContext, ctx: &mut ViewContext<Self>) {
        if !ctx.is_self_or_child_focused() {
            self.selected_index = None;
            ctx.notify();
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        match &self.repo {
            RepoTarget::None => render_empty_state(
                "No repository",
                "Open a git repository to use source control.",
                app,
            ),
            RepoTarget::RemoteUnsupported => render_empty_state(
                "Remote session",
                "Source control isn't supported for remote sessions yet.",
                app,
            ),
            RepoTarget::Local(_) => {
                #[cfg(feature = "local_fs")]
                {
                    self.render_panel(app)
                }
                #[cfg(not(feature = "local_fs"))]
                {
                    render_empty_state(
                        "No repository",
                        "Open a git repository to use source control.",
                        app,
                    )
                }
            }
        }
    }
}
