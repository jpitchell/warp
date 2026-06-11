# Source Control Panel — Implementation Plan

A VS Code–style Source Control panel for Warp, living as a new left-panel toolbelt tab.

## Summary

Warp surfaces git state in scattered places (prompt context chips, the code review right
panel, AI commit flows) but has no dedicated interactive source-control surface. This plan
adds a Source Control tab to the left panel with:

- Changed-files sections: Merge Conflicts / Staged Changes / Changes / Untracked
- Stage / unstage / discard, per file and per section
- Commit box with Commit / Commit & Push / Amend and ✨ AI commit-message generation
- Stash management: list, apply, pop, drop, stash all, stash staged
- Worktree management: list, add, remove, open in new tab, dirty indicators
- Branch management with **worktree-aware checkout** (see below)
- Collapsible commit history section with unpushed markers
- Live updates driven by the existing repository file watcher

Out of scope for v1: hunk-level staging, remote (SSH/WSL) repos, multi-repo header
switcher, drag interactions.

### Worktree-aware branch checkout

The branch picker mirrors the behavior of the existing prompt branch chip
(`app/src/context_chips/git_branch_on_click.rs`, `app/src/context_chips/display_chip.rs:449-466`):

- Branch not checked out anywhere → `git switch <branch>` in the current repo (files change
  in place; the tab's cwd is unchanged).
- Branch already checked out in another worktree → git would refuse a plain checkout, so the
  picker shows a worktree icon and selecting it **changes the current tab's cwd** to that
  worktree's path, reusing the `PromptChipShellCommand::ChangeDirectory` execution path
  (`PromptDisplayChipEvent::TryExecuteCommand`).
- "Create new branch…" optionally creates it in a new worktree (`git worktree add`) and then
  cds the current tab there.

## Architecture

### Data layer — new `app/src/source_control/`

```
app/src/source_control/
  mod.rs            // exports; #[cfg(feature = "local_fs")] stubs like git_status_update.rs
  model.rs          // SourceControlModel + SourceControlCacheModel (singleton)
  status.rs         // porcelain v2 parser + RepoStatus structs (pure, unit-testable)
  status_tests.rs
  git_ops.rs        // new git CLI wrappers (stash/worktree/branch/stage)
  git_ops_tests.rs
```

**`status.rs`** — pure `parse_porcelain_v2()` over `git status --porcelain=v2 --branch -z`
(`1`/`2`/`u`/`?` records, `# branch.ab +A -B` headers) producing:

```rust
pub struct RepoStatus {
    pub branch: BranchStatus,          // head, upstream, ahead, behind, detached
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<FileChange>,
    pub conflicted: Vec<FileChange>,
}
pub struct FileChange { pub path: String, pub orig_path: Option<String>, pub status: GitFileStatus }
pub struct StashEntry { pub index: usize, pub message: String, pub branch: Option<String>, pub age: Option<String> }
pub struct WorktreeEntry { pub path: PathBuf, pub branch: Option<String>, pub head: String,
                           pub is_current: bool, pub is_main: bool, pub locked: bool }
pub struct CommitEntry { pub sha: String, pub short_sha: String, pub subject: String,
                         pub author: String, pub relative_time: String, pub is_unpushed: bool }
```

`GitFileStatus` is reused from `app/src/code_review/diff_state/mod.rs:94`. Its existing
porcelain `TryFrom<&str>` is lossy (no staged/unstaged split), hence the new parser.

**`model.rs`** — `SourceControlModel`, one per repo root, cached through a singleton
`SourceControlCacheModel` (mirror `GitStatusUpdateModel::subscribe`,
`app/src/code_review/git_status_update.rs:79`).

- Watcher subscription: copy the pattern at `git_status_update.rs:195-260` —
  `repo_metadata::Repository::start_watching` with a `RepositorySubscriber`, filtering with
  the same `should_refresh` logic (commit updated / index.lock / remote ref / non-ignored
  file changes). The watcher is already worktree-aware
  (`crates/repo_metadata/src/watcher.rs:148` routes `.git/worktrees/<name>/…` tiers).
  Debounce ~500 ms (the chip's 5 s throttle is too coarse for an interactive panel).
- Refresh: abort any in-flight refresh, then run `git status --porcelain=v2 --branch -z`,
  `git stash list`, `git worktree list --porcelain`, and `git log -n <limit>`; history and
  per-worktree dirty checks are fetched lazily, only while their sections are expanded.
- Operation state machine: `OperationState::{Idle, Running { kind: GitOpKind }}`. One
  mutating op at a time; mutating buttons disabled + spinner while `Running`; on completion
  emit `OperationFinished { kind, result }` (errors → dismissible toast) and refresh
  immediately rather than waiting for the watcher. Ops are refused while
  `git_operation_in_progress()` (`app/src/util/git.rs:512`) reports a merge/rebase/etc. in
  flight — same guard the code review dialog uses.
- Repo resolution: follow the active session's repo, the same data flow code review uses
  (`terminal/view.rs` `current_repo_path()`); remote/SSH repos render a "not supported"
  empty state in v1.

**`git_ops.rs`** — thin async wrappers over `warp_util::git::run_git_command`
(`crates/warp_util/src/git.rs`), matching `app/src/util/git.rs` style (free `async fn`,
`anyhow::Result`, `local_fs`-gated, `[GIT OPERATION]` debug logs):

- Staging: `stage_paths` (`git add --`), `unstage_paths` (`git restore --staged --`),
  `stage_all`, `unstage_all`
- Discard: lift the proven `LocalDiffStateModel::git_restore_and_clean` /
  `stash_uncommitted_changes` helpers (`app/src/code_review/diff_state/local.rs:579,624`)
  into shared functions rather than reimplementing
- Branches: `switch_branch`, `create_branch`, `delete_branch`, `pull` (`--ff-only`)
- Stash: `stash_list`, `stash_push` (`-u` / `--staged`), `stash_apply`, `stash_pop`,
  `stash_drop`
- Worktrees: `worktree_list`, `worktree_add`, `worktree_remove`
- History: `log_recent`; unpushed markers via `compute_unpushed_state`
  (`app/src/util/git.rs:479`)
- `run_commit_amend` (new primitive; don't overload the shared commit chain)

Commit / push / AI message are **not** reimplemented: reuse
`app/src/code_review/git_actions.rs` (`run_commit_chain` with `include_unstaged = false`
since the panel's staging UI is the source of truth, `run_push`,
`generate_commit_message`). Commit is pre-disabled when nothing is staged because
`run_commit` errors on an empty index.

### View layer — new `app/src/workspace/view/source_control/`

Template: `app/src/workspace/view/conversation_list/` (UniformList virtualization, section
headers, per-item hover `ItemState`, kebab menus, keyboard navigation).

```
app/src/workspace/view/source_control/
  mod.rs
  view.rs          // SourceControlView: View + TypedActionView + Entity
  item.rs          // flat SourceControlListItem enum over all sections; collapsed sections skip children
  commit_box.rs    // EditorView-based message input + CompactibleSplitActionButton ("Commit ▾")
  header.rs        // repo/branch header, FilterableDropdown branch picker, sync widget
  dialogs.rs       // create-branch / stash-message popovers (SubmittableTextInput); add-worktree dialog
```

- Commit input follows the precedent in `app/src/code_review/git_dialog/commit.rs:100`
  (EditorView, placeholder, AI autofill that never clobbers user-typed text, min-height 72).
- Add-worktree dialog defaults the path to `<repo>-worktrees/<branch>` per
  `app/src/tab_configs/session_config.rs:118`.
- Open diff: a row click emits an event the workspace handles by opening the **code review
  right panel** for the repo (`open_code_review_panel_from_arg`) and selecting the file
  (adding a small `select_file_by_path` API to `CodeReviewView` if none is public). Accepted
  v1 limitation: staged rows show the combined working-tree diff, not a staged-only diff.
- Open worktree: existing `pane_group::Event::OpenDirectoryInNewTab` plumbing.
- UI guidelines: reuse `PrimaryTheme` / `SecondaryTheme` / `NakedTheme` unchanged
  (`app/src/view_components/action_button.rs`); all colors via `Appearance::theme()`
  accessors and `internal_colors` — no hard-coded `ColorU`. Toolbelt icon: `Icon::GitBranch`.

### Integration & rollout wiring

- Left panel: new variants in `ToolPanelView` / `LeftPanelAction`
  (`app/src/workspace/view/left_panel.rs`), toolbelt button config, render/focus/active
  arms.
- Snapshot persistence: `LeftPanelDisplayedTab::SourceControl` (`app/src/app_state.rs`)
  plus both `From` impls; verify older-build deserialization tolerance in M0.
- Actions/bindings: `WorkspaceAction::ToggleSourceControlPanel`
  (`app/src/workspace/action.rs`); binding names `workspace:left_panel_source_control` and
  `workspace:toggle_source_control` registered like the conversation-list ones; the command
  palette entry comes free via `BindingDescription`.
- Feature flag: `FeatureFlag::SourceControlPanel` (`crates/warp_features/src/lib.rs` +
  `app/src/features.rs`) gating `compute_left_panel_views` and the bindings.
- Settings: `SourceControlSettings` group (`app/src/settings/source_control.rs`,
  `define_settings_group!`): `show_source_control`, `history_commit_limit`; reuse the
  existing git-ops AI consent setting for ✨.
- Telemetry: one `SourceControlPanel { action }` event (opened, stage, unstage, discard,
  commit{mode}, sync, branch_*, stash_*, worktree_*, open_diff, ai_message).

## Milestones

- **M0 — scaffolding**: flag, settings group, toolbelt tab + bindings + snapshot variant,
  placeholder view, `opened` telemetry. Everything flag-gated.
- **M1 — read-only status**: porcelain parser + tests; model + watcher subscription +
  debounce; header (repo/branch/ahead-behind); the four change sections with status
  letters/colors; click → open file; live updates.
- **M2 — staging, commit, sync, diff**: stage/unstage/discard + section-level actions;
  operation state machine + spinners + error toasts; commit box with amend + ✨; sync
  widget (publish/push/pull); open-diff into the code review panel.
- **M3 — branches + stashes**: worktree-aware branch picker (switch / create / delete /
  cd-to-worktree); stash section (list/apply/pop/drop, stash-all, stash-staged, message
  prompt).
- **M4 — worktrees + history**: worktree section (list/add/remove/open, lazy dirty dots);
  history section with unpushed markers.
- **M5 — polish + rollout**: keyboard navigation, edge states (detached HEAD, empty repo,
  non-repo cwd, mid-merge banner), integration test, then promote the flag.

## Verification

- `cargo check -p app` per milestone.
- Unit tests: porcelain v2 fixtures (renames, conflict `u` records, `# branch.ab`,
  detached HEAD, pre-first-commit); temp-repo `git_ops` tests mirroring
  `app/src/util/git_tests.rs`; list-building tests (section collapse, ordering) following
  the conversation-list precedent.
- Integration test (`crates/integration` Builder/TestStep): open panel via action → seeded
  temp repo → assert rows → stage a file → commit → assert clean state.
- Manual: dogfood flag on; mutate the repo from a terminal pane in the same window and
  watch the panel refresh; multi-worktree repo smoke test.

## Risks

1. Snapshot forward-compat for the new `LeftPanelDisplayedTab` variant in older builds.
2. `git status` cost on huge repos → 500 ms debounce, `--no-optional-locks`,
   abort-previous-refresh, skip refresh while `index.lock` exists.
3. `CodeReviewView` file-selection API may need to be added; merge friction with active
   code-review development.
4. `git stash push --staged` requires git ≥ 2.35 → surface stderr or hide the action after
   a one-time version probe.
5. Multi-repo windows: v1 follows the active session's repo; a header repo switcher is a
   follow-up.

## Mockup

See [`mockup.html`](./mockup.html) — a standalone, dependency-free HTML mockup of the
panel docked in a Warp-like window.
