//! The "lazy" tier: full parse of an external session's JSONL into a harness-agnostic
//! [`NormalizedTranscript`] for the read-only preview panel. Never rehydrates into
//! Warp's task model.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::index::{read_all_values, ExternalAgentKind, ExternalSessionId};

/// A read-only, harness-agnostic transcript produced from a Claude or Codex session file.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedTranscript {
    pub title: String,
    pub cwd: PathBuf,
    pub messages: Vec<TranscriptMessage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptMessage {
    pub role: TranscriptRole,
    pub timestamp: Option<DateTime<Utc>>,
    pub blocks: Vec<TranscriptBlock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptBlock {
    Text(String),
    ToolUse { name: String, input: String },
    ToolResult { content: String },
}

/// Load and normalize a single session transcript from disk.
pub fn load_transcript(id: ExternalSessionId, path: &Path) -> NormalizedTranscript {
    let values = read_all_values(path);
    let messages = match id.agent {
        ExternalAgentKind::ClaudeCode => parse_claude(&values),
        ExternalAgentKind::Codex => parse_codex(&values),
    };
    let cwd = values
        .iter()
        .find_map(claude_or_codex_cwd)
        .unwrap_or_default();
    let title = messages
        .iter()
        .find(|m| m.role == TranscriptRole::User)
        .and_then(|m| m.blocks.iter().find_map(block_text))
        .map(|t| t.lines().next().unwrap_or(&t).to_owned())
        .unwrap_or_else(|| match id.agent {
            ExternalAgentKind::ClaudeCode => "Claude Code session".to_owned(),
            ExternalAgentKind::Codex => "Codex session".to_owned(),
        });
    NormalizedTranscript {
        title,
        cwd,
        messages,
    }
}

fn block_text(block: &TranscriptBlock) -> Option<String> {
    match block {
        TranscriptBlock::Text(t) => Some(t.clone()),
        _ => None,
    }
}

fn claude_or_codex_cwd(value: &Value) -> Option<PathBuf> {
    // Claude: top-level `cwd`. Codex: `payload.cwd` on the session_meta line.
    value
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("payload")
                .and_then(|p| p.get("cwd"))
                .and_then(Value::as_str)
        })
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let raw = value.get("timestamp").and_then(Value::as_str)?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

// ---- Claude ----

fn parse_claude(values: &[Value]) -> Vec<TranscriptMessage> {
    let mut messages = Vec::new();
    for value in values {
        let role = match value.get("type").and_then(Value::as_str) {
            Some("user") => TranscriptRole::User,
            Some("assistant") => TranscriptRole::Assistant,
            _ => continue,
        };
        if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(content) = value.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let blocks = claude_content_blocks(content);
        if blocks.is_empty() {
            continue;
        }
        messages.push(TranscriptMessage {
            role,
            timestamp: parse_timestamp(value),
            blocks,
        });
    }
    messages
}

fn claude_content_blocks(content: &Value) -> Vec<TranscriptBlock> {
    match content {
        Value::String(s) if !s.trim().is_empty() => vec![TranscriptBlock::Text(s.clone())],
        Value::Array(items) => items.iter().filter_map(claude_content_block).collect(),
        _ => Vec::new(),
    }
}

fn claude_content_block(item: &Value) -> Option<TranscriptBlock> {
    match item.get("type").and_then(Value::as_str) {
        Some("text") => {
            let text = item.get("text").and_then(Value::as_str)?;
            (!text.trim().is_empty()).then(|| TranscriptBlock::Text(text.to_owned()))
        }
        Some("tool_use") => {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_owned();
            let input = item.get("input").map(stringify_compact).unwrap_or_default();
            Some(TranscriptBlock::ToolUse { name, input })
        }
        Some("tool_result") => Some(TranscriptBlock::ToolResult {
            content: item
                .get("content")
                .map(stringify_content)
                .unwrap_or_default(),
        }),
        _ => None,
    }
}

// ---- Codex ----

fn parse_codex(values: &[Value]) -> Vec<TranscriptMessage> {
    let mut messages = Vec::new();
    for value in values {
        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = match payload.get("role").and_then(Value::as_str) {
                    Some("user") => TranscriptRole::User,
                    Some("assistant") => TranscriptRole::Assistant,
                    Some("system") => TranscriptRole::System,
                    _ => continue,
                };
                let text = codex_message_text(payload);
                let trimmed = text.trim();
                if trimmed.is_empty()
                    || (role == TranscriptRole::User
                        && (trimmed.starts_with("<environment_context")
                            || trimmed.starts_with("<user_instructions")))
                {
                    continue;
                }
                messages.push(TranscriptMessage {
                    role,
                    timestamp: parse_timestamp(value),
                    blocks: vec![TranscriptBlock::Text(trimmed.to_owned())],
                });
            }
            Some("function_call") => {
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                let input = payload
                    .get("arguments")
                    .map(stringify_content)
                    .unwrap_or_default();
                messages.push(TranscriptMessage {
                    role: TranscriptRole::Tool,
                    timestamp: parse_timestamp(value),
                    blocks: vec![TranscriptBlock::ToolUse { name, input }],
                });
            }
            Some("function_call_output") => {
                messages.push(TranscriptMessage {
                    role: TranscriptRole::Tool,
                    timestamp: parse_timestamp(value),
                    blocks: vec![TranscriptBlock::ToolResult {
                        content: payload
                            .get("output")
                            .map(stringify_content)
                            .unwrap_or_default(),
                    }],
                });
            }
            _ => continue,
        }
    }
    messages
}

fn codex_message_text(payload: &Value) -> String {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

// ---- shared ----

/// Render a JSON value as a compact one-line string (for tool inputs).
fn stringify_compact(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Render tool result/output content, which may be a string, an array of text
/// blocks, or arbitrary JSON.
fn stringify_content(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| stringify_compact(item))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}
