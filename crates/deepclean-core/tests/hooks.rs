//! Installing into Claude Code's settings must preserve everything the user
//! already had, never duplicate, and undo cleanly.

use std::path::{Path, PathBuf};

use deepclean_core::hooks;
use deepclean_core::testkit::FakeHome;
use serde_json::{json, Value};

fn original() -> Value {
    json!({
        "env": {"FOO": "bar"},
        "permissions": {"allow": ["Bash(npm test)"], "deny": ["Read(.env)"]},
        "model": "opus",
        "hooks": {
            "SessionStart": [
                {"matcher": "startup", "hooks": [{"type": "command", "command": "echo hello"}]}
            ],
            "PreToolUse": [
                {"matcher": "Bash", "hooks": [{"type": "command", "command": "./check.sh"}]}
            ]
        }
    })
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn backups(home: &FakeHome) -> Vec<PathBuf> {
    std::fs::read_dir(home.path(".claude"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.json.void-backup-")
        })
        .collect()
}

fn count_ours(settings: &Value, event: &str) -> usize {
    settings["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter(|h| {
                    h["command"]
                        .as_str()
                        .is_some_and(|c| c.contains("void\" hook "))
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn install_preserves_everything_and_uninstall_restores_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let settings = home.file(
        ".claude/settings.json",
        serde_json::to_string_pretty(&original()).unwrap(),
    );
    let bin = Path::new("/opt/Void Tools/void");

    let status = hooks::install(home.root(), bin).unwrap();
    assert!(status.installed);
    assert!(status.session_start && status.worktree_remove);
    assert_eq!(
        status.command.as_deref(),
        Some("\"/opt/Void Tools/void\" hook session-start")
    );

    let after = read(&settings);
    assert_eq!(after["env"], original()["env"]);
    assert_eq!(after["permissions"], original()["permissions"]);
    assert_eq!(after["model"], "opus");
    assert_eq!(
        after["hooks"]["PreToolUse"],
        original()["hooks"]["PreToolUse"]
    );
    assert_eq!(
        after["hooks"]["SessionStart"][0],
        original()["hooks"]["SessionStart"][0],
        "the user's own SessionStart hook is kept, first"
    );
    assert_eq!(count_ours(&after, "SessionStart"), 1);
    assert_eq!(count_ours(&after, "WorktreeRemove"), 1);
    assert!(after.get("WorktreeCreate").is_none());
    assert!(after["hooks"].get("WorktreeCreate").is_none());

    // A backup of the original was written first.
    let written = backups(&home);
    assert_eq!(written.len(), 1);
    assert_eq!(read(&written[0]), original());

    // Idempotent, including from a different Void binary location.
    hooks::install(home.root(), bin).unwrap();
    hooks::install(home.root(), Path::new("/usr/local/bin/void")).unwrap();
    let again = read(&settings);
    let ours = |v: &Value, e: &str| {
        v["hooks"][e]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
            .filter(|h| h["command"].as_str().unwrap().contains(" hook "))
            .count()
    };
    assert_eq!(ours(&again, "SessionStart"), 1);
    assert_eq!(ours(&again, "WorktreeRemove"), 1);
    assert!(hooks::status(home.root()).installed);

    let status = hooks::uninstall(home.root()).unwrap();
    assert!(!status.installed);
    assert_eq!(
        read(&settings),
        original(),
        "uninstall restores the original"
    );
    assert!(!hooks::status(home.root()).installed);
}

#[test]
fn install_into_a_missing_file_creates_it_and_uninstall_empties_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    assert!(!hooks::status(home.root()).installed);

    hooks::install(home.root(), Path::new("/bin/void")).unwrap();
    let settings = home.path(".claude/settings.json");
    assert!(settings.exists());
    assert_eq!(count_ours(&read(&settings), "SessionStart"), 1);
    assert!(hooks::status(home.root()).installed);

    hooks::uninstall(home.root()).unwrap();
    assert_eq!(read(&settings), json!({}));
}

#[test]
fn invalid_json_is_left_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let garbage = "{ \"hooks\": { oops";
    let settings = home.file(".claude/settings.json", garbage);

    assert!(hooks::install(home.root(), Path::new("/bin/void")).is_err());
    assert!(hooks::uninstall(home.root()).is_err());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), garbage);
    assert!(backups(&home).is_empty());

    let status = hooks::status(home.root());
    assert!(!status.installed);
    assert!(status.error.is_some());
}

#[test]
fn uninstall_without_our_hooks_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let raw = serde_json::to_string_pretty(&original()).unwrap();
    let settings = home.file(".claude/settings.json", &raw);

    hooks::uninstall(home.root()).unwrap();
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), raw);
    assert!(backups(&home).is_empty());
}
