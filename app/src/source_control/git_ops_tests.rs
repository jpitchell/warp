use std::path::{Path, PathBuf};

use command::r#async::Command;
use command::Stdio;
use tempfile::TempDir;

use super::*;
use crate::code_review::diff_state::GitFileStatus;
use crate::source_control::status::parse_porcelain_v2;

/// Helper: run a git command inside the given repo directory.
async fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .expect("failed to run git");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Writes a file relative to `repo`.
fn write(repo: &Path, rel: &str, contents: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Creates a temp git repo with one commit and returns `(dir_handle, repo_path)`.
async fn init_repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let path = dir.path().to_path_buf();

    git(&path, &["init", "-b", "main"]).await;
    git(&path, &["config", "user.email", "test@test.com"]).await;
    git(&path, &["config", "user.name", "Test"]).await;
    // Disable commit signing so these tests don't depend on a configured
    // signing key / gpg in the environment.
    git(&path, &["config", "commit.gpgsign", "false"]).await;
    git(&path, &["config", "tag.gpgsign", "false"]).await;
    write(&path, "tracked.txt", "hello\n");
    git(&path, &["add", "tracked.txt"]).await;
    git(&path, &["commit", "-m", "initial"]).await;

    (dir, path)
}

/// Reads parsed status for the repo. Uses `run_git_command` directly so the
/// NUL separators in `-z` output are preserved (the `git` test helper trims).
async fn status(repo: &Path) -> crate::source_control::status::RepoStatus {
    let raw =
        warp_util::git::run_git_command(repo, &["status", "--porcelain=v2", "--branch", "-z"])
            .await
            .unwrap();
    parse_porcelain_v2(&raw)
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn stage_and_unstage_paths_roundtrip() {
    let (_dir, repo) = init_repo().await;
    write(&repo, "tracked.txt", "changed\n");

    let before = status(&repo).await;
    assert_eq!(before.unstaged.len(), 1);
    assert_eq!(before.staged.len(), 0);

    stage_paths(&repo, &["tracked.txt".to_string()])
        .await
        .unwrap();
    let staged = status(&repo).await;
    assert_eq!(staged.staged.len(), 1);
    assert_eq!(staged.staged[0].path, "tracked.txt");

    unstage_paths(&repo, &["tracked.txt".to_string()])
        .await
        .unwrap();
    let unstaged = status(&repo).await;
    assert_eq!(unstaged.staged.len(), 0);
    assert_eq!(unstaged.unstaged.len(), 1);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn stage_all_and_unstage_all() {
    let (_dir, repo) = init_repo().await;
    write(&repo, "tracked.txt", "changed\n");
    write(&repo, "new.txt", "new\n");

    stage_all(&repo).await.unwrap();
    let staged = status(&repo).await;
    assert_eq!(staged.staged.len(), 2);
    assert_eq!(staged.untracked.len(), 0);

    unstage_all(&repo).await.unwrap();
    let unstaged = status(&repo).await;
    assert_eq!(unstaged.staged.len(), 0);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn discard_untracked_removes_file() {
    let (_dir, repo) = init_repo().await;
    write(&repo, "junk.txt", "junk\n");
    assert!(repo.join("junk.txt").exists());

    discard_untracked(&repo, &["junk.txt".to_string()])
        .await
        .unwrap();
    assert!(!repo.join("junk.txt").exists());
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn discard_untracked_removes_directory() {
    let (_dir, repo) = init_repo().await;
    // A fully-untracked directory is reported by `git status` as a single
    // `dir/` entry; discarding it must remove the directory (`clean -d`).
    write(&repo, "newdir/junk.txt", "junk\n");
    let st = status(&repo).await;
    assert_eq!(st.untracked.len(), 1);
    assert_eq!(st.untracked[0].path, "newdir/");

    discard_untracked(&repo, &["newdir/".to_string()])
        .await
        .unwrap();
    assert!(!repo.join("newdir").exists());
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn git_restore_and_clean_reverts_changes() {
    let (_dir, repo) = init_repo().await;
    write(&repo, "tracked.txt", "modified\n");

    git_restore_and_clean(&repo, &["tracked.txt".to_string()], "HEAD")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "hello\n"
    );
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn create_switch_and_delete_branch() {
    let (_dir, repo) = init_repo().await;

    create_branch(&repo, "feature", None).await.unwrap();
    let st = status(&repo).await;
    assert_eq!(st.branch.head, "feature");

    switch_branch(&repo, "main").await.unwrap();
    let st = status(&repo).await;
    assert_eq!(st.branch.head, "main");

    delete_branch(&repo, "feature", false).await.unwrap();
    let branches = git(&repo, &["branch", "--format=%(refname:short)"]).await;
    assert!(!branches.contains("feature"));
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn stash_push_list_and_pop() {
    let (_dir, repo) = init_repo().await;
    write(&repo, "tracked.txt", "stashed change\n");

    stash_push(&repo, Some("my stash"), false, false)
        .await
        .unwrap();
    // Working tree should be clean after stashing.
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "hello\n"
    );

    let stashes = stash_list(&repo).await.unwrap();
    assert_eq!(stashes.len(), 1);
    assert_eq!(stashes[0].index, 0);
    assert_eq!(stashes[0].message, "my stash");
    assert_eq!(stashes[0].branch.as_deref(), Some("main"));

    stash_pop(&repo, 0).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
        "stashed change\n"
    );
    assert_eq!(stash_list(&repo).await.unwrap().len(), 0);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn worktree_add_list_and_remove() {
    let (dir, repo) = init_repo().await;
    let wt_path = dir.path().join("wt-feature");

    worktree_add(
        &repo,
        &wt_path,
        WorktreeBranch::New {
            name: "feature".to_string(),
            base: None,
        },
    )
    .await
    .unwrap();

    let worktrees = worktree_list(&repo, Some(&repo)).await.unwrap();
    assert_eq!(worktrees.len(), 2);
    assert!(worktrees[0].is_main);
    assert!(worktrees[0].is_current);
    let feature = worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("feature"))
        .expect("feature worktree present");
    assert!(!feature.is_main);

    worktree_remove(&repo, &wt_path, true).await.unwrap();
    let worktrees = worktree_list(&repo, None).await.unwrap();
    assert_eq!(worktrees.len(), 1);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn log_recent_returns_commits() {
    let (_dir, repo) = init_repo().await;
    write(&repo, "tracked.txt", "second\n");
    git(&repo, &["commit", "-am", "second commit"]).await;

    let commits = log_recent(&repo, 10).await.unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "second commit");
    assert_eq!(commits[1].subject, "initial");
    // No upstream configured, so both commits are unpushed.
    assert!(commits[0].is_unpushed);
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn run_commit_amend_replaces_message() {
    let (_dir, repo) = init_repo().await;
    run_commit_amend(&repo, Some("amended message"))
        .await
        .unwrap();
    let subject = git(&repo, &["log", "-1", "--format=%s"]).await;
    assert_eq!(subject, "amended message");
}

#[cfg(feature = "local_fs")]
#[tokio::test]
async fn conflict_produces_unmerged_record() {
    let (_dir, repo) = init_repo().await;
    // Create a conflicting change on two branches.
    write(&repo, "tracked.txt", "main change\n");
    git(&repo, &["commit", "-am", "main change"]).await;

    git(&repo, &["checkout", "-b", "other", "HEAD~1"]).await;
    write(&repo, "tracked.txt", "other change\n");
    git(&repo, &["commit", "-am", "other change"]).await;

    git(&repo, &["checkout", "main"]).await;
    // Merge will conflict; ignore the failure status.
    let _ = git(&repo, &["merge", "other"]).await;

    let st = status(&repo).await;
    assert_eq!(st.conflicted.len(), 1);
    assert_eq!(st.conflicted[0].status, GitFileStatus::Conflicted);
}
