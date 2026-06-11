use std::collections::HashSet;

use super::*;
use crate::code_review::diff_state::GitFileStatus;

fn change(path: &str) -> FileChange {
    FileChange {
        path: path.to_string(),
        orig_path: None,
        status: GitFileStatus::Modified,
    }
}

fn status_with(
    conflicted: Vec<FileChange>,
    staged: Vec<FileChange>,
    unstaged: Vec<FileChange>,
    untracked: Vec<FileChange>,
) -> RepoStatus {
    RepoStatus {
        branch: Default::default(),
        staged,
        unstaged,
        untracked,
        conflicted,
    }
}

fn stash(index: usize) -> StashEntry {
    StashEntry {
        index,
        message: format!("stash {index}"),
        branch: Some("main".to_string()),
        age: None,
    }
}

fn worktree(name: &str, is_current: bool) -> WorktreeEntry {
    WorktreeEntry {
        path: std::path::PathBuf::from(format!("/repos/{name}")),
        branch: Some(name.to_string()),
        head: "abcdef1234567".to_string(),
        is_current,
        is_main: is_current,
        locked: false,
    }
}

fn commit(sha: &str) -> CommitEntry {
    CommitEntry {
        sha: sha.to_string(),
        short_sha: sha[..7.min(sha.len())].to_string(),
        subject: "subject".to_string(),
        author: "author".to_string(),
        relative_time: "1h ago".to_string(),
        is_unpushed: false,
    }
}

fn header_sections(items: &[SourceControlListItem]) -> Vec<Section> {
    items
        .iter()
        .filter_map(|item| match item {
            SourceControlListItem::SectionHeader { section, .. } => Some(*section),
            _ => None,
        })
        .collect()
}

#[test]
fn sections_are_ordered_and_conflicts_hidden_when_empty() {
    let status = status_with(vec![], vec![change("a.rs")], vec![change("b.rs")], vec![]);
    let items = build_list_items(
        Some(&status),
        &[stash(0)],
        &[worktree("main", true)],
        &[],
        &HashSet::new(),
    );

    assert_eq!(
        header_sections(&items),
        vec![
            Section::Staged,
            Section::Changes,
            Section::Untracked,
            Section::Stashes,
            Section::Worktrees,
            Section::Commits,
        ],
        "Conflicts header must be omitted when there are no conflicted files"
    );
}

#[test]
fn conflicts_section_appears_first_when_non_empty() {
    let status = status_with(vec![change("conflict.rs")], vec![], vec![], vec![]);
    let items = build_list_items(Some(&status), &[], &[], &[], &HashSet::new());

    assert_eq!(header_sections(&items)[0], Section::Conflicts);
    assert!(matches!(
        &items[1],
        SourceControlListItem::File {
            section: Section::Conflicts,
            change,
        } if change.path == "conflict.rs"
    ));
}

#[test]
fn collapsed_sections_skip_children_but_keep_headers() {
    let status = status_with(
        vec![],
        vec![change("staged.rs")],
        vec![change("unstaged.rs")],
        vec![change("untracked.rs")],
    );
    let mut collapsed = HashSet::new();
    collapsed.insert(Section::Staged);
    collapsed.insert(Section::Stashes);

    let items = build_list_items(
        Some(&status),
        &[stash(0), stash(1)],
        &[worktree("main", true)],
        &[],
        &collapsed,
    );

    // Headers survive collapsing.
    assert!(header_sections(&items).contains(&Section::Staged));
    assert!(header_sections(&items).contains(&Section::Stashes));

    // ...but their children don't.
    assert!(!items.iter().any(|item| matches!(
        item,
        SourceControlListItem::File {
            section: Section::Staged,
            ..
        }
    )));
    assert!(!items
        .iter()
        .any(|item| matches!(item, SourceControlListItem::Stash(_))));

    // Non-collapsed sections still contribute children.
    assert!(items.iter().any(|item| matches!(
        item,
        SourceControlListItem::File {
            section: Section::Changes,
            ..
        }
    )));
}

#[test]
fn commits_collapsed_by_default_renders_no_commit_rows() {
    // The view seeds the collapse set with `Section::Commits`.
    let mut collapsed = HashSet::new();
    collapsed.insert(Section::Commits);

    let items = build_list_items(
        None,
        &[],
        &[],
        &[commit("abcdef1234567890"), commit("1234567890abcdef")],
        &collapsed,
    );

    assert!(header_sections(&items).contains(&Section::Commits));
    assert!(!items
        .iter()
        .any(|item| matches!(item, SourceControlListItem::Commit(_))));

    // Expanding renders the commit rows.
    let expanded = build_list_items(
        None,
        &[],
        &[],
        &[commit("abcdef1234567890")],
        &HashSet::new(),
    );
    assert!(expanded
        .iter()
        .any(|item| matches!(item, SourceControlListItem::Commit(_))));
}

#[test]
fn empty_stashes_show_hint_and_loading_commits_hint() {
    let items = build_list_items(None, &[], &[], &[], &HashSet::new());
    assert!(items.iter().any(|item| matches!(
        item,
        SourceControlListItem::EmptyHint {
            section: Section::Stashes,
            ..
        }
    )));
    assert!(items.iter().any(|item| matches!(
        item,
        SourceControlListItem::EmptyHint {
            section: Section::Commits,
            ..
        }
    )));
}

#[test]
fn files_with_both_staged_and_unstaged_changes_appear_in_both_sections() {
    let status = status_with(
        vec![],
        vec![change("both.rs")],
        vec![change("both.rs")],
        vec![],
    );
    let items = build_list_items(Some(&status), &[], &[], &[], &HashSet::new());

    let file_sections: Vec<Section> = items
        .iter()
        .filter_map(|item| match item {
            SourceControlListItem::File { section, change } if change.path == "both.rs" => {
                Some(*section)
            }
            _ => None,
        })
        .collect();
    assert_eq!(file_sections, vec![Section::Staged, Section::Changes]);

    // State keys disambiguate the two rows for hover-state bookkeeping.
    let keys: HashSet<String> = items.iter().map(|item| item.state_key()).collect();
    assert_eq!(keys.len(), items.len());
}
