//! Scans `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl` for externally-run
//! Codex sessions.

use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use super::index::{
    file_modified_at, normalize_title, read_head_values, ExternalAgentKind, ExternalSessionId,
    ExternalSessionIndexEntry, ExternalSessionReader,
};
use crate::ai::agent_sdk::driver::harness::codex_transcript::{
    codex_sessions_root, parse_session_meta,
};

/// Codex front-loads `session_meta`, an `environment_context` user message, and a
/// `turn_context` before the first real prompt, so scan a modest window.
const HEAD_LINES: usize = 80;

pub struct CodexSessionReader;

impl ExternalSessionReader for CodexSessionReader {
    fn scan_index(&self) -> Vec<ExternalSessionIndexEntry> {
        let sessions_root = match codex_sessions_root() {
            Ok(dir) => dir,
            Err(err) => {
                log::debug!("external_sessions: cannot resolve Codex sessions root: {err:#}");
                return Vec::new();
            }
        };

        let mut entries = Vec::new();
        // Layout is sessions_root/YYYY/MM/DD/rollout-*.jsonl.
        for year in read_subdirs(&sessions_root) {
            for month in read_subdirs(&year) {
                for day in read_subdirs(&month) {
                    let Ok(files) = std::fs::read_dir(&day) else {
                        continue;
                    };
                    for file in files.flatten() {
                        let path = file.path();
                        if let Some(entry) = scan_session_file(&path) {
                            entries.push(entry);
                        }
                    }
                }
            }
        }
        entries
    }
}

fn read_subdirs(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            path.is_dir().then_some(path)
        })
        .collect()
}

fn scan_session_file(path: &Path) -> Option<ExternalSessionIndexEntry> {
    let name = path.file_name().and_then(|n| n.to_str())?;
    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
        return None;
    }

    let head = read_head_values(path, HEAD_LINES);
    let meta = parse_session_meta(head.first());
    let cwd = meta.as_ref().map(|m| m.cwd.clone())?;
    if cwd.as_os_str().is_empty() {
        return None;
    }

    // Prefer the session uuid from the meta payload; fall back to the trailing
    // `-<uuid>` in the filename.
    let uuid = head
        .first()
        .and_then(session_meta_id)
        .or_else(|| uuid_from_filename(name))?;

    let title = head
        .iter()
        .find_map(user_prompt_text)
        .and_then(|text| normalize_title(&text))
        .unwrap_or_else(|| "Codex session".to_owned());
    let last_modified = file_modified_at(path)?;
    let created_at = meta
        .as_ref()
        .and_then(|m| m.session_start_timestamp)
        .unwrap_or(last_modified);

    Some(ExternalSessionIndexEntry {
        id: ExternalSessionId {
            agent: ExternalAgentKind::Codex,
            uuid,
        },
        cwd,
        title,
        created_at,
        last_modified,
        jsonl_path: path.to_path_buf(),
        git_branch: None,
    })
}

fn session_meta_id(value: &Value) -> Option<Uuid> {
    if value.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let raw = value.get("payload")?.get("id").and_then(Value::as_str)?;
    Uuid::parse_str(raw).ok()
}

fn uuid_from_filename(name: &str) -> Option<Uuid> {
    let stem = name.strip_suffix(".jsonl")?;
    // rollout-<ts>-<uuid>: the uuid is the last 5 hyphen-separated groups.
    let parts: Vec<&str> = stem.rsplitn(6, '-').collect();
    if parts.len() < 5 {
        return None;
    }
    let candidate = format!(
        "{}-{}-{}-{}-{}",
        parts[4], parts[3], parts[2], parts[1], parts[0]
    );
    Uuid::parse_str(&candidate).ok()
}

/// First genuine user prompt from a Codex rollout, skipping the synthetic
/// environment/instructions messages Codex injects before the real prompt.
fn user_prompt_text(value: &Value) -> Option<String> {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    let text = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("<environment_context")
        || trimmed.starts_with("<user_instructions")
    {
        return None;
    }
    Some(trimmed.to_owned())
}
