use std::path::PathBuf;

use crate::code_review::diff_state::GitFileStatus;

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;

/// Branch / upstream state parsed from the `# branch.*` headers of
/// `git status --porcelain=v2 --branch`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BranchStatus {
    /// The current branch name, or the literal `(detached)` placeholder git
    /// emits for `# branch.head` when HEAD is detached.
    pub head: String,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
}

/// A single changed file. For renames/copies `orig_path` carries the source
/// path; for everything else it is `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: GitFileStatus,
}

/// The parsed working-tree status. A file with both staged and unstaged
/// changes (XY codes like `MM`) appears in both `staged` and `unstaged`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoStatus {
    pub branch: BranchStatus,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<FileChange>,
    pub conflicted: Vec<FileChange>,
}

/// A single `git stash list` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
    pub branch: Option<String>,
    pub age: Option<String>,
}

/// A single `git worktree list --porcelain` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub is_current: bool,
    pub is_main: bool,
    pub locked: bool,
}

/// A single commit from `git log`.
#[derive(Clone, Debug, PartialEq)]
pub struct CommitEntry {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    pub relative_time: String,
    pub is_unpushed: bool,
}

/// Maps a porcelain-v2 single-character `XY` status code to a [`GitFileStatus`].
/// `orig_path` is supplied for rename/copy records so the variant can carry it.
fn file_status_from_code(code: char, orig_path: Option<&str>) -> GitFileStatus {
    match code {
        'A' => GitFileStatus::New,
        'D' => GitFileStatus::Deleted,
        'R' => GitFileStatus::Renamed {
            old_path: orig_path.unwrap_or_default().to_string(),
        },
        'C' => GitFileStatus::Copied {
            old_path: orig_path.unwrap_or_default().to_string(),
        },
        // 'M', 'T' (typechange), and anything else fall back to Modified.
        _ => GitFileStatus::Modified,
    }
}

/// Parses `git status --porcelain=v2 --branch -z` output.
///
/// The `-z` form terminates every record with a NUL byte and disables C-style
/// path quoting, so paths with spaces / non-ASCII round-trip intact. The only
/// wrinkle is rename/copy (`2`) records: their original path is a *separate*
/// NUL-terminated field that follows the main record.
pub fn parse_porcelain_v2(output: &str) -> RepoStatus {
    let mut status = RepoStatus::default();

    // Split on NUL; renames consume the following field as their orig path.
    let mut fields = output.split('\0');
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }

        if let Some(header) = record.strip_prefix("# ") {
            parse_branch_header(header, &mut status.branch);
            continue;
        }

        // The record kind is the first whitespace-delimited token.
        let kind = record.as_bytes()[0] as char;
        match kind {
            '1' => parse_ordinary_record(record, &mut status),
            '2' => {
                // Rename/copy: orig path is the next NUL-separated field.
                let orig = fields.next().unwrap_or("");
                parse_rename_record(record, orig, &mut status);
            }
            'u' => parse_unmerged_record(record, &mut status),
            '?' => {
                if let Some(path) = record.strip_prefix("? ") {
                    status.untracked.push(FileChange {
                        path: path.to_string(),
                        orig_path: None,
                        status: GitFileStatus::Untracked,
                    });
                }
            }
            _ => {}
        }
    }

    status
}

fn parse_branch_header(header: &str, branch: &mut BranchStatus) {
    if let Some(head) = header.strip_prefix("branch.head ") {
        branch.head = head.to_string();
        branch.detached = head == "(detached)";
    } else if let Some(upstream) = header.strip_prefix("branch.upstream ") {
        let upstream = upstream.trim();
        if !upstream.is_empty() {
            branch.upstream = Some(upstream.to_string());
        }
    } else if let Some(ab) = header.strip_prefix("branch.ab ") {
        // Format: "+<ahead> -<behind>".
        let mut parts = ab.split_whitespace();
        if let Some(ahead) = parts.next() {
            branch.ahead = ahead.trim_start_matches('+').parse().unwrap_or(0);
        }
        if let Some(behind) = parts.next() {
            branch.behind = behind.trim_start_matches('-').parse().unwrap_or(0);
        }
    }
}

/// Splits a `1`/`2` record's leading fields off from the trailing path. The
/// ordinary (`1`) record layout is:
/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
/// and the rename (`2`) record inserts `<X><score>` before the path:
/// `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>`.
/// Returns `(xy, path)`.
fn split_record(record: &str, path_field_index: usize) -> Option<(&str, &str)> {
    let mut iter = record.splitn(path_field_index + 1, ' ');
    let _kind = iter.next()?;
    let xy = iter.next()?;
    // Skip the intermediate metadata fields.
    for _ in 0..(path_field_index - 2) {
        iter.next()?;
    }
    let path = iter.next()?;
    Some((xy, path))
}

fn parse_ordinary_record(record: &str, status: &mut RepoStatus) {
    // Path is the 9th field (index 8).
    let Some((xy, path)) = split_record(record, 8) else {
        return;
    };
    push_xy(xy, path, None, status);
}

fn parse_rename_record(record: &str, orig_path: &str, status: &mut RepoStatus) {
    // Path is the 10th field (index 9); the extra field is the rename score.
    let Some((xy, path)) = split_record(record, 9) else {
        return;
    };
    let orig = (!orig_path.is_empty()).then(|| orig_path.to_string());
    push_xy(xy, path, orig, status);
}

/// Pushes a `FileChange` into the staged and/or unstaged lists based on the
/// two-character `XY` code (`X` = staged/index, `Y` = unstaged/worktree).
fn push_xy(xy: &str, path: &str, orig_path: Option<String>, status: &mut RepoStatus) {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');

    if x != '.' {
        status.staged.push(FileChange {
            path: path.to_string(),
            orig_path: orig_path.clone(),
            status: file_status_from_code(x, orig_path.as_deref()),
        });
    }
    if y != '.' {
        status.unstaged.push(FileChange {
            path: path.to_string(),
            // The unstaged side of a rename is a plain modification of the
            // new path; the rename itself lives in the index (X) side.
            orig_path: None,
            status: file_status_from_code(y, None),
        });
    }
}

fn parse_unmerged_record(record: &str, status: &mut RepoStatus) {
    // Layout: `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>` —
    // path is the 11th field (index 10).
    let mut iter = record.splitn(11, ' ');
    iter.next(); // kind
    iter.next(); // XY
    for _ in 0..8 {
        if iter.next().is_none() {
            return;
        }
    }
    let Some(path) = iter.next() else {
        return;
    };
    status.conflicted.push(FileChange {
        path: path.to_string(),
        orig_path: None,
        status: GitFileStatus::Conflicted,
    });
}

/// Parses `git worktree list --porcelain` output. Records are separated by
/// blank lines; each begins with a `worktree <path>` line. The first record is
/// always the main worktree. `current_path`, when supplied, marks the matching
/// worktree as the active one.
pub fn parse_worktree_list(
    output: &str,
    current_path: Option<&std::path::Path>,
) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut head = String::new();
    let mut branch: Option<String> = None;
    let mut locked = false;
    let mut detached = false;

    let flush = |path: &mut Option<PathBuf>,
                 head: &mut String,
                 branch: &mut Option<String>,
                 locked: &mut bool,
                 detached: &mut bool,
                 entries: &mut Vec<WorktreeEntry>| {
        if let Some(p) = path.take() {
            let is_current = current_path.map(|c| c == p).unwrap_or(false);
            let is_main = entries.is_empty();
            entries.push(WorktreeEntry {
                path: p,
                branch: branch.take(),
                head: std::mem::take(head),
                is_current,
                is_main,
                locked: *locked,
            });
        }
        *locked = false;
        *detached = false;
    };

    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            flush(
                &mut path,
                &mut head,
                &mut branch,
                &mut locked,
                &mut detached,
                &mut entries,
            );
            continue;
        }

        if let Some(p) = line.strip_prefix("worktree ") {
            // Defensive: flush any record not terminated by a blank line.
            flush(
                &mut path,
                &mut head,
                &mut branch,
                &mut locked,
                &mut detached,
                &mut entries,
            );
            path = Some(PathBuf::from(p));
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            head = h.to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        } else if line == "detached" {
            detached = true;
        } else if line == "locked" || line.starts_with("locked ") {
            locked = true;
        }
    }
    // Final record (output may not end with a blank line).
    flush(
        &mut path,
        &mut head,
        &mut branch,
        &mut locked,
        &mut detached,
        &mut entries,
    );

    entries
}

/// Field separator used by [`parse_stash_list`]'s custom `--format`.
pub const STASH_FIELD_SEP: &str = "\u{1f}";

/// Parses `git stash list` output produced with the custom format
/// `--format=%gd<US>%gs<US>%gD<US>%cr` where `<US>` is [`STASH_FIELD_SEP`]:
/// reflog selector (`stash@{N}`), subject, full reflog name, relative time.
/// The subject's leading `WIP on <branch>: ` / `On <branch>: ` prefix yields
/// the originating branch.
pub fn parse_stash_list(output: &str) -> Vec<StashEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(STASH_FIELD_SEP);
        let selector = parts.next().unwrap_or("");
        let subject = parts.next().unwrap_or("");
        let _full = parts.next().unwrap_or("");
        let age = parts.next().map(|s| s.trim()).filter(|s| !s.is_empty());

        let index = selector
            .trim()
            .strip_prefix("stash@{")
            .and_then(|s| s.strip_suffix('}'))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(entries.len());

        let (branch, message) = parse_stash_subject(subject);

        entries.push(StashEntry {
            index,
            message: message.to_string(),
            branch,
            age: age.map(str::to_string),
        });
    }
    entries
}

/// Extracts `(branch, message)` from a stash subject. Subjects look like
/// `WIP on main: 1a2b3c subject`, `On main: custom message`, or just a custom
/// message (when stashed with an explicit `-m`).
fn parse_stash_subject(subject: &str) -> (Option<String>, &str) {
    let rest = subject
        .strip_prefix("WIP on ")
        .or_else(|| subject.strip_prefix("On "));
    let Some(rest) = rest else {
        return (None, subject);
    };
    let Some((branch, message)) = rest.split_once(": ") else {
        return (None, subject);
    };
    (Some(branch.to_string()), message)
}

/// Field separator used by `log_recent`'s custom `--format`.
pub const LOG_FIELD_SEP: &str = "\u{1f}";

/// Parses `git log` output produced with the custom format
/// `--format=%H<US>%h<US>%s<US>%an<US>%cr` (one record per line) into
/// [`CommitEntry`] values. `is_unpushed` is filled in by the caller since it
/// depends on the upstream comparison, not the log itself.
pub fn parse_commit_log(output: &str) -> Vec<CommitEntry> {
    let mut commits = Vec::new();
    for line in output.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(LOG_FIELD_SEP);
        let sha = parts.next().unwrap_or("").to_string();
        if sha.is_empty() {
            continue;
        }
        let short_sha = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        let author = parts.next().unwrap_or("").to_string();
        let relative_time = parts.next().unwrap_or("").to_string();
        commits.push(CommitEntry {
            sha,
            short_sha,
            subject,
            author,
            relative_time,
            is_unpushed: false,
        });
    }
    commits
}
