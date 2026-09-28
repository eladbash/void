//! Plans end to end: a real scan of a fake home, a saved plan, and the real
//! executor applying exactly what the plan lists — nothing more.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::model::{
    ActionMethod, ArtifactKind, CleanAction, CleanableItem, Ecosystem, RiskLevel, ScanEvent,
};
use deepclean_core::plan::{apply_plan, build_plan, PlanFilter, PlanStore, PLAN_TTL};
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::{registry, ScanOrchestrator};
use deepclean_core::testkit::FakeHome;
use deepclean_core::trash::TrashBackend;
use uuid::Uuid;

async fn scan(home: &FakeHome) -> Vec<CleanableItem> {
    let config = AppConfig {
        scan_roots: vec![home.root().to_path_buf()],
        enabled_ecosystems: vec![Ecosystem::Rust, Ecosystem::Node],
        ..AppConfig::default()
    };
    let scanners = registry::build_with_home(&config, home.root().to_path_buf());
    let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
    let mut items = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            ScanEvent::ItemFound { item } => items.push(item),
            ScanEvent::ScanComplete { .. } => break,
            _ => {}
        }
    }
    items
}

fn executor(home: &FakeHome) -> ActionExecutor {
    ActionExecutor::with_trash(
        SafetyChecker::with_home(home.root().to_path_buf(), vec![]),
        TrashBackend::Directory(home.path(".trash-bin")),
    )
}

fn rust_project(home: &FakeHome, name: &str) {
    home.file(
        format!("code/{name}/Cargo.toml"),
        "[package]\nname = \"x\"\n",
    );
    home.file(format!("code/{name}/src/main.rs"), "fn main() {}\n");
    home.sized_file(format!("code/{name}/target/debug/app"), 4096);
}

#[tokio::test]
async fn applying_a_plan_removes_exactly_the_planned_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    rust_project(&home, "alpha");
    rust_project(&home, "beta");
    home.file("code/web/package.json", "{}");
    home.sized_file("code/web/node_modules/left-pad/index.js", 2048);

    let items = scan(&home).await;
    let plan = build_plan(
        &items,
        &PlanFilter {
            paths: vec![home.path("code/alpha")],
            ..PlanFilter::default()
        },
    );
    assert_eq!(plan.entries.len(), 1, "only alpha/target is under the path");
    assert!(plan
        .entries
        .iter()
        .all(|e| e.action.risk == RiskLevel::Safe));
    // A filesystem removal beats `cargo clean` on a tie: no tool needed.
    assert!(matches!(
        plan.entries[0].action.method,
        ActionMethod::RemoveDir { .. }
    ));

    let store = PlanStore::new(tmp.path().join("plans"));
    store.save(&plan).unwrap();
    let loaded = store.load(plan.id).unwrap();
    assert_eq!(loaded.entries.len(), 1);

    let report = apply_plan(&loaded, executor(&home)).await;
    assert_eq!(report.succeeded, 1);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(report.bytes_freed >= 4096);
    assert_eq!(report.run.items.len(), 1);

    assert!(!home.path("code/alpha/target").exists());
    assert!(home.path("code/alpha/src/main.rs").exists());
    assert!(home.path("code/alpha/Cargo.toml").exists());
    assert!(home.path("code/beta/target/debug/app").exists());
    assert!(home
        .path("code/web/node_modules/left-pad/index.js")
        .exists());
}

#[tokio::test]
async fn a_blocked_path_in_a_plan_fails_and_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let ssh = home.file(".ssh/id_ed25519", "secret");
    let item = CleanableItem {
        id: Uuid::new_v4(),
        path: home.path(".ssh"),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::SystemLogs,
        risk: RiskLevel::Safe,
        size_bytes: 6,
        size_display: "6 B".into(),
        last_modified: None,
        days_stale: None,
        project_name: None,
        project_root: None,
        available_actions: vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove".into(),
            description: String::new(),
            method: ActionMethod::RemoveDir {
                path: home.path(".ssh"),
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: 6,
        }],
        details: vec![],
        agent: None,
    };
    let plan = build_plan(&[item], &PlanFilter::default());
    let report = apply_plan(&plan, executor(&home)).await;
    assert_eq!(report.succeeded, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(
        ssh.exists(),
        "the safety checker must still gate plan entries"
    );
}

#[tokio::test]
async fn store_refuses_unknown_and_expired_plans_and_prunes_old_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let store = PlanStore::new(tmp.path().join("plans"));

    let err = store.load(Uuid::new_v4()).unwrap_err();
    assert!(err.contains("no plan"), "{err}");

    let mut old = build_plan(&[], &PlanFilter::default());
    old.created_at = Utc::now() - chrono::Duration::hours(2);
    store.save(&old).unwrap();
    let err = store.load(old.id).unwrap_err();
    assert!(err.contains("expired"), "{err}");

    let fresh = build_plan(&[], &PlanFilter::default());
    store.save(&fresh).unwrap();
    assert!(store.load(fresh.id).is_ok());

    assert_eq!(store.prune_older_than(PLAN_TTL), 1);
    assert!(store.load(fresh.id).is_ok(), "fresh plans survive pruning");
    assert_eq!(store.prune_older_than(Duration::from_secs(3600)), 0);

    store.remove(fresh.id).unwrap();
    assert!(store.load(fresh.id).is_err());
}

#[tokio::test]
async fn danger_actions_are_never_selected_under_a_caution_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    rust_project(&home, "gamma");
    let mut items = scan(&home).await;
    for item in &mut items {
        for action in &mut item.available_actions {
            action.risk = RiskLevel::Danger;
        }
    }
    let plan = build_plan(
        &items,
        &PlanFilter {
            max_risk: RiskLevel::Caution,
            ..PlanFilter::default()
        },
    );
    assert!(plan.entries.is_empty());
    assert!(Path::new(&home.path("code/gamma/target")).exists());
}
