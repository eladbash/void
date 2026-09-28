//! Shared helpers: run the real `void` binary against a sandbox home.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use chrono::Utc;
use deepclean_core::model::{
    ActionMethod, ArtifactKind, CleanAction, CleanableItem, Ecosystem, RiskLevel,
};
use deepclean_core::plan::{Plan, PlanEntry, PlanStore};
use deepclean_core::testkit::FakeHome;
use uuid::Uuid;

pub fn void_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_void"));
    cmd.arg("--home").arg(home);
    // Plans must land in the sandbox, whatever the developer's shell says.
    cmd.env_remove("VOID_PLAN_DIR");
    cmd
}

/// Run with `args`, feeding `stdin` (or an empty, closed stdin).
pub fn run(home: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = void_cmd(home);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn void");
    {
        let mut pipe = child.stdin.take().expect("stdin");
        if let Some(input) = stdin {
            pipe.write_all(input.as_bytes()).expect("write stdin");
        }
    }
    child.wait_with_output().expect("wait for void")
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

pub fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}):\n{}\nstderr:\n{}",
            stdout(out),
            stderr(out)
        )
    })
}

/// A Rust project with a `target/` the Rust scanner reports.
pub fn rust_project(home: &FakeHome, rel: &str) -> PathBuf {
    home.file(format!("{rel}/Cargo.toml"), "[package]\nname = \"demo\"\n");
    home.file(format!("{rel}/src/main.rs"), "fn main() {}\n");
    home.sized_file(format!("{rel}/target/debug/demo"), 8192);
    home.path(rel)
}

/// Save a plan holding one Danger entry into the sandbox plan store.
pub fn danger_plan(home: &FakeHome, victim: &Path) -> Uuid {
    let action = CleanAction {
        id: Uuid::new_v4(),
        label: "Delete everything".into(),
        description: String::new(),
        method: ActionMethod::RemoveDir {
            path: victim.to_path_buf(),
        },
        risk: RiskLevel::Danger,
        estimated_savings_bytes: 1,
    };
    let item = CleanableItem {
        id: Uuid::new_v4(),
        path: victim.to_path_buf(),
        ecosystem: Ecosystem::Projects,
        kind: ArtifactKind::StaleProject,
        risk: RiskLevel::Danger,
        size_bytes: 1,
        size_display: "1 B".into(),
        last_modified: None,
        days_stale: None,
        project_name: None,
        project_root: None,
        available_actions: vec![action.clone()],
        details: vec![],
        agent: None,
    };
    let plan = Plan {
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        home: home.root().to_path_buf(),
        entries: vec![PlanEntry { item, action }],
        total_bytes: 1,
        estimated: false,
    };
    PlanStore::new(home.path(".void-sandbox-plans"))
        .save(&plan)
        .expect("save plan");
    plan.id
}
