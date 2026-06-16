//! Tests for the fetch-related refresh behavior added to `SourceControlModel`:
//! the remote-gating used by the background fetch (`repo_has_remote`) and the
//! fetch step in `load`. Mirrors the temp-repo style of `git_ops_tests.rs`.
//!
//! These exercise real `git` subprocesses, so they're gated on `local_fs` like
//! the rest of the model; run with `cargo nextest run -p warp --features local_fs`.

use std::path::Path;

use command::r#async::Command;
use command::Stdio;

use super::*;

/// Runs a git command inside `repo`, returning trimmed stdout.
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

fn write(repo: &Path, rel: &str, contents: &str) {
    std::fs::write(repo.join(rel), contents).unwrap();
}

/// Identity + signing config so commits don't depend on the environment.
async fn configure(repo: &Path) {
    git(repo, &["config", "user.email", "test@test.com"]).await;
    git(repo, &["config", "user.name", "Test"]).await;
    git(repo, &["config", "commit.gpgsign", "false"]).await;
    git(repo, &["config", "tag.gpgsign", "false"]).await;
}

/// Initializes a fresh repo with one commit on `main`.
async fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]).await;
    configure(path).await;
    write(path, "tracked.txt", "hello\n");
    git(path, &["add", "tracked.txt"]).await;
    git(path, &["commit", "-m", "initial"]).await;
}

#[tokio::test]
async fn repo_has_remote_is_false_without_a_remote() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path()).await;

    assert!(!SourceControlModel::repo_has_remote(dir.path()).await);
}

#[tokio::test]
async fn repo_has_remote_becomes_true_after_adding_a_remote() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path()).await;

    // No remote yet: the periodic fetch should stay dormant.
    assert!(!SourceControlModel::repo_has_remote(dir.path()).await);

    // `git remote add` on a previously remote-less repo. Because the periodic
    // fetch re-checks every tick, this is the transition that flips it on.
    git(
        dir.path(),
        &["remote", "add", "origin", "https://example.com/repo.git"],
    )
    .await;

    assert!(SourceControlModel::repo_has_remote(dir.path()).await);
}

#[tokio::test]
async fn load_with_fetch_picks_up_new_remote_commits() {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin");
    let work = root.path().join("work");
    std::fs::create_dir_all(&origin).unwrap();

    init_repo(&origin).await;
    git(
        root.path(),
        &["clone", origin.to_str().unwrap(), work.to_str().unwrap()],
    )
    .await;

    // Advance origin by one commit *after* the clone, so work's remote-tracking
    // ref is stale until it fetches.
    write(&origin, "tracked.txt", "world\n");
    git(&origin, &["commit", "-am", "second"]).await;

    // Without fetching, the stale `origin/main` shows nothing behind.
    let stale = SourceControlModel::load(work.clone(), false, false, 0).await;
    assert_eq!(stale.status.expect("status loaded").branch.behind, 0);

    // Fetching first surfaces the new origin commit as "behind 1".
    let fetched = SourceControlModel::load(work.clone(), true, false, 0).await;
    assert_eq!(fetched.status.expect("status loaded").branch.behind, 1);
}
