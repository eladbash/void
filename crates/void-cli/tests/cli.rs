//! The `void` binary end to end, always against a sandbox `--home`.

mod common;

use std::path::Path;

use chrono::Utc;
use common::*;
use deepclean_core::plan::PlanStore;
use deepclean_core::testkit::FakeHome;
use serde_json::{json, Value};

fn plan_json(home: &FakeHome, extra: &[&str]) -> Value {
    let mut args = vec!["plan", "--json"];
    args.extend_from_slice(extra);
    let out = run(home.root(), &args, None);
    assert!(out.status.success(), "plan failed: {}", stderr(&out));
    json(&out)
}

#[test]
fn seeded_sandbox_scans_several_ecosystems_and_stays_inside_home() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sandbox");
    let seeded = std::process::Command::new(env!("CARGO_BIN_EXE_void"))
        .args(["dev", "seed"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(seeded.status.success(), "seed failed: {}", stderr(&seeded));
    assert!(stdout(&seeded).contains("--home"));

    let home = FakeHome::at(&dir);
    let project = rust_project(&home, "code/rusty");

    let out = run(home.root(), &["scan", "--json"], None);
    assert!(out.status.success(), "scan failed: {}", stderr(&out));
    let items = json(&out);
    let items = items.as_array().expect("a JSON array");

    let target = project.join("target");
    assert!(
        items
            .iter()
            .any(|i| i["kind"] == "target_dir" && Path::new(i["path"].as_str().unwrap()) == target),
        "the seeded Rust target/ is reported"
    );
    let ecosystems: std::collections::HashSet<&str> = items
        .iter()
        .filter_map(|i| i["ecosystem"].as_str())
        .collect();
    assert!(
        ecosystems.len() >= 2,
        "several ecosystems, got {ecosystems:?}"
    );
    for item in items {
        let path = Path::new(item["path"].as_str().unwrap());
        assert!(
            path.starts_with(home.root()),
            "sandbox scan escaped home: {}",
            path.display()
        );
    }
}

#[test]
fn safe_plan_applies_exactly_what_it_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let project = rust_project(&home, "code/app");
    home.file("code/web/package.json", "{}");
    home.sized_file("code/web/node_modules/dep/index.js", 4096);

    let plan = plan_json(
        &home,
        &["--max-risk", "safe", "--path", project.to_str().unwrap()],
    );
    let entries = plan["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{plan:#}");
    for entry in entries {
        assert_eq!(entry["action"]["risk"], "safe");
    }
    assert_eq!(
        Path::new(entries[0]["path"].as_str().unwrap()),
        project.join("target")
    );
    assert!(entries[0]["runs"].as_str().unwrap().contains("target"));
    let id = plan["plan_id"].as_str().unwrap().to_string();

    // Not a TTY and no --yes: refused, nothing touched.
    let refused = run(home.root(), &["apply", &id], None);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    assert!(project.join("target").exists());

    let applied = run(home.root(), &["apply", &id, "--yes", "--json"], None);
    assert!(
        applied.status.success(),
        "apply failed: {}",
        stderr(&applied)
    );
    let report = json(&applied);
    assert_eq!(report["succeeded"], 1);
    assert_eq!(report["failed"], json!([]));

    assert!(!project.join("target").exists(), "planned path removed");
    assert!(project.join("src/main.rs").exists(), "source intact");
    assert!(project.join("Cargo.toml").exists());
    assert!(
        home.path("code/web/node_modules/dep/index.js").exists(),
        "unplanned path intact"
    );

    // Recorded where the app's History screen reads it (sandboxed).
    let history: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path(".void-sandbox-config/history.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 1);
    assert_eq!(history["runs"][0]["trigger"], "cli");

    // Plans are single use.
    let again = run(home.root(), &["apply", &id, "--yes"], None);
    assert!(!again.status.success());
}

#[test]
fn unknown_malformed_and_expired_plans_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());

    let unknown = run(
        home.root(),
        &["apply", &uuid::Uuid::new_v4().to_string(), "--yes"],
        None,
    );
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("no plan"), "{}", stderr(&unknown));

    let garbage = run(home.root(), &["apply", "not-a-plan", "--yes"], None);
    assert!(!garbage.status.success());

    // An expired plan that would otherwise remove a real directory.
    let project = rust_project(&home, "code/old");
    let plan = plan_json(&home, &["--path", project.to_str().unwrap()]);
    let id = uuid::Uuid::parse_str(plan["plan_id"].as_str().unwrap()).unwrap();
    let store = PlanStore::new(home.path(".void-sandbox-plans"));
    let mut saved = store.load(id).unwrap();
    saved.created_at = Utc::now() - chrono::Duration::hours(2);
    store.save(&saved).unwrap();

    let expired = run(home.root(), &["apply", &id.to_string(), "--yes"], None);
    assert!(!expired.status.success());
    assert!(stderr(&expired).contains("expired"), "{}", stderr(&expired));
    assert!(project.join("target").exists());
}

#[test]
fn danger_is_refused_by_plan_and_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());

    let out = run(home.root(), &["plan", "--max-risk", "danger"], None);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("Void app"), "{}", stderr(&out));

    let victim = home.dir("code/precious");
    home.file("code/precious/notes.md", "keep");
    let id = danger_plan(&home, &victim);
    let out = run(home.root(), &["apply", &id.to_string(), "--yes"], None);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("Danger"), "{}", stderr(&out));
    assert!(victim.join("notes.md").exists());
}

#[test]
fn hook_install_status_uninstall_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let original = json!({
        "permissions": {"allow": ["Bash(ls)"]},
        "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "say done"}]}]}
    });
    let settings = home.file(".claude/settings.json", original.to_string());

    let status = json(&run(home.root(), &["hook", "status", "--json"], None));
    assert_eq!(status["installed"], false);

    let out = run(home.root(), &["hook", "install"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    let installed: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(installed["permissions"], original["permissions"]);
    assert_eq!(installed["hooks"]["Stop"], original["hooks"]["Stop"]);
    let cmd = installed["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(cmd.ends_with(" hook session-start"), "{cmd}");
    assert!(cmd.contains("void"), "{cmd}");
    let cmd = installed["hooks"]["WorktreeRemove"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(cmd.ends_with(" hook worktree-remove"), "{cmd}");

    // Idempotent.
    run(home.root(), &["hook", "install"], None);
    let twice: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(twice, installed);

    let status = json(&run(home.root(), &["hook", "status", "--json"], None));
    assert_eq!(status["installed"], true);

    let out = run(home.root(), &["hook", "uninstall"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(after, original);
}

fn write_config(dir: &Path, warn: u8, critical: u8) -> std::path::PathBuf {
    let path = dir.join("config.json");
    std::fs::write(
        &path,
        json!({"guard": {"warn_free_percent": warn, "critical_free_percent": critical}})
            .to_string(),
    )
    .unwrap();
    path
}

#[test]
fn session_start_warns_only_when_disk_is_low_and_always_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let cfg_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let payload = json!({
        "session_id": "abc",
        "hook_event_name": "SessionStart",
        "cwd": home.root(),
    })
    .to_string();

    // Thresholds so high every disk is "low".
    let always = write_config(&cfg_dir, 100, 1);
    let started = std::time::Instant::now();
    let out = run(
        home.root(),
        &[
            "--config",
            always.to_str().unwrap(),
            "hook",
            "session-start",
        ],
        Some(&payload),
    );
    assert!(out.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let text = stdout(&out);
    assert!(
        text.contains("Disk") && text.contains("void plan"),
        "{text}"
    );

    // Guard check agrees, via its exit code.
    let check = run(
        home.root(),
        &["--config", always.to_str().unwrap(), "guard", "check"],
        None,
    );
    assert_eq!(check.status.code(), Some(1), "{}", stdout(&check));

    // Thresholds so low no disk is.
    let never = write_config(&cfg_dir, 1, 1);
    let out = run(
        home.root(),
        &["--config", never.to_str().unwrap(), "hook", "session-start"],
        Some(&payload),
    );
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");

    // Garbage or missing input still exits 0.
    let out = run(
        home.root(),
        &[
            "--config",
            always.to_str().unwrap(),
            "hook",
            "session-start",
        ],
        Some("not json"),
    );
    assert!(out.status.success());
    let out = run(
        home.root(),
        &["--config", never.to_str().unwrap(), "hook", "session-start"],
        None,
    );
    assert!(out.status.success());
}

#[test]
fn worktree_remove_trims_build_dirs_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let wt = home.dir("code/app/.claude/worktrees/feature");
    home.file(
        "code/app/.claude/worktrees/feature/.git",
        "gitdir: /nowhere\n",
    );
    home.file("code/app/.claude/worktrees/feature/package.json", "{}");
    home.file("code/app/.claude/worktrees/feature/src/app.js", "code");
    home.sized_file(
        "code/app/.claude/worktrees/feature/node_modules/dep/index.js",
        2048,
    );
    home.sized_file(
        "code/app/.claude/worktrees/feature/packages/ui/node_modules/x/i.js",
        1024,
    );
    home.sized_file("code/app/.claude/worktrees/feature/.next/cache/a", 512);
    // A `target/` that is not a build folder (no Cargo.toml/pom.xml beside it).
    home.file(
        "code/app/.claude/worktrees/feature/docs/target/plan.md",
        "mine",
    );

    let payload = json!({
        "session_id": "abc",
        "hook_event_name": "WorktreeRemove",
        "cwd": wt,
        "worktree_path": wt,
    })
    .to_string();
    let out = run(home.root(), &["hook", "worktree-remove"], Some(&payload));
    assert!(out.status.success());
    assert_eq!(stdout(&out), "", "silent on success");

    assert!(!wt.join("node_modules").exists());
    assert!(!wt.join("packages/ui/node_modules").exists());
    assert!(!wt.join(".next").exists());
    assert!(wt.join("src/app.js").exists());
    assert!(wt.join("package.json").exists());
    assert!(wt.join("docs/target/plan.md").exists());
    assert!(
        wt.join(".git").exists(),
        "the worktree itself is left alone"
    );

    // Not a worktree (no .git): nothing is removed.
    home.sized_file("code/plain/node_modules/dep/index.js", 10);
    let payload = json!({"worktree_path": home.path("code/plain")}).to_string();
    let out = run(home.root(), &["hook", "worktree-remove"], Some(&payload));
    assert!(out.status.success());
    assert!(home.path("code/plain/node_modules/dep/index.js").exists());

    // Missing or bad input still exits 0.
    assert!(run(home.root(), &["hook", "worktree-remove"], Some("{}"))
        .status
        .success());
    assert!(run(home.root(), &["hook", "worktree-remove"], None)
        .status
        .success());
}
