//! Integration that keeps Warp's working-directory / git-branch cards in sync
//! while Claude Code runs as the foreground process.
//!
//! While `claude` runs it blocks the outer shell, so the shell's `precmd` (which
//! normally refreshes the cards) never fires. To update mid-session we rely on a
//! small Claude Code `CwdChanged` hook ([`HOOK_SCRIPT`]) that emits an OSC 7
//! sequence to the terminal whenever Claude changes directory; Warp already
//! parses OSC 7 mid-command and re-runs git detection from there.
//!
//! This module owns three things:
//! - detecting that the running command is Claude Code ([`command_is_claude`]),
//! - checking whether the hook is installed ([`is_hook_installed`]),
//! - installing it globally into `~/.claude/` ([`install_hook`]).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// The hook script written into `~/.claude/hooks/`. Bundled at compile time.
pub const HOOK_SCRIPT: &str = include_str!("../../assets/claude/warp-emit-cwd.sh");

/// File name of the installed hook script.
const HOOK_SCRIPT_NAME: &str = "warp-emit-cwd.sh";

/// `~/.claude`.
fn claude_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude"))
}

/// `~/.claude/hooks/warp-emit-cwd.sh`.
fn hook_script_path() -> Option<PathBuf> {
    claude_dir().map(|dir| dir.join("hooks").join(HOOK_SCRIPT_NAME))
}

/// `~/.claude/settings.json`.
fn settings_path() -> Option<PathBuf> {
    claude_dir().map(|dir| dir.join("settings.json"))
}

/// Returns `true` when `command` invokes Claude Code (`claude`).
///
/// Skips leading `VAR=value` environment assignments and compares the basename
/// of the program token, so `claude`, `/opt/bin/claude`, and `FOO=1 claude -r`
/// all match while `claude-something-else` does not.
pub fn command_is_claude(command: &str) -> bool {
    for token in command.split_whitespace() {
        if is_env_assignment(token) {
            continue;
        }
        let program = token.rsplit(['/', '\\']).next().unwrap_or(token);
        return program == "claude";
    }
    false
}

/// `NAME=value` (a leading shell environment assignment).
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) if !name.is_empty() => name
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())),
        _ => false,
    }
}

/// Returns `true` when the hook is installed: the script exists on disk *and*
/// `settings.json` references it from a `CwdChanged` hook.
pub fn is_hook_installed() -> bool {
    let Some(script) = hook_script_path() else {
        return false;
    };
    if !script.exists() {
        return false;
    }
    let Some(settings) = settings_path() else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(&settings) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&contents) else {
        return false;
    };
    settings_has_cwd_hook(&value, &script.to_string_lossy())
}

/// Installs the hook globally: writes the script (executable) and merges a
/// `CwdChanged` entry into `~/.claude/settings.json`, preserving any existing
/// configuration. Idempotent.
pub fn install_hook() -> Result<()> {
    let script = hook_script_path().context("could not resolve ~/.claude/hooks path")?;
    let settings = settings_path().context("could not resolve ~/.claude/settings.json path")?;

    // Write the hook script (0755 on unix).
    if let Some(parent) = script.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating hooks dir {}", parent.display()))?;
    }
    std::fs::write(&script, HOOK_SCRIPT)
        .with_context(|| format!("writing hook script {}", script.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {}", script.display()))?;
    }

    // Merge the CwdChanged entry into settings.json, preserving the rest.
    let script_command = script.to_string_lossy().to_string();
    let mut value: Value = match std::fs::read_to_string(&settings) {
        Ok(contents) if !contents.trim().is_empty() => serde_json::from_str(&contents)
            .with_context(|| format!("parsing existing {}", settings.display()))?,
        _ => json!({}),
    };

    if merge_cwd_hook(&mut value, &script_command) {
        // Back up the previous settings before overwriting (best effort).
        if settings.exists() {
            let _ = std::fs::copy(&settings, settings.with_extension("json.warp-bak"));
        }
        if let Some(parent) = settings.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let serialized =
            serde_json::to_string_pretty(&value).context("serializing merged settings")?;
        std::fs::write(&settings, serialized + "\n")
            .with_context(|| format!("writing {}", settings.display()))?;
    }

    Ok(())
}

/// Returns `true` if a `CwdChanged` hook already references `script_command`.
fn settings_has_cwd_hook(settings: &Value, script_command: &str) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get("CwdChanged"))
        .and_then(Value::as_array)
        .is_some_and(|groups| {
            groups.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command").and_then(Value::as_str) == Some(script_command)
                        })
                    })
            })
        })
}

/// Adds a `CwdChanged` hook for `script_command` to `settings` if not already
/// present, preserving all existing keys/hooks. Returns `true` if modified.
fn merge_cwd_hook(settings: &mut Value, script_command: &str) -> bool {
    if settings_has_cwd_hook(settings, script_command) {
        return false;
    }

    // Ensure `settings` is an object.
    if !settings.is_object() {
        *settings = json!({});
    }
    let obj = settings.as_object_mut().expect("settings is an object");

    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let hooks = hooks.as_object_mut().expect("hooks is an object");

    let cwd_changed = hooks.entry("CwdChanged").or_insert_with(|| json!([]));
    if !cwd_changed.is_array() {
        *cwd_changed = json!([]);
    }
    let cwd_changed = cwd_changed.as_array_mut().expect("CwdChanged is an array");

    cwd_changed.push(json!({
        "hooks": [
            { "type": "command", "command": script_command }
        ]
    }));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_claude_command() {
        assert!(command_is_claude("claude"));
        assert!(command_is_claude("claude --resume"));
        assert!(command_is_claude("  claude   -p 'hi'"));
        assert!(command_is_claude("/usr/local/bin/claude"));
        assert!(command_is_claude("ANTHROPIC_API_KEY=x claude --foo"));
        assert!(command_is_claude("FOO=1 BAR=2 claude"));
    }

    #[test]
    fn rejects_non_claude_command() {
        assert!(!command_is_claude("clauded"));
        assert!(!command_is_claude("git claude"));
        assert!(!command_is_claude("claude-code-router"));
        assert!(!command_is_claude("npx claude"));
        assert!(!command_is_claude(""));
        assert!(!command_is_claude("vim"));
    }

    #[test]
    fn merge_into_empty_settings() {
        let mut v = json!({});
        assert!(merge_cwd_hook(
            &mut v,
            "/home/u/.claude/hooks/warp-emit-cwd.sh"
        ));
        assert!(settings_has_cwd_hook(
            &v,
            "/home/u/.claude/hooks/warp-emit-cwd.sh"
        ));
        // The new structure is well-formed.
        let cmd = v["hooks"]["CwdChanged"][0]["hooks"][0]["command"].as_str();
        assert_eq!(cmd, Some("/home/u/.claude/hooks/warp-emit-cwd.sh"));
    }

    #[test]
    fn merge_is_idempotent() {
        let mut v = json!({});
        assert!(merge_cwd_hook(&mut v, "/h/warp-emit-cwd.sh"));
        // Second merge is a no-op.
        assert!(!merge_cwd_hook(&mut v, "/h/warp-emit-cwd.sh"));
        let groups = v["hooks"]["CwdChanged"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn merge_preserves_existing_hooks() {
        // Settings with unrelated hooks and a pre-existing CwdChanged entry.
        let mut v = json!({
            "model": "opus",
            "hooks": {
                "PostToolUse": [ { "hooks": [ { "type": "command", "command": "other.sh" } ] } ],
                "CwdChanged": [ { "hooks": [ { "type": "command", "command": "user-own.sh" } ] } ]
            }
        });
        assert!(merge_cwd_hook(&mut v, "/h/warp-emit-cwd.sh"));

        // Unrelated config preserved.
        assert_eq!(v["model"], json!("opus"));
        assert_eq!(
            v["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            json!("other.sh")
        );
        // Existing CwdChanged hook preserved, ours appended.
        let groups = v["hooks"]["CwdChanged"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["hooks"][0]["command"], json!("user-own.sh"));
        assert!(settings_has_cwd_hook(&v, "/h/warp-emit-cwd.sh"));
    }

    #[test]
    fn merge_repairs_malformed_hooks_key() {
        // `hooks` present but the wrong type — we replace it rather than panic.
        let mut v = json!({ "hooks": "oops" });
        assert!(merge_cwd_hook(&mut v, "/h/warp-emit-cwd.sh"));
        assert!(settings_has_cwd_hook(&v, "/h/warp-emit-cwd.sh"));
    }
}
