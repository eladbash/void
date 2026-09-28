//! The project graveyard, end to end: real git repos in a fake home, the real
//! scanner, the real executor with a directory Trash.

use std::path::{Path, PathBuf};

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::model::{ActionEvent, ActionMethod, CleanAction, CleanableItem, RiskLevel};
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::projects::ProjectScanner;
use deepclean_core::scanner::EcosystemScanner;
use deepclean_core::testkit::{self, projects, FakeHome};
use deepclean_core::trash::TrashBackend;

fn config_for(home: &FakeHome, roots: Vec<PathBuf>) -> AppConfig {
    let _ = home;
    AppConfig {
        scan_roots: roots,
        ..Default::default()
    }
}

async fn scan(home: &FakeHome, root: &Path) -> Vec<CleanableItem> {
    let config = config_for(home, vec![root.to_path_buf()]);
    let scanner = ProjectScanner::new(home.root().to_path_buf(), &config);
    let mut items = Vec::new();
    for loc in scanner.global_locations() {
        items.extend(scanner.analyze_many(&loc).await.unwrap());
    }
    items
}

fn find<'a>(items: &'a [CleanableItem], repo: &Path) -> &'a CleanableItem {
    items
        .iter()
        .find(|i| i.path == repo)
        .unwrap_or_else(|| panic!("{} not listed", repo.display()))
}

fn trash_action(item: &CleanableItem) -> &CleanAction {
    item.available_actions
        .iter()
        .find(|a| matches!(a.method, ActionMethod::MoveToTrash { .. }))
        .expect("trash action")
}

fn detail<'a>(item: &'a CleanableItem, label: &str) -> &'a str {
    item.details
        .iter()
        .find(|d| d.label == label)
        .map(|d| d.value.as_str())
        .unwrap_or_else(|| panic!("no detail {label}"))
}

async fn run(home: &FakeHome, bin: &Path, item: &CleanableItem, action: &CleanAction) -> usize {
    let safety = SafetyChecker::with_home(home.root().to_path_buf(), vec![]);
    let exec = ActionExecutor::with_trash(safety, TrashBackend::Directory(bin.to_path_buf()));
    let mut rx = exec.execute_batch(vec![(item.clone(), action.clone())]);
    let mut succeeded = 0;
    while let Some(ev) = rx.recv().await {
        if let ActionEvent::BatchComplete { succeeded: s, .. } = ev {
            succeeded = s;
        }
    }
    succeeded
}

#[tokio::test]
async fn lists_only_stale_projects_with_honest_risks() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let pushed = projects::stale_pushed(&home, "pushed");
    let spike = projects::stale_unpushed_with_node_modules(&home, "spike");
    let dirty = projects::stale_dirty(&home, "dirty");
    let fresh = projects::fresh(&home, "fresh");

    let items = scan(&home, home.root()).await;
    let paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
    assert_eq!(items.len(), 3, "{paths:?}");
    assert!(!paths.contains(&fresh));
    assert!(!paths.contains(&home.root().to_path_buf()));

    // Pushed and clean: re-clonable, so Caution.
    let p = find(&items, &pushed);
    assert_eq!(trash_action(p).risk, RiskLevel::Caution);
    assert_eq!(p.risk, RiskLevel::Caution);
    assert_eq!(detail(p, "Unpushed commits"), "0");
    assert_eq!(detail(p, "Uncommitted changes"), "none");
    assert!(p.days_stale.unwrap() >= 199);
    assert!(p
        .available_actions
        .iter()
        .all(|a| !matches!(a.method, ActionMethod::RemoveDirs { .. })));

    // Never pushed: the only copy. Danger, but stripping artifacts is Safe.
    let s = find(&items, &spike);
    let t = trash_action(s);
    assert_eq!(t.risk, RiskLevel::Danger);
    assert!(t.description.contains("never pushed"), "{}", t.description);
    assert_eq!(detail(s, "Remote"), "none — never pushed");
    assert_eq!(s.available_actions[0].risk, RiskLevel::Safe);
    assert_eq!(s.risk, RiskLevel::Safe);
    // The ignored node_modules is not "uncommitted work".
    assert_eq!(detail(s, "Uncommitted changes"), "none");

    // Pushed but dirty: work exists nowhere else.
    let d = find(&items, &dirty);
    assert_eq!(trash_action(d).risk, RiskLevel::Danger);
    assert_eq!(detail(d, "Uncommitted changes"), "1 file");
}

#[tokio::test]
async fn strip_artifacts_removes_node_modules_and_keeps_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let spike = projects::stale_unpushed_with_node_modules(&home, "spike");
    let bin = tmp.path().join("bin");

    let items = scan(&home, home.root()).await;
    let item = find(&items, &spike);
    let strip = &item.available_actions[0];
    assert!(matches!(strip.method, ActionMethod::RemoveDirs { .. }));
    assert!(strip.estimated_savings_bytes >= 12_288);

    assert_eq!(run(&home, &bin, item, strip).await, 1);
    assert!(!spike.join("node_modules").exists());
    assert!(spike.join("package.json").is_file());
    assert!(spike.join("index.js").is_file());
    assert!(spike.join(".git").is_dir());
    assert_eq!(
        testkit::git(&spike, &["log", "--oneline"]).lines().count(),
        1
    );
    assert!(
        !bin.exists(),
        "a permanent strip must not go through the Trash"
    );
}

#[tokio::test]
async fn move_to_trash_lands_in_the_test_trash_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let pushed = projects::stale_pushed(&home, "pushed");
    let bin = tmp.path().join("bin");

    let items = scan(&home, home.root()).await;
    let item = find(&items, &pushed);
    assert_eq!(run(&home, &bin, item, trash_action(item)).await, 1);

    assert!(!pushed.exists());
    let moved: Vec<_> = std::fs::read_dir(&bin).unwrap().flatten().collect();
    assert_eq!(moved.len(), 1);
    let m = moved[0].path();
    assert!(m.file_name().unwrap().to_string_lossy().ends_with("pushed"));
    assert_eq!(
        std::fs::read_to_string(m.join("README.md")).unwrap(),
        "# pushed\n"
    );
    assert!(m.join(".git").is_dir());
    assert!(testkit::git(&m, &["log", "--oneline"]).contains("initial"));
}

#[tokio::test]
async fn fresh_projects_and_scan_roots_are_never_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fresh = projects::fresh(&home, "fresh");

    // The scan root is itself a stale repo (e.g. a dotfiles repo at `~`).
    testkit::git(home.root(), &["init", "-q", "-b", "main"]);
    home.file("dotfile.txt", "x");
    projects::commit_dated(home.root(), "dotfiles", 400);
    testkit::age_tree(&home.path("dotfile.txt"), 400);

    assert!(scan(&home, home.root()).await.is_empty());
    let code = home.path("code");
    assert!(scan(&home, &code).await.is_empty());
    assert!(fresh.join("node_modules").exists());
}

#[tokio::test]
async fn scan_root_that_is_the_project_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let pushed = projects::stale_pushed(&home, "pushed");
    assert!(scan(&home, &pushed).await.is_empty());
}

#[tokio::test]
async fn stale_threshold_is_configurable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    projects::stale_pushed(&home, "pushed");
    let mut config = config_for(&home, vec![home.root().to_path_buf()]);
    config.ai.stale_project_days = 365;
    let scanner = ProjectScanner::new(home.root().to_path_buf(), &config);
    assert!(scanner.analyze_many(home.root()).await.unwrap().is_empty());
}

#[tokio::test]
async fn project_with_a_sentinel_is_not_offered_for_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let spike = projects::stale_unpushed_with_node_modules(&home, "spike");
    home.file("code/spike/.env", "API_KEY=x");
    testkit::age_tree(&spike, projects::STALE_DAYS);

    let items = scan(&home, home.root()).await;
    let item = find(&items, &spike);
    assert!(item
        .available_actions
        .iter()
        .all(|a| !matches!(a.method, ActionMethod::MoveToTrash { .. })));
    assert!(detail(item, "Move to Trash").starts_with("unavailable"));
}

#[tokio::test]
async fn projects_under_agent_worktree_roots_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    // Agents keep their checkouts under hidden roots and `~/conductor`.
    for (name, dest) in [
        ("a", ".claude/worktrees/a"),
        ("b", ".cursor/worktrees/b"),
        ("c", "conductor/workspaces/c"),
    ] {
        let repo = projects::stale_pushed(&home, name);
        let target = home.path(dest);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::rename(&repo, &target).unwrap();
    }
    assert!(scan(&home, home.root()).await.is_empty());
}

#[test]
fn seed_builds_the_fixtures() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    projects::seed(&home);
    for name in [
        "old-landing-page",
        "agent-spike-todo-app",
        "half-finished-cli",
        "current-work",
    ] {
        assert!(home.path("code").join(name).join(".git").is_dir(), "{name}");
    }
}
