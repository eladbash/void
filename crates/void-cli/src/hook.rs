//! `void hook …`: installing into Claude Code, and the handlers Claude Code
//! runs.
//!
//! The handlers run inside someone else's session, so they are held to a
//! stricter contract than the rest of the CLI: always exit 0 (a failing hook
//! would surface as an error in the agent's session), never scan, finish
//! well under two seconds, and write to stdout only what is meant for the
//! agent's context.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use deepclean_core::guard::DiskState;
use deepclean_core::hooks;
use deepclean_core::model::{
    ActionMethod, ArtifactKind, CleanAction, CleanableItem, Ecosystem, RiskLevel,
};
use deepclean_core::plan::{apply_plan, build_plan, PlanFilter};
use serde_json::Value;
use uuid::Uuid;

use crate::commands::current_exe;
use crate::ctx::Ctx;
use crate::output::{print_json, size};

pub fn install(ctx: &Ctx) -> Result<i32, String> {
    let status = hooks::install(&ctx.home, &current_exe()?)?;
    report(ctx, &status, "Installed");
    Ok(0)
}

pub fn uninstall(ctx: &Ctx) -> Result<i32, String> {
    let status = hooks::uninstall(&ctx.home)?;
    report(ctx, &status, "Removed");
    Ok(0)
}

pub fn status(ctx: &Ctx) -> Result<i32, String> {
    let status = hooks::status(&ctx.home);
    if ctx.json {
        print_json(&status);
    } else {
        if let Some(err) = &status.error {
            println!("{err}");
        }
        println!(
            "{}: SessionStart {}, WorktreeRemove {}",
            status.settings_path.display(),
            if status.session_start {
                "installed"
            } else {
                "not installed"
            },
            if status.worktree_remove {
                "installed"
            } else {
                "not installed"
            },
        );
    }
    Ok(if status.error.is_some() { 1 } else { 0 })
}

fn report(ctx: &Ctx, status: &hooks::HookStatus, verb: &str) {
    if ctx.json {
        print_json(status);
        return;
    }
    println!(
        "{verb} Void's Claude Code hooks in {}",
        status.settings_path.display()
    );
    if let Some(backup) = &status.backup_path {
        println!("Backup of the previous file: {}", backup.display());
    }
    if status.installed {
        println!("  SessionStart   → warns the agent when disk space is low");
        println!("  WorktreeRemove → removes node_modules/target/.venv/.next/.turbo inside a removed worktree");
    }
}

/// Read the hook's JSON payload from stdin without ever blocking the
/// session: a terminal is not read at all, and a pipe that never closes is
/// abandoned after `wait`.
fn read_payload(wait: Duration) -> Value {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return Value::Null;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::stdin().take(1 << 20).read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    rx.recv_timeout(wait)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

fn payload_path(payload: &Value, key: &str) -> Option<PathBuf> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.exists())
}

/// SessionStart: a one-paragraph warning for the agent's context when the
/// disk is low. Silent otherwise. No scan — this must be instant.
pub fn session_start(ctx: &Ctx) -> i32 {
    if !ctx.config.guard.enabled {
        return 0;
    }
    let payload = read_payload(Duration::from_millis(500));
    let path = payload_path(&payload, "cwd").unwrap_or_else(|| ctx.home.clone());
    let Some(status) = ctx.guard_status(Some(&path)) else {
        return 0;
    };
    if status.state == DiskState::Ok {
        return 0;
    }
    let used = (100.0 - status.free_percent).clamp(0.0, 100.0);
    let urgency = if status.state == DiskState::Critical {
        "critically low"
    } else {
        "running low"
    };
    println!(
        "⚠ Disk space is {urgency}: the disk is {used:.0}% full ({} free). \
         Before installing dependencies, downloading models, or creating worktrees, \
         consider asking the user to run `void plan` to review safe cleanups \
         (rebuildable caches, build folders, idle agent worktrees), then `void apply <plan-id>` \
         once they approve. If the `void` MCP server is available, use its disk_status, scan \
         and plan_cleanup tools — and only call apply_plan after the user explicitly approves the plan.",
        size(status.free_bytes)
    );
    0
}

/// Directory names that are always regenerable build output, and what (if
/// anything) must sit beside them to prove it.
fn is_build_dir(dir: &Path, name: &str) -> bool {
    let parent = dir.parent();
    let sibling = |f: &str| parent.is_some_and(|p| p.join(f).exists());
    match name {
        "node_modules" | ".next" | ".turbo" => true,
        // A bare `target/` could be anything; Cargo and Maven make it theirs.
        "target" => sibling("Cargo.toml") || sibling("pom.xml"),
        // A virtualenv carries its own marker.
        ".venv" => dir.join("pyvenv.cfg").exists(),
        _ => false,
    }
}

/// Build directories strictly inside `root`, not descending into them,
/// symlinks, or `.git`.
fn find_build_dirs(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        if is_build_dir(&path, &name) {
            out.push(path);
        } else {
            find_build_dirs(&path, depth - 1, out);
        }
    }
}

/// WorktreeRemove: trim the regenerable build output inside the worktree.
/// Never the worktree itself — git (or Claude Code) owns that.
pub async fn worktree_remove(ctx: &Ctx) -> i32 {
    let payload = read_payload(Duration::from_secs(2));
    let Some(worktree) = payload_path(&payload, "worktree_path") else {
        return 0;
    };
    if let Err(err) = trim_worktree(ctx, &worktree).await {
        eprintln!("void: {err}");
    }
    0
}

async fn trim_worktree(ctx: &Ctx, worktree: &Path) -> Result<(), String> {
    let worktree = worktree
        .canonicalize()
        .map_err(|err| format!("{}: {err}", worktree.display()))?;
    if !worktree.is_dir() {
        return Ok(());
    }
    // Only a real checkout: a path like `/` or home must never be walked for
    // build folders to delete.
    let home = ctx.home.canonicalize().unwrap_or_else(|_| ctx.home.clone());
    if home.starts_with(&worktree) {
        return Err(format!(
            "refusing to trim {}: it is home or contains it",
            worktree.display()
        ));
    }
    if !worktree.join(".git").exists() {
        return Err(format!(
            "refusing to trim {}: not a git worktree",
            worktree.display()
        ));
    }

    let mut dirs = Vec::new();
    find_build_dirs(&worktree, 6, &mut dirs);
    let safety = ctx.safety();
    dirs.retain(|d| d != &worktree && d.starts_with(&worktree) && safety.is_path_allowed(d));
    if dirs.is_empty() {
        return Ok(());
    }

    let sizes: u64 = dirs
        .iter()
        .map(|d| deepclean_core::staleness::compute_dir_size_sync(d))
        .sum();
    let item = CleanableItem {
        id: Uuid::new_v4(),
        path: worktree.clone(),
        ecosystem: Ecosystem::Worktrees,
        kind: ArtifactKind::AgentWorktree,
        risk: RiskLevel::Safe,
        size_bytes: sizes,
        size_display: size(sizes),
        last_modified: None,
        days_stale: None,
        project_name: worktree
            .file_name()
            .map(|n| n.to_string_lossy().to_string()),
        project_root: Some(worktree.clone()),
        available_actions: vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove build artifacts".into(),
            description: "Remove regenerable build folders inside a worktree being removed".into(),
            method: ActionMethod::RemoveDirs { paths: dirs },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: sizes,
        }],
        details: Vec::new(),
        agent: Some("claude".into()),
    };
    let plan = build_plan(&[item], &PlanFilter::default());
    let report = apply_plan(&plan, ctx.executor()).await;
    if let Err(err) = ctx.record_history(report.run, "hook") {
        eprintln!("void: {err}");
    }
    for f in report.failed {
        eprintln!("void: could not trim {}: {}", f.path.display(), f.error);
    }
    Ok(())
}
