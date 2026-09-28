//! Guard policies against items from the real scanners, executed by the
//! real executor.

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::{AppConfig, Policy};
use deepclean_core::guard::{self, DiskState};
use deepclean_core::model::{
    ActionEvent, ActionMethod, CleanableItem, Ecosystem, RiskLevel, ScanEvent,
};
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::{registry, ScanOrchestrator};
use deepclean_core::testkit::{self, projects, FakeHome};
use deepclean_core::trash::TrashBackend;

async fn scan(home: &FakeHome, ecosystems: Vec<Ecosystem>) -> Vec<CleanableItem> {
    let config = AppConfig {
        scan_roots: vec![home.root().to_path_buf()],
        enabled_ecosystems: ecosystems,
        walker_threads: 1,
        ..Default::default()
    };
    let scanners = registry::build_with_home(&config, home.root().to_path_buf());
    let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
    let mut items = Vec::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            // Some older scanners still resolve their global caches against
            // the real home; keep only what lives in the sandbox.
            ScanEvent::ItemFound { item } if item.path.starts_with(home.root()) => items.push(item),
            ScanEvent::ScanComplete { .. } => break,
            _ => {}
        }
    }
    items
}

fn all_enabled() -> Vec<Policy> {
    Policy::defaults()
        .into_iter()
        .map(|p| Policy { enabled: true, ..p })
        .collect()
}

fn node_project(home: &FakeHome, name: &str, days: Option<u64>) -> std::path::PathBuf {
    home.file(format!("code/{name}/package.json"), "{}");
    home.file(format!("code/{name}/index.js"), "1");
    home.sized_file(format!("code/{name}/node_modules/dep/index.js"), 2048);
    let nm = home.path(format!("code/{name}/node_modules"));
    if let Some(d) = days {
        testkit::age_tree(&nm, d);
    }
    nm
}

#[tokio::test]
async fn default_policies_pick_only_the_stale_build_dir_and_clean_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let stale = node_project(&home, "old", Some(40));
    let fresh = node_project(&home, "new", None);

    let items = scan(&home, vec![Ecosystem::Node]).await;
    assert_eq!(items.len(), 2, "{items:#?}");

    // Shipped disabled: nothing selected until the user opts in.
    assert!(guard::policy_selections(&items, &Policy::defaults()).is_empty());

    let selections = guard::policy_selections(&items, &all_enabled());
    assert_eq!(selections.len(), 1);
    let sel = &selections[0];
    assert_eq!(sel.item.path, stale);
    assert_eq!(sel.policy_id, "stale-build-dirs");
    assert_eq!(sel.action.risk, RiskLevel::Safe);
    assert!(matches!(sel.action.method, ActionMethod::RemoveDir { .. }));

    let bin = tmp.path().join("bin");
    let safety = SafetyChecker::with_home(home.root().to_path_buf(), vec![]);
    let exec = ActionExecutor::with_trash(safety, TrashBackend::Directory(bin.clone()));
    let batch = selections.into_iter().map(|s| (s.item, s.action)).collect();
    let mut rx = exec.execute_batch(batch);
    let mut ok = 0;
    while let Some(ev) = rx.recv().await {
        if let ActionEvent::BatchComplete {
            succeeded, failed, ..
        } = ev
        {
            ok = succeeded;
            assert_eq!(failed, 0);
        }
    }
    assert_eq!(ok, 1);
    assert!(!stale.exists());
    assert!(home.path("code/old/package.json").is_file());
    assert!(fresh.join("dep/index.js").is_file());
    assert!(!bin.exists(), "a cache is removed, not trashed");
}

#[tokio::test]
async fn stale_projects_are_never_selected_even_by_a_hand_edited_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let pushed = projects::stale_pushed(&home, "pushed");
    projects::stale_unpushed_with_node_modules(&home, "spike");

    let items = scan(&home, vec![Ecosystem::Projects]).await;
    assert_eq!(items.len(), 2);

    let reckless = Policy {
        id: "reckless".into(),
        enabled: true,
        ecosystems: vec![Ecosystem::Projects],
        action: deepclean_core::config::PolicyAction::Safest,
        max_risk: RiskLevel::Danger,
        ..Default::default()
    };
    let selections = guard::policy_selections(&items, &[reckless]);
    // Only the Safe strip of the spike's node_modules; never a project trash.
    assert_eq!(selections.len(), 1);
    assert!(matches!(
        selections[0].action.method,
        ActionMethod::RemoveDirs { .. }
    ));
    assert!(selections.iter().all(|s| s.item.path != pushed));
}

#[test]
fn guard_status_on_a_synthetic_volume() {
    let usage = deepclean_core::disk::DiskUsage {
        total_bytes: 100,
        available_bytes: 3,
        used_bytes: 97,
    };
    let status = guard::evaluate(&usage, &AppConfig::default().guard);
    assert_eq!(status.state, DiskState::Critical);
}
