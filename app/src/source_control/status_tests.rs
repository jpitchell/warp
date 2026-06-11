use std::path::PathBuf;

use super::*;
use crate::code_review::diff_state::GitFileStatus;

/// Builds a NUL-terminated porcelain-v2 stream from logical records, mirroring
/// the `-z` output format git produces (every record ends with a NUL).
fn z(records: &[&str]) -> String {
    let mut s = String::new();
    for r in records {
        s.push_str(r);
        s.push('\0');
    }
    s
}

#[test]
fn parses_branch_headers_with_ahead_behind() {
    let out = z(&[
        "# branch.oid 1111111111111111111111111111111111111111",
        "# branch.head main",
        "# branch.upstream origin/main",
        "# branch.ab +2 -1",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.branch.head, "main");
    assert_eq!(status.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!(status.branch.ahead, 2);
    assert_eq!(status.branch.behind, 1);
    assert!(!status.branch.detached);
}

#[test]
fn parses_detached_head() {
    let out = z(&[
        "# branch.oid 2222222222222222222222222222222222222222",
        "# branch.head (detached)",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.branch.head, "(detached)");
    assert!(status.branch.detached);
    assert_eq!(status.branch.upstream, None);
}

#[test]
fn parses_pre_first_commit_no_oid() {
    // Before the first commit there is no oid and no upstream/ab headers.
    let out = z(&[
        "# branch.oid (initial)",
        "# branch.head main",
        "1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 1111111111111111111111111111111111111111 new.txt",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.branch.head, "main");
    assert_eq!(status.branch.ahead, 0);
    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.staged[0].path, "new.txt");
    assert_eq!(status.staged[0].status, GitFileStatus::New);
}

#[test]
fn parses_untracked() {
    let out = z(&[
        "# branch.head main",
        "? untracked.txt",
        "? dir/other file.txt",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.untracked.len(), 2);
    assert_eq!(status.untracked[0].path, "untracked.txt");
    assert_eq!(status.untracked[1].path, "dir/other file.txt");
    assert_eq!(status.untracked[0].status, GitFileStatus::Untracked);
}

#[test]
fn staged_and_unstaged_same_file() {
    // `1 MM ...` => modified in both index and worktree.
    let out = z(&[
        "# branch.head main",
        "1 MM N... 100644 100644 100644 aaaa bbbb file.rs",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.staged[0].path, "file.rs");
    assert_eq!(status.unstaged[0].path, "file.rs");
    assert_eq!(status.staged[0].status, GitFileStatus::Modified);
    assert_eq!(status.unstaged[0].status, GitFileStatus::Modified);
}

#[test]
fn staged_only_and_unstaged_only() {
    let out = z(&[
        "# branch.head main",
        "1 M. N... 100644 100644 100644 aaaa bbbb staged.rs",
        "1 .M N... 100644 100644 100644 aaaa bbbb unstaged.rs",
        "1 D. N... 100644 000000 000000 aaaa bbbb removed.rs",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.staged.len(), 2);
    assert_eq!(status.unstaged.len(), 1);
    assert_eq!(status.unstaged[0].path, "unstaged.rs");
    assert_eq!(status.staged[1].status, GitFileStatus::Deleted);
}

#[test]
fn parses_rename_record_with_orig_path() {
    // `2` record's orig path is the following NUL-terminated field.
    let out = z(&[
        "# branch.head main",
        "2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.rs",
        "old name.rs",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.staged.len(), 1);
    let change = &status.staged[0];
    assert_eq!(change.path, "new name.rs");
    assert_eq!(change.orig_path.as_deref(), Some("old name.rs"));
    assert_eq!(
        change.status,
        GitFileStatus::Renamed {
            old_path: "old name.rs".to_string()
        }
    );
}

#[test]
fn parses_conflict_unmerged_record() {
    let out = z(&[
        "# branch.head main",
        "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflicted.rs",
    ]);
    let status = parse_porcelain_v2(&out);
    assert_eq!(status.conflicted.len(), 1);
    assert_eq!(status.conflicted[0].path, "conflicted.rs");
    assert_eq!(status.conflicted[0].status, GitFileStatus::Conflicted);
    assert!(status.staged.is_empty());
    assert!(status.unstaged.is_empty());
}

#[test]
fn worktree_list_marks_main_and_current() {
    let out = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo-wt/feature
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feature
locked

worktree /repo-wt/detached
HEAD 3333333333333333333333333333333333333333
detached
";
    let current = PathBuf::from("/repo-wt/feature");
    let entries = parse_worktree_list(out, Some(&current));
    assert_eq!(entries.len(), 3);

    assert_eq!(entries[0].path, PathBuf::from("/repo"));
    assert!(entries[0].is_main);
    assert!(!entries[0].is_current);
    assert_eq!(entries[0].branch.as_deref(), Some("main"));

    assert_eq!(entries[1].branch.as_deref(), Some("feature"));
    assert!(entries[1].is_current);
    assert!(entries[1].locked);
    assert!(!entries[1].is_main);

    assert_eq!(entries[2].branch, None);
    assert!(!entries[2].locked);
}

#[test]
fn stash_list_parses_branch_and_age() {
    let sep = STASH_FIELD_SEP;
    let out = format!(
        "stash@{{0}}{sep}WIP on main: 1a2b3c the subject{sep}refs/stash@{{0}}{sep}2 hours ago\n\
         stash@{{1}}{sep}On feature: custom message{sep}refs/stash@{{1}}{sep}3 days ago\n\
         stash@{{2}}{sep}plain message{sep}refs/stash@{{2}}{sep}1 minute ago\n"
    );
    let entries = parse_stash_list(&out);
    assert_eq!(entries.len(), 3);

    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[0].branch.as_deref(), Some("main"));
    assert_eq!(entries[0].message, "1a2b3c the subject");
    assert_eq!(entries[0].age.as_deref(), Some("2 hours ago"));

    assert_eq!(entries[1].branch.as_deref(), Some("feature"));
    assert_eq!(entries[1].message, "custom message");

    assert_eq!(entries[2].branch, None);
    assert_eq!(entries[2].message, "plain message");
}

#[test]
fn commit_log_parses_records() {
    let sep = LOG_FIELD_SEP;
    let out = format!(
        "1111111111111111111111111111111111111111{sep}1111111{sep}First subject{sep}Alice{sep}2 hours ago\n\
         2222222222222222222222222222222222222222{sep}2222222{sep}Second subject{sep}Bob{sep}yesterday\n"
    );
    let commits = parse_commit_log(&out);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].short_sha, "1111111");
    assert_eq!(commits[0].subject, "First subject");
    assert_eq!(commits[0].author, "Alice");
    assert_eq!(commits[0].relative_time, "2 hours ago");
    assert!(!commits[0].is_unpushed);
    assert_eq!(commits[1].author, "Bob");
}
