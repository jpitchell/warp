//! Scans `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl` for externally-run
//! Claude Code sessions.

use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use super::index::{
    file_modified_at, normalize_title, read_head_values, ExternalAgentKind, ExternalSessionId,
    ExternalSessionIndexEntry, ExternalSessionReader,
};
use crate::ai::agent_sdk::driver::harness::claude_transcript::claude_config_dir;

/// How many head lines to scan per session looking for the first user prompt + cwd.
/// Claude transcripts front-load metadata lines (mode, permission-mode, attachments,
/// system) before the first real `user` message, so we read a generous window.
const HEAD_LINES: usize = 400;

pub struct ClaudeSessionReader;

impl ExternalSessionReader for ClaudeSessionReader {
    fn scan_index(&self) -> Vec<ExternalSessionIndexEntry> {
        let projects_dir = match claude_config_dir() {
            Ok(dir) => dir.join("projects"),
            Err(err) => {
                log::debug!("external_sessions: cannot resolve Claude config dir: {err:#}");
                return Vec::new();
            }
        };
        let Ok(project_dirs) = std::fs::read_dir(&projects_dir) else {
            return Vec::new();
        };

        let mut entries = Vec::new();
        for project in project_dirs.flatten() {
            let path = project.path();
            if !path.is_dir() {
                continue;
            }
            let Ok(session_files) = std::fs::read_dir(&path) else {
                continue;
            };
            for file in session_files.flatten() {
                let file_path = file.path();
                if let Some(entry) = scan_session_file(&file_path) {
                    entries.push(entry);
                }
            }
        }
        entries
    }
}

/// Parse a single `<uuid>.jsonl` into an index entry, or `None` if it isn't a
/// resumable session file (wrong extension, non-uuid stem, no recoverable cwd).
fn scan_session_file(path: &Path) -> Option<ExternalSessionIndexEntry> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    // The filename stem is the Claude session uuid — the value passed to
    // `claude --resume`. Anything else (subagent dirs, indexes) is skipped.
    let uuid = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| Uuid::parse_str(s).ok())?;

    let head = read_head_values(path, HEAD_LINES);

    let cwd = head.iter().find_map(value_cwd).map(PathBuf::from)?;
    let git_branch = head.iter().find_map(value_git_branch);
    let title = head
        .iter()
        .find_map(user_prompt_text)
        .and_then(|text| normalize_title(&text))
        .unwrap_or_else(|| "Claude Code session".to_owned());
    let last_modified = file_modified_at(path)?;
    let created_at = head
        .iter()
        .find_map(value_timestamp)
        .unwrap_or(last_modified);

    Some(ExternalSessionIndexEntry {
        id: ExternalSessionId {
            agent: ExternalAgentKind::ClaudeCode,
            uuid,
        },
        cwd,
        title,
        created_at,
        last_modified,
        jsonl_path: path.to_path_buf(),
        git_branch,
    })
}

fn value_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn value_git_branch(value: &Value) -> Option<String> {
    value
        .get("gitBranch")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn value_timestamp(value: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = value.get("timestamp").and_then(Value::as_str)?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Extract the prompt text from a real `user` line, skipping meta messages
/// (e.g. injected reminders) and tool-result carriers that have no text.
fn user_prompt_text(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let content = value.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                (item.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| item.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}
