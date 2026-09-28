//! Claude Code hook integration: `void hook install` adds Void to
//! `~/.claude/settings.json` so agents warn on low disk at session start and
//! trim a worktree's build artifacts when it is removed.
//!
//! The settings file belongs to the user and to Claude Code, not to Void, so
//! every write here is conservative: an unparseable file is never
//! overwritten, a timestamped backup is written before any change, the write
//! is atomic, and only entries Void recognises as its own are ever replaced
//! or removed. Hooks are registered as:
//!
//! ```json
//! "hooks": {
//!   "SessionStart":   [{ "hooks": [{ "type": "command", "command": "\"/path/void\" hook session-start" }] }],
//!   "WorktreeRemove": [{ "hooks": [{ "type": "command", "command": "\"/path/void\" hook worktree-remove" }] }]
//! }
//! ```
//!
//! `WorktreeCreate` is deliberately not used: a hook on it *replaces*
//! Claude Code's own worktree creation.
//!
//! Key order: `serde_json` is built without `preserve_order`, so objects are
//! re-serialized with their keys sorted. Every key and value survives; only
//! their order in the file may change.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// The hook events Void registers, with the subcommand each runs.
const OUR_HOOKS: [(&str, &str); 2] = [
    ("SessionStart", "session-start"),
    ("WorktreeRemove", "worktree-remove"),
];

/// Seconds Claude Code may wait for each hook. Session start must never slow
/// a session down; trimming a worktree may take a moment.
fn timeout_for(subcommand: &str) -> u64 {
    if subcommand == "session-start" {
        10
    } else {
        120
    }
}

/// Whether Void's hooks are present in Claude Code's settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookStatus {
    /// Both hooks are present.
    pub installed: bool,
    pub settings_path: PathBuf,
    /// The session-start command Void registered, when installed.
    pub command: Option<String>,
    /// Which of Void's hooks are present.
    #[serde(default)]
    pub session_start: bool,
    #[serde(default)]
    pub worktree_remove: bool,
    /// Set when the settings file exists but could not be read or parsed.
    #[serde(default)]
    pub error: Option<String>,
    /// The backup written by the last install/uninstall, if any.
    #[serde(default)]
    pub backup_path: Option<PathBuf>,
}

fn settings_path(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

/// Quote the binary for a shell command line. Hooks run through the shell,
/// so a path with spaces (`/Applications/Void Beta.app/...`) must be quoted,
/// and the characters a double-quoted string still interprets escaped.
fn shell_quote(path: &Path) -> String {
    let raw = path.display().to_string();
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        if matches!(ch, '"' | '\\' | '$' | '`') && !cfg!(windows) {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// The command line Void registers for `subcommand`.
pub fn hook_command(void_bin: &Path, subcommand: &str) -> String {
    format!("{} hook {subcommand}", shell_quote(void_bin))
}

/// Whether `command` is a hook Void registered for `subcommand` — by any
/// Void binary, so reinstalling from a new location replaces the old entry.
fn is_ours(command: &str, subcommand: &str) -> bool {
    let suffix = format!(" hook {subcommand}");
    let Some(bin) = command.trim_end().strip_suffix(&suffix) else {
        return false;
    };
    let bin = bin.trim().trim_matches(['"', '\'']).replace("\\\"", "\"");
    let name = Path::new(&bin)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    name == "void"
}

fn entry_is_ours(entry: &Value, subcommand: &str) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| is_ours(c, subcommand))
}

/// Read and parse the settings file. `Ok(None)` when it does not exist.
fn read_settings(path: &Path) -> Result<Option<Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(Some(Value::Object(Map::new())));
    }
    let value: Value = serde_json::from_str(&raw).map_err(|err| {
        format!(
            "{} is not valid JSON ({err}); leaving it untouched",
            path.display()
        )
    })?;
    if !value.is_object() {
        return Err(format!(
            "{} is not a JSON object; leaving it untouched",
            path.display()
        ));
    }
    Ok(Some(value))
}

/// Copy the current file aside before changing it.
fn write_backup(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ");
    let backup = path.with_file_name(format!("settings.json.void-backup-{stamp}"));
    std::fs::copy(path, &backup)
        .map_err(|err| format!("could not back up {}: {err}", path.display()))?;
    Ok(Some(backup))
}

/// Write `value` via a sibling temp file and rename, so Claude Code never
/// reads a half-written settings file.
fn write_atomic(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let mut json = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    json.push('\n');
    let tmp = path.with_file_name(format!(".settings.json.void-{}.tmp", std::process::id()));
    std::fs::write(&tmp, json)
        .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not write {}: {err}", path.display())
    })
}

/// Remove Void's entries from `settings`, dropping any group, event array or
/// `hooks` object that removal left empty. Returns whether anything changed.
fn strip_ours(settings: &mut Value) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for (event, subcommand) in OUR_HOOKS {
        let Some(groups) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut changed_here = false;
        groups.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let before = entries.len();
            entries.retain(|e| !entry_is_ours(e, subcommand));
            if entries.len() == before {
                return true;
            }
            changed_here = true;
            // A group that only held our hook was ours; drop it.
            !entries.is_empty()
        });
        // Only an array *we* emptied is removed; a user's own empty array stays.
        if changed_here && groups.is_empty() {
            hooks.remove(event);
        }
        changed |= changed_here;
    }
    if changed && hooks.is_empty() {
        if let Some(obj) = settings.as_object_mut() {
            obj.remove("hooks");
        }
    }
    changed
}

/// Add Void's groups to `settings` (which must already be stripped of them).
fn add_ours(settings: &mut Value, void_bin: &Path) -> Result<(), String> {
    let obj = settings
        .as_object_mut()
        .ok_or_else(|| "settings is not a JSON object".to_string())?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or_else(|| {
        "\"hooks\" in settings.json is not an object; leaving it untouched".to_string()
    })?;
    for (event, subcommand) in OUR_HOOKS {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let groups = groups.as_array_mut().ok_or_else(|| {
            format!("\"hooks.{event}\" in settings.json is not an array; leaving it untouched")
        })?;
        groups.push(json!({
            "hooks": [{
                "type": "command",
                "command": hook_command(void_bin, subcommand),
                "timeout": timeout_for(subcommand),
            }]
        }));
    }
    Ok(())
}

fn status_of(settings: &Value, path: PathBuf) -> HookStatus {
    let find = |event: &str, subcommand: &str| -> Option<String> {
        settings
            .get("hooks")?
            .get(event)?
            .as_array()?
            .iter()
            .filter_map(|g| g.get("hooks")?.as_array())
            .flatten()
            .filter_map(|e| e.get("command")?.as_str())
            .find(|c| is_ours(c, subcommand))
            .map(str::to_string)
    };
    let session = find("SessionStart", "session-start");
    let worktree = find("WorktreeRemove", "worktree-remove");
    HookStatus {
        installed: session.is_some() && worktree.is_some(),
        settings_path: path,
        session_start: session.is_some(),
        worktree_remove: worktree.is_some(),
        command: session,
        error: None,
        backup_path: None,
    }
}

/// Inspect `<home>/.claude/settings.json`.
pub fn status(home: &Path) -> HookStatus {
    let path = settings_path(home);
    match read_settings(&path) {
        Ok(Some(settings)) => status_of(&settings, path),
        Ok(None) => status_of(&Value::Object(Map::new()), path),
        Err(err) => HookStatus {
            error: Some(err),
            ..status_of(&Value::Object(Map::new()), path)
        },
    }
}

/// Add Void's hooks, preserving everything else in the file (a backup is
/// written first). Idempotent: reinstalling replaces Void's entries rather
/// than adding more, and an install that changes nothing writes nothing.
pub fn install(home: &Path, void_bin: &Path) -> Result<HookStatus, String> {
    let path = settings_path(home);
    let original = read_settings(&path)?;
    let mut settings = original
        .clone()
        .unwrap_or_else(|| Value::Object(Map::new()));
    strip_ours(&mut settings);
    add_ours(&mut settings, void_bin)?;

    if original.as_ref() == Some(&settings) {
        return Ok(status_of(&settings, path));
    }
    let backup = write_backup(&path)?;
    write_atomic(&path, &settings)?;
    Ok(HookStatus {
        backup_path: backup,
        ..status_of(&settings, path)
    })
}

/// Remove only the hooks Void added (a backup is written first).
pub fn uninstall(home: &Path) -> Result<HookStatus, String> {
    let path = settings_path(home);
    let Some(mut settings) = read_settings(&path)? else {
        return Ok(status_of(&Value::Object(Map::new()), path));
    };
    if !strip_ours(&mut settings) {
        return Ok(status_of(&settings, path));
    }
    let backup = write_backup(&path)?;
    write_atomic(&path, &settings)?;
    Ok(HookStatus {
        backup_path: backup,
        ..status_of(&settings, path)
    })
}

/// JSON snippet registering `void mcp` as an MCP server, for pasting into an
/// agent's MCP config.
pub fn mcp_config_snippet(void_bin: &Path) -> String {
    let value = json!({
        "mcpServers": {
            "void": {
                "command": void_bin.display().to_string(),
                "args": ["mcp"],
            }
        }
    });
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_our_commands_from_any_void_binary() {
        let cmd = hook_command(
            Path::new("/Applications/Void Beta.app/bin/void"),
            "session-start",
        );
        assert_eq!(
            cmd,
            "\"/Applications/Void Beta.app/bin/void\" hook session-start"
        );
        assert!(is_ours(&cmd, "session-start"));
        assert!(!is_ours(&cmd, "worktree-remove"));
        assert!(is_ours(
            "/usr/local/bin/void hook worktree-remove",
            "worktree-remove"
        ));
        assert!(!is_ours(
            "/usr/bin/other hook session-start",
            "session-start"
        ));
        assert!(!is_ours(
            "echo void hook session-start now",
            "session-start"
        ));
    }

    #[test]
    fn strip_leaves_foreign_entries_in_a_shared_group() {
        let mut settings = json!({
            "hooks": {"SessionStart": [{"hooks": [
                {"type": "command", "command": "echo hi"},
                {"type": "command", "command": "/x/void hook session-start"}
            ]}]}
        });
        assert!(strip_ours(&mut settings));
        assert_eq!(
            settings,
            json!({"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "echo hi"}]}]}})
        );
    }

    #[test]
    fn snippet_is_valid_json_even_with_odd_paths() {
        let s = mcp_config_snippet(Path::new("C:\\Program Files\\Void\\void.exe"));
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["mcpServers"]["void"]["args"][0], "mcp");
    }
}
