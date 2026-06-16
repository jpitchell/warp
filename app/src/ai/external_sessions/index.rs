//! Lightweight index types for externally-run Claude Code / Codex sessions.
//!
//! The index is the "cheap" tier: it `stat`s each session file and reads only
//! enough of its head to recover a title, cwd, and timestamps. Full transcript
//! parsing (the "lazy" tier) lives in [`super::transcript`] and only runs when a
//! session is selected for preview.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;
use warp_cli::agent::Harness;

/// Which external CLI agent a session belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExternalAgentKind {
    ClaudeCode,
    Codex,
}

impl ExternalAgentKind {
    /// Short stable identifier used in keys and logs.
    pub fn slug(self) -> &'static str {
        match self {
            ExternalAgentKind::ClaudeCode => "claude",
            ExternalAgentKind::Codex => "codex",
        }
    }

    /// The corresponding [`Harness`], so external entries flow through the
    /// existing harness filter and harness-aware display logic for free.
    pub fn harness(self) -> Harness {
        match self {
            ExternalAgentKind::ClaudeCode => Harness::Claude,
            ExternalAgentKind::Codex => Harness::Codex,
        }
    }

    /// The executable name to look up on `PATH` and invoke.
    pub fn cli_name(self) -> &'static str {
        match self {
            ExternalAgentKind::ClaudeCode => "claude",
            ExternalAgentKind::Codex => "codex",
        }
    }

    /// Command line that resumes an existing session by its UUID.
    pub fn resume_command(self, uuid: Uuid) -> String {
        match self {
            ExternalAgentKind::ClaudeCode => format!("claude --resume {uuid}"),
            ExternalAgentKind::Codex => format!("codex resume {uuid}"),
        }
    }

    /// Command line that starts a fresh session (no resume).
    pub fn new_session_command(self) -> String {
        self.cli_name().to_string()
    }
}

/// Stable identity for an external session: the agent plus its on-disk session UUID
/// (the Claude session uuid / Codex thread uuid, which is also what the CLI's
/// resume flag takes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExternalSessionId {
    pub agent: ExternalAgentKind,
    pub uuid: Uuid,
}

impl ExternalSessionId {
    pub fn as_key(&self) -> String {
        format!("ext_{}_{}", self.agent.slug(), self.uuid)
    }
}

/// One row in the external-session index. Produced by [`ExternalSessionReader::scan_index`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalSessionIndexEntry {
    pub id: ExternalSessionId,
    /// The directory the session ran in, read from inside the JSONL (never decoded
    /// from the lossy dash-encoded directory name).
    pub cwd: PathBuf,
    /// First user prompt, used as the display title.
    pub title: String,
    /// Session start time (head timestamp), falling back to file mtime.
    pub created_at: DateTime<Utc>,
    /// File modification time — the freshness signal for active detection.
    pub last_modified: DateTime<Utc>,
    pub jsonl_path: PathBuf,
    pub git_branch: Option<String>,
}

/// Reads a single external agent's on-disk sessions.
pub trait ExternalSessionReader: Send + Sync {
    /// Cheap scan: walk the agent's sessions directory, `stat` for mtime, and read
    /// only the head of each file. Never parses full transcripts. A single
    /// unreadable/malformed session is skipped, never fatal.
    fn scan_index(&self) -> Vec<ExternalSessionIndexEntry>;
}

/// Read a file's modification time as a UTC timestamp.
pub(super) fn file_modified_at(path: &Path) -> Option<DateTime<Utc>> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(system_time_to_utc(modified))
}

pub(super) fn system_time_to_utc(time: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(time)
}

/// Read up to `max_lines` non-blank lines from the head of a JSONL file, parsing
/// each as a [`Value`]. Malformed lines are skipped. Used by readers to recover a
/// title/cwd without loading a potentially huge transcript.
pub(super) fn read_head_values(path: &Path, max_lines: usize) -> Vec<Value> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut values = Vec::new();
    for line in reader.lines().take(max_lines) {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            values.push(value);
        }
    }
    values
}

/// Read every non-blank line of a JSONL file as a [`Value`], skipping malformed
/// lines. Used by the lazy transcript tier (preview) where the full body is needed.
pub(super) fn read_all_values(path: &Path) -> Vec<Value> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut values = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            values.push(value);
        }
    }
    values
}

/// Collapse whitespace and truncate a candidate title to a sane length for list display.
pub(super) fn normalize_title(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    const MAX: usize = 120;
    if trimmed.chars().count() > MAX {
        Some(trimmed.chars().take(MAX).collect::<String>() + "…")
    } else {
        Some(trimmed.to_owned())
    }
}
