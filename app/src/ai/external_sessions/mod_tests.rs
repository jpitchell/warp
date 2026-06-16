use std::path::Path;
use std::sync::Mutex;

use uuid::Uuid;

use super::claude::ClaudeSessionReader;
use super::codex::CodexSessionReader;
use super::index::{ExternalAgentKind, ExternalSessionId, ExternalSessionReader};
use super::transcript::{load_transcript, TranscriptBlock, TranscriptRole};

fn tool_use(name: &str, input: &str) -> TranscriptBlock {
    TranscriptBlock::ToolUse {
        name: name.to_string(),
        input: input.to_string(),
    }
}

/// Serializes tests that mutate process-global env vars (CLAUDE_CONFIG_DIR / CODEX_HOME).
static ENV_LOCK: Mutex<()> = Mutex::new(());

const CLAUDE_UUID: &str = "11111111-1111-4111-8111-111111111111";
const CODEX_UUID: &str = "22222222-2222-4222-8222-222222222222";

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn claude_session_jsonl(cwd: &str) -> String {
    [
        r#"{"type":"mode","mode":"normal"}"#.to_string(),
        format!(
            r#"{{"type":"user","isMeta":true,"cwd":"{cwd}","message":{{"role":"user","content":"injected reminder"}}}}"#
        ),
        format!(
            r#"{{"type":"user","cwd":"{cwd}","gitBranch":"main","timestamp":"2026-06-15T10:00:00.000Z","message":{{"role":"user","content":"Fix the parser bug"}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","cwd":"{cwd}","message":{{"role":"assistant","content":[{{"type":"text","text":"On it."}},{{"type":"tool_use","name":"Edit","input":{{"file":"a.rs"}}}}]}}}}"#
        ),
        "not valid json".to_string(),
    ]
    .join("\n")
}

fn codex_session_jsonl(cwd: &str) -> String {
    [
        format!(
            r#"{{"timestamp":"2026-06-15T10:00:00.000Z","type":"session_meta","payload":{{"id":"{CODEX_UUID}","timestamp":"2026-06-15T10:00:00.000Z","cwd":"{cwd}","cli_version":"0.45.0"}}}}"#
        ),
        r#"{"timestamp":"2026-06-15T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context><cwd>/x</cwd></environment_context>"}]}}"#.to_string(),
        r#"{"timestamp":"2026-06-15T10:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello codex"}]}}"#.to_string(),
        r#"{"timestamp":"2026-06-15T10:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi there"}]}}"#.to_string(),
    ]
    .join("\n")
}

#[test]
fn scans_claude_sessions_reading_cwd_from_jsonl() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path());

    // The encoded dir name is intentionally lossy; cwd must come from the JSONL body.
    let cwd = "/Users/dev/proj";
    let session = tmp
        .path()
        .join("projects")
        .join("-Users-dev-proj")
        .join(format!("{CLAUDE_UUID}.jsonl"));
    write(&session, &claude_session_jsonl(cwd));

    let entries = ClaudeSessionReader.scan_index();
    std::env::remove_var("CLAUDE_CONFIG_DIR");

    let entry = entries
        .iter()
        .find(|e| e.id.uuid == Uuid::parse_str(CLAUDE_UUID).unwrap())
        .expect("claude session indexed");
    assert_eq!(entry.id.agent, ExternalAgentKind::ClaudeCode);
    assert_eq!(entry.cwd, Path::new(cwd));
    assert_eq!(entry.title, "Fix the parser bug");
    assert_eq!(entry.git_branch.as_deref(), Some("main"));
}

#[test]
fn scans_codex_sessions_skipping_environment_context() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("CODEX_HOME", tmp.path());

    let cwd = "/Users/dev/codex-proj";
    let session = tmp
        .path()
        .join("sessions")
        .join("2026")
        .join("06")
        .join("15")
        .join(format!("rollout-2026-06-15T10-00-00-{CODEX_UUID}.jsonl"));
    write(&session, &codex_session_jsonl(cwd));

    let entries = CodexSessionReader.scan_index();
    std::env::remove_var("CODEX_HOME");

    let entry = entries
        .iter()
        .find(|e| e.id.uuid == Uuid::parse_str(CODEX_UUID).unwrap())
        .expect("codex session indexed");
    assert_eq!(entry.id.agent, ExternalAgentKind::Codex);
    assert_eq!(entry.cwd, Path::new(cwd));
    assert_eq!(entry.title, "hello codex");
}

#[test]
fn loads_claude_transcript_with_text_and_tool_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(format!("{CLAUDE_UUID}.jsonl"));
    write(&path, &claude_session_jsonl("/Users/dev/proj"));

    let id = ExternalSessionId {
        agent: ExternalAgentKind::ClaudeCode,
        uuid: Uuid::parse_str(CLAUDE_UUID).unwrap(),
    };
    let transcript = load_transcript(id, &path);

    // The meta user line is skipped; the first real user prompt drives the title.
    assert_eq!(transcript.title, "Fix the parser bug");
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[0].role, TranscriptRole::User);
    assert_eq!(transcript.messages[1].role, TranscriptRole::Assistant);
    assert!(matches!(
        transcript.messages[1].blocks[1],
        TranscriptBlock::ToolUse { .. }
    ));
}

#[test]
fn claude_tool_result_user_turn_is_labeled_as_tool() {
    // Claude nests `tool_result` inside a `user` message; it should be tagged as
    // a tool turn, not a human ("You") turn.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(format!("{CLAUDE_UUID}.jsonl"));
    let jsonl = [
        format!(
            r#"{{"type":"user","cwd":"/p","message":{{"role":"user","content":"Do the thing"}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","cwd":"/p","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Edit","input":{{"file_path":"/p/a.rs"}}}}]}}}}"#
        ),
        format!(
            r#"{{"type":"user","cwd":"/p","message":{{"role":"user","content":[{{"type":"tool_result","content":"done"}}]}}}}"#
        ),
    ]
    .join("\n");
    write(&path, &jsonl);

    let id = ExternalSessionId {
        agent: ExternalAgentKind::ClaudeCode,
        uuid: Uuid::parse_str(CLAUDE_UUID).unwrap(),
    };
    let transcript = load_transcript(id, &path);

    let roles: Vec<_> = transcript.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![
            TranscriptRole::User,
            TranscriptRole::Assistant,
            TranscriptRole::Tool,
        ]
    );
    // The real user prompt still drives the title.
    assert_eq!(transcript.title, "Do the thing");
}

#[test]
fn loads_codex_transcript_skipping_synthetic_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp
        .path()
        .join(format!("rollout-2026-06-15T10-00-00-{CODEX_UUID}.jsonl"));
    write(&path, &codex_session_jsonl("/Users/dev/codex-proj"));

    let id = ExternalSessionId {
        agent: ExternalAgentKind::Codex,
        uuid: Uuid::parse_str(CODEX_UUID).unwrap(),
    };
    let transcript = load_transcript(id, &path);

    assert_eq!(transcript.title, "hello codex");
    let roles: Vec<_> = transcript.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![TranscriptRole::User, TranscriptRole::Assistant]);
}

#[test]
fn tool_use_detail_prefers_salient_argument() {
    // Bash surfaces the command; Edit/Read surface the file path.
    assert_eq!(
        tool_use("Bash", r#"{"command":"cargo build","timeout":120}"#).tool_use_detail(),
        Some("cargo build".to_string())
    );
    assert_eq!(
        tool_use("Edit", r#"{"file_path":"/a/b.rs","old_string":"x"}"#).tool_use_detail(),
        Some("/a/b.rs".to_string())
    );
}

#[test]
fn tool_use_detail_collapses_whitespace_and_truncates() {
    let detail = tool_use("Bash", r#"{"command":"echo   a\n  b"}"#)
        .tool_use_detail()
        .unwrap();
    assert_eq!(detail, "echo a b");

    let long = "x".repeat(500);
    let input = format!(r#"{{"command":"{long}"}}"#);
    let detail = tool_use("Bash", &input).tool_use_detail().unwrap();
    assert!(
        detail.chars().count() <= 161,
        "got {} chars",
        detail.chars().count()
    );
    assert!(detail.ends_with('…'));
}

#[test]
fn tool_use_detail_falls_back_to_first_scalar() {
    // Unknown tool with no recognized key still shows its first string value.
    assert_eq!(
        tool_use("Custom", r#"{"thing":"value here"}"#).tool_use_detail(),
        Some("value here".to_string())
    );
}

#[test]
fn tool_use_detail_handles_unparseable_input() {
    assert_eq!(tool_use("Bash", "not json").tool_use_detail(), None);
    assert_eq!(tool_use("Bash", "{}").tool_use_detail(), None);
}

#[test]
fn builds_cli_commands_for_each_agent() {
    let uuid = Uuid::parse_str(CLAUDE_UUID).unwrap();

    assert_eq!(ExternalAgentKind::ClaudeCode.cli_name(), "claude");
    assert_eq!(ExternalAgentKind::Codex.cli_name(), "codex");

    assert_eq!(
        ExternalAgentKind::ClaudeCode.resume_command(uuid),
        format!("claude --resume {uuid}")
    );
    assert_eq!(
        ExternalAgentKind::Codex.resume_command(uuid),
        format!("codex resume {uuid}")
    );

    // A fresh session is just the bare CLI, no resume flag.
    assert_eq!(
        ExternalAgentKind::ClaudeCode.new_session_command(),
        "claude"
    );
    assert_eq!(ExternalAgentKind::Codex.new_session_command(), "codex");
}
