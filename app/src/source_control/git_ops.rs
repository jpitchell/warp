//! Thin async wrappers over `warp_util::git::run_git_command` for the Source
//! Control panel: staging, discard, branches, stashes, worktrees, and history.
//!
//! Commit / push / AI commit-message generation are intentionally **not** here;
//! the panel reuses the existing primitives in `code_review::git_actions` and
//! `util::git`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
#[cfg(feature = "local_fs")]
use warp_util::git::{run_git_command, run_git_command_with_env};

use super::status::{
    parse_commit_log, parse_stash_list, parse_worktree_list, CommitEntry, StashEntry,
    WorktreeEntry, LOG_FIELD_SEP, STASH_FIELD_SEP,
};
#[cfg(feature = "local_fs")]
use crate::util::git::compute_unpushed_state;

#[cfg(all(test, feature = "local_fs"))]
#[path = "git_ops_tests.rs"]
mod tests;

/// How a worktree's branch is specified when adding a worktree.
#[derive(Clone, Debug, PartialEq)]
pub enum WorktreeBranch {
    /// Check out an existing branch into the new worktree.
    Existing(String),
    /// Create a new branch (`-b <name>`) from `base` in the new worktree.
    New { name: String, base: Option<String> },
}

// ── Discard helpers (shared with code_review::diff_state::local) ─────────────

/// Stashes uncommitted changes for specific files.
///
/// Lifted verbatim from `LocalDiffStateModel::stash_uncommitted_changes` so the
/// Source Control panel and the code-review discard path share one
/// implementation. `local.rs` delegates here.
#[cfg(feature = "local_fs")]
pub async fn stash_uncommitted_changes(repo_path: &Path, relative_paths: &[String]) -> Result<()> {
    use warp_core::channel::ChannelState;

    let app_id = ChannelState::app_id();
    let app_name = app_id.application_name();
    let msg = if relative_paths.len() == 1 {
        format!("{app_name}: stash {}", relative_paths[0])
    } else {
        format!("{app_name}: stash {} files", relative_paths.len())
    };

    let mut stash_args = vec!["stash", "push", "-u", "-m", msg.as_str(), "--"];
    for path in relative_paths {
        stash_args.push(path.as_str());
    }

    log::debug!(
        "[GIT OPERATION] git_ops.rs stash_uncommitted_changes git {}",
        stash_args.join(" ")
    );
    let stash_res = run_git_command(repo_path, &stash_args).await;

    match stash_res {
        Ok(_) => Ok(()),
        Err(err) => {
            let err_msg = err.to_string();
            // If there are no local changes to stash, git stash will fail
            // In this case, we can safely ignore the error
            if err_msg.contains("No local changes to save") {
                Ok(())
            } else {
                let context = if relative_paths.len() == 1 {
                    relative_paths[0].clone()
                } else {
                    format!("{} files", relative_paths.len())
                };
                Err(anyhow!(
                    "Failed to stash changes for {}: {}",
                    context,
                    err_msg
                ))
            }
        }
    }
}

/// Runs git restore and git clean for one or more files.
///
/// Lifted verbatim from `LocalDiffStateModel::git_restore_and_clean`; `local.rs`
/// delegates here.
#[cfg(feature = "local_fs")]
pub async fn git_restore_and_clean(
    repo_path: &Path,
    relative_paths: &[String],
    branch: &str,
) -> Result<()> {
    use std::fs;

    let source_arg = format!("--source={branch}");
    let mut restore_args = vec![
        "restore",
        "--staged",
        "--worktree",
        source_arg.as_str(),
        "--",
    ];
    for path in relative_paths {
        restore_args.push(path.as_str());
    }

    log::debug!(
        "[GIT OPERATION] git_ops.rs git_restore_and_clean git {}",
        restore_args.join(" ")
    );
    let restore_res = run_git_command(repo_path, &restore_args).await;

    match restore_res {
        Ok(_) => {
            // Clean untracked files for these specific paths
            let mut clean_args = vec!["clean", "-fd"];
            for path in relative_paths {
                clean_args.push(path.as_str());
            }
            log::debug!(
                "[GIT OPERATION] git_ops.rs git_restore_and_clean git {}",
                clean_args.join(" ")
            );
            let clean_res = run_git_command(repo_path, &clean_args).await;

            match clean_res {
                Ok(_) => Ok(()),
                Err(err) => {
                    log::warn!("Failed to clean untracked files: {err}");
                    Ok(())
                }
            }
        }
        Err(err) => {
            let err_msg = err.to_string();
            if branch == "HEAD" && err_msg.contains("could not resolve HEAD") {
                let mut clean_args = vec!["clean", "-fd"];
                for path in relative_paths {
                    clean_args.push(path.as_str());
                }
                log::debug!(
                    "[GIT OPERATION] git_ops.rs git_restore_and_clean git {}",
                    clean_args.join(" ")
                );
                let clean_res = run_git_command(repo_path, &clean_args).await;
                if let Err(err) = clean_res {
                    log::warn!("Failed to clean untracked files: {err}");
                }
                Ok(())
            } else if err_msg.contains("did not match any file(s) known to git") {
                // If some files don't exist in the branch, we need to remove them
                for file_path in relative_paths {
                    log::debug!(
                        "[GIT OPERATION] git_ops.rs git_restore_and_clean git rm -f -- {file_path}"
                    );
                    let rm_res =
                        run_git_command(repo_path, &["rm", "-f", "--", file_path.as_str()]).await;

                    if let Err(rm_err) = rm_res {
                        let rm_err_msg = rm_err.to_string();
                        if rm_err_msg.contains("did not match any files") {
                            // if the file was staged but it isn't in the working directory,
                            // e.g. it was locally deleted
                            log::debug!(
                                "[GIT OPERATION] git_ops.rs git_restore_and_clean git reset -- {file_path}"
                            );
                            if let Err(e) =
                                run_git_command(repo_path, &["reset", "--", file_path.as_str()])
                                    .await
                            {
                                log::warn!("Failed to unstage file '{file_path}': {e}");
                            }
                        } else {
                            log::warn!("Failed to remove file '{file_path}': {rm_err_msg}");
                        }
                    }

                    if let Err(e) = fs::remove_file(repo_path.join(file_path)) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            log::warn!("Failed to remove file '{file_path}' from filesystem: {e}");
                        }
                    }
                }
                Ok(())
            } else {
                Err(err)
            }
        }
    }
}

// ── Staging ─────────────────────────────────────────────────────────────────

/// Stages the given paths (`git add -- <paths>`).
#[cfg(feature = "local_fs")]
pub async fn stage_paths(repo_path: &Path, relative_paths: &[String]) -> Result<()> {
    if relative_paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["add", "--"];
    args.extend(relative_paths.iter().map(String::as_str));
    log::debug!(
        "[GIT OPERATION] git_ops.rs stage_paths git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await.map(|_| ())
}

/// Unstages the given paths (`git restore --staged -- <paths>`).
#[cfg(feature = "local_fs")]
pub async fn unstage_paths(repo_path: &Path, relative_paths: &[String]) -> Result<()> {
    if relative_paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(relative_paths.iter().map(String::as_str));
    log::debug!(
        "[GIT OPERATION] git_ops.rs unstage_paths git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await.map(|_| ())
}

/// Stages every change in the working tree (`git add -A`).
#[cfg(feature = "local_fs")]
pub async fn stage_all(repo_path: &Path) -> Result<()> {
    log::debug!("[GIT OPERATION] git_ops.rs stage_all git add -A");
    run_git_command(repo_path, &["add", "-A"]).await.map(|_| ())
}

/// Unstages everything (`git restore --staged -- .`).
#[cfg(feature = "local_fs")]
pub async fn unstage_all(repo_path: &Path) -> Result<()> {
    log::debug!("[GIT OPERATION] git_ops.rs unstage_all git restore --staged -- .");
    run_git_command(repo_path, &["restore", "--staged", "--", "."])
        .await
        .map(|_| ())
}

/// Discards untracked paths (`git clean -fd -- <paths>`). `-d` is required
/// because fully-untracked directories show up as a single `dir/` status
/// entry, and `git clean` refuses to remove directories without it.
#[cfg(feature = "local_fs")]
pub async fn discard_untracked(repo_path: &Path, relative_paths: &[String]) -> Result<()> {
    if relative_paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["clean", "-fd", "--"];
    args.extend(relative_paths.iter().map(String::as_str));
    log::debug!(
        "[GIT OPERATION] git_ops.rs discard_untracked git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await.map(|_| ())
}

// ── Branches ─────────────────────────────────────────────────────────────────

/// Switches to an existing branch (`git switch <branch>`).
#[cfg(feature = "local_fs")]
pub async fn switch_branch(repo_path: &Path, branch: &str) -> Result<()> {
    log::debug!("[GIT OPERATION] git_ops.rs switch_branch git switch {branch}");
    run_git_command(repo_path, &["switch", branch])
        .await
        .map(|_| ())
}

/// Creates and switches to a new branch (`git switch -c <name>`), optionally
/// from `base`.
#[cfg(feature = "local_fs")]
pub async fn create_branch(repo_path: &Path, name: &str, base: Option<&str>) -> Result<()> {
    let mut args = vec!["switch", "-c", name];
    if let Some(base) = base {
        args.push(base);
    }
    log::debug!(
        "[GIT OPERATION] git_ops.rs create_branch git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await.map(|_| ())
}

/// Deletes a branch (`git branch -d`, or `-D` when `force`).
#[cfg(feature = "local_fs")]
pub async fn delete_branch(repo_path: &Path, name: &str, force: bool) -> Result<()> {
    let flag = if force { "-D" } else { "-d" };
    log::debug!("[GIT OPERATION] git_ops.rs delete_branch git branch {flag} {name}");
    run_git_command(repo_path, &["branch", flag, name])
        .await
        .map(|_| ())
}

/// Pulls with fast-forward only (`git pull --ff-only`). `path_env` is forwarded
/// so hooks can find user-installed tools, mirroring `util::git::run_push`.
#[cfg(feature = "local_fs")]
pub async fn pull(repo_path: &Path, path_env: Option<&str>) -> Result<()> {
    log::debug!("[GIT OPERATION] git_ops.rs pull git pull --ff-only");
    run_git_command_with_env(repo_path, &["pull", "--ff-only"], path_env)
        .await
        .map(|_| ())
}

// ── Stashes ─────────────────────────────────────────────────────────────────

/// Lists stashes via a custom format carrying branch + relative time.
#[cfg(feature = "local_fs")]
pub async fn stash_list(repo_path: &Path) -> Result<Vec<StashEntry>> {
    let format =
        format!("--format=%gd{STASH_FIELD_SEP}%gs{STASH_FIELD_SEP}%gD{STASH_FIELD_SEP}%cr");
    log::debug!("[GIT OPERATION] git_ops.rs stash_list git stash list {format}");
    let output = run_git_command(repo_path, &["stash", "list", &format]).await?;
    Ok(parse_stash_list(&output))
}

/// Pushes a stash. `include_untracked` adds `-u`; `staged_only` adds
/// `--staged` (requires git ≥ 2.35).
#[cfg(feature = "local_fs")]
pub async fn stash_push(
    repo_path: &Path,
    message: Option<&str>,
    include_untracked: bool,
    staged_only: bool,
) -> Result<()> {
    let mut args = vec!["stash", "push"];
    if include_untracked {
        args.push("-u");
    }
    if staged_only {
        args.push("--staged");
    }
    if let Some(message) = message.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("-m");
        args.push(message);
    }
    log::debug!(
        "[GIT OPERATION] git_ops.rs stash_push git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await.map(|_| ())
}

/// Applies a stash without removing it (`git stash apply stash@{n}`).
#[cfg(feature = "local_fs")]
pub async fn stash_apply(repo_path: &Path, index: usize) -> Result<()> {
    let selector = format!("stash@{{{index}}}");
    log::debug!("[GIT OPERATION] git_ops.rs stash_apply git stash apply {selector}");
    run_git_command(repo_path, &["stash", "apply", &selector])
        .await
        .map(|_| ())
}

/// Applies and drops a stash (`git stash pop stash@{n}`).
#[cfg(feature = "local_fs")]
pub async fn stash_pop(repo_path: &Path, index: usize) -> Result<()> {
    let selector = format!("stash@{{{index}}}");
    log::debug!("[GIT OPERATION] git_ops.rs stash_pop git stash pop {selector}");
    run_git_command(repo_path, &["stash", "pop", &selector])
        .await
        .map(|_| ())
}

/// Drops a stash (`git stash drop stash@{n}`).
#[cfg(feature = "local_fs")]
pub async fn stash_drop(repo_path: &Path, index: usize) -> Result<()> {
    let selector = format!("stash@{{{index}}}");
    log::debug!("[GIT OPERATION] git_ops.rs stash_drop git stash drop {selector}");
    run_git_command(repo_path, &["stash", "drop", &selector])
        .await
        .map(|_| ())
}

// ── Worktrees ────────────────────────────────────────────────────────────────

/// Lists worktrees (`git worktree list --porcelain`). `current_path` marks the
/// active worktree in the returned entries.
#[cfg(feature = "local_fs")]
pub async fn worktree_list(
    repo_path: &Path,
    current_path: Option<&Path>,
) -> Result<Vec<WorktreeEntry>> {
    log::debug!("[GIT OPERATION] git_ops.rs worktree_list git worktree list --porcelain");
    let output = run_git_command(repo_path, &["worktree", "list", "--porcelain"]).await?;
    Ok(parse_worktree_list(&output, current_path))
}

/// Adds a worktree at `path` checking out / creating a branch per `branch`.
#[cfg(feature = "local_fs")]
pub async fn worktree_add(repo_path: &Path, path: &Path, branch: WorktreeBranch) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    let mut args = vec!["worktree".to_string(), "add".to_string()];
    match &branch {
        WorktreeBranch::Existing(name) => {
            // `--` so a user-typed path starting with `-` can't be parsed as
            // a flag.
            args.push("--".to_string());
            args.push(path_str);
            args.push(name.clone());
        }
        WorktreeBranch::New { name, base } => {
            args.push("-b".to_string());
            args.push(name.clone());
            args.push("--".to_string());
            args.push(path_str);
            if let Some(base) = base {
                args.push(base.clone());
            }
        }
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    log::debug!(
        "[GIT OPERATION] git_ops.rs worktree_add git {}",
        arg_refs.join(" ")
    );
    run_git_command(repo_path, &arg_refs).await.map(|_| ())
}

/// Removes a worktree (`git worktree remove`, adding `--force` when `force`).
#[cfg(feature = "local_fs")]
pub async fn worktree_remove(repo_path: &Path, path: &Path, force: bool) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_str);
    log::debug!(
        "[GIT OPERATION] git_ops.rs worktree_remove git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await.map(|_| ())
}

// ── History ──────────────────────────────────────────────────────────────────

/// Returns the most recent `limit` commits, with `is_unpushed` set for commits
/// not yet on the upstream (or, lacking an upstream, the unpushed set from
/// [`compute_unpushed_state`]).
#[cfg(feature = "local_fs")]
pub async fn log_recent(repo_path: &Path, limit: usize) -> Result<Vec<CommitEntry>> {
    let count = format!("-n{limit}");
    let format = format!(
        "--format=%H{LOG_FIELD_SEP}%h{LOG_FIELD_SEP}%s{LOG_FIELD_SEP}%an{LOG_FIELD_SEP}%cr"
    );
    log::debug!("[GIT OPERATION] git_ops.rs log_recent git log {count} {format}");
    let output = run_git_command(repo_path, &["log", &count, &format]).await?;
    let mut commits = parse_commit_log(&output);

    // Mark unpushed commits. `compute_unpushed_state` returns the unpushed
    // commits (vs upstream, or vs fork point when no upstream) and never errors.
    let (unpushed, _upstream) = compute_unpushed_state(repo_path).await;
    let unpushed_shas: std::collections::HashSet<String> =
        unpushed.into_iter().map(|c| c.hash).collect();
    for commit in &mut commits {
        commit.is_unpushed = unpushed_shas.contains(&commit.sha);
    }

    Ok(commits)
}

/// Amends the previous commit. `Some(message)` replaces the message
/// (`--amend -m`); `None` keeps it (`--amend --no-edit`).
#[cfg(feature = "local_fs")]
pub async fn run_commit_amend(repo_path: &Path, message: Option<&str>) -> Result<String> {
    let args: Vec<&str> = match message.map(str::trim).filter(|m| !m.is_empty()) {
        Some(message) => vec!["commit", "--amend", "-m", message],
        None => vec!["commit", "--amend", "--no-edit"],
    };
    log::debug!(
        "[GIT OPERATION] git_ops.rs run_commit_amend git {}",
        args.join(" ")
    );
    run_git_command(repo_path, &args).await
}

// ── Non-local_fs stubs ───────────────────────────────────────────────────────

#[cfg(not(feature = "local_fs"))]
macro_rules! unsupported {
    () => {
        Err(anyhow!("Not supported without local_fs"))
    };
}

#[cfg(not(feature = "local_fs"))]
pub async fn stage_paths(_repo_path: &Path, _relative_paths: &[String]) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn unstage_paths(_repo_path: &Path, _relative_paths: &[String]) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn stage_all(_repo_path: &Path) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn unstage_all(_repo_path: &Path) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn discard_untracked(_repo_path: &Path, _relative_paths: &[String]) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn switch_branch(_repo_path: &Path, _branch: &str) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn create_branch(_repo_path: &Path, _name: &str, _base: Option<&str>) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn delete_branch(_repo_path: &Path, _name: &str, _force: bool) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn pull(_repo_path: &Path, _path_env: Option<&str>) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn stash_list(_repo_path: &Path) -> Result<Vec<StashEntry>> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn stash_push(
    _repo_path: &Path,
    _message: Option<&str>,
    _include_untracked: bool,
    _staged_only: bool,
) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn stash_apply(_repo_path: &Path, _index: usize) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn stash_pop(_repo_path: &Path, _index: usize) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn stash_drop(_repo_path: &Path, _index: usize) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn worktree_list(
    _repo_path: &Path,
    _current_path: Option<&Path>,
) -> Result<Vec<WorktreeEntry>> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn worktree_add(_repo_path: &Path, _path: &Path, _branch: WorktreeBranch) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn worktree_remove(_repo_path: &Path, _path: &Path, _force: bool) -> Result<()> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn log_recent(_repo_path: &Path, _limit: usize) -> Result<Vec<CommitEntry>> {
    unsupported!()
}

#[cfg(not(feature = "local_fs"))]
pub async fn run_commit_amend(_repo_path: &Path, _message: Option<&str>) -> Result<String> {
    unsupported!()
}

/// Convenience: the default worktree path for a branch, mirroring
/// `<repo>-worktrees/<branch>` used elsewhere in the codebase.
pub fn default_worktree_path(repo_path: &Path, branch: &str) -> PathBuf {
    let dir_name = repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    let parent = repo_path.parent().unwrap_or(repo_path);
    parent
        .join(format!("{dir_name}-worktrees"))
        .join(branch.replace('/', "-"))
}
