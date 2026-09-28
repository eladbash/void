//! Build caches in the classic scanners: the real orchestrator walking a
//! mixed project tree, and the real executor removing what it found.
//!
//! The scanners' global locations resolve against the process's home, which
//! a test must never read or clean; the orchestrator runs them wrapped so
//! only the walk phase contributes. Global caches are covered by analyzing
//! fake-home paths directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::error::ScanError;
use deepclean_core::model::*;
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::node::NodeScanner;
use deepclean_core::scanner::rust::RustScanner;
use deepclean_core::scanner::{registry, EcosystemScanner, ScanOrchestrator};
use deepclean_core::testkit::FakeHome;
use deepclean_core::trash::TrashBackend;

/// A real scanner with its global locations switched off.
struct WalkOnly(Arc<dyn EcosystemScanner>);

#[async_trait]
impl EcosystemScanner for WalkOnly {
    fn ecosystem(&self) -> Ecosystem {
        self.0.ecosystem()
    }
    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        self.0.is_candidate(file_name, path)
    }
    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        self.0.analyze(path).await
    }
    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        self.0.analyze_many(path).await
    }
}

/// A tree with one of everything, plus the traps: a Maven `target/` next to
/// a Rust one, a crate that also has a `pom.xml`, a venv with no manifest.
fn seed_projects(home: &FakeHome) {
    home.file("code/java-svc/pom.xml", "<project/>");
    home.file("code/java-svc/src/Main.java", "class Main {}");
    home.sized_file("code/java-svc/target/classes/Main.class", 400);

    home.file("code/rust-app/Cargo.toml", "[package]\nname = \"app\"");
    home.sized_file("code/rust-app/target/debug/app", 500);

    // Polyglot: Cargo.toml wins, so the Java scanner must not claim it.
    home.file("code/both/Cargo.toml", "[package]\nname = \"both\"");
    home.file("code/both/pom.xml", "<project/>");
    home.sized_file("code/both/target/debug/both", 300);

    home.file("code/web/package.json", r#"{"name":"web"}"#);
    home.file("code/web/app/page.tsx", "export default 1");
    home.sized_file("code/web/.next/cache/webpack/x.pack", 600);
    home.sized_file("code/web/node_modules/react/index.js", 200);

    home.file("code/py/pyproject.toml", "[project]\nname = \"py\"");
    home.file("code/py/src/py/__init__.py", "");
    home.sized_file("code/py/.pytest_cache/v/cache/lastfailed", 50);
    home.sized_file("code/py/.ruff_cache/0.6.0/123", 70);

    home.file("code/scratch/notes.txt", "no manifest here");
    home.file("code/scratch/.venv/pyvenv.cfg", "home = /usr/bin\n");
    home.sized_file("code/scratch/.venv/lib/python3.12/site-packages/x.py", 90);
}

async fn scan_tree(home: &FakeHome) -> Vec<CleanableItem> {
    let config = AppConfig {
        scan_roots: vec![home.path("code")],
        enabled_ecosystems: vec![
            Ecosystem::Rust,
            Ecosystem::Node,
            Ecosystem::Python,
            Ecosystem::Java,
        ],
        walker_threads: 1,
        max_concurrent_analyses: 4,
        ..Default::default()
    };
    let scanners: Vec<Arc<dyn EcosystemScanner>> =
        registry::build_with_home(&config, home.root().to_path_buf())
            .into_iter()
            .map(|s| Arc::new(WalkOnly(s)) as Arc<dyn EcosystemScanner>)
            .collect();
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

fn of_kind(items: &[CleanableItem], kind: ArtifactKind) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = items
        .iter()
        .filter(|i| i.kind == kind)
        .map(|i| i.path.clone())
        .collect();
    paths.sort();
    paths
}

fn executor(home: &FakeHome, trash: &Path) -> ActionExecutor {
    ActionExecutor::with_trash(
        SafetyChecker::with_home(home.root().to_path_buf(), vec![]),
        TrashBackend::Directory(trash.to_path_buf()),
    )
}

/// Run a batch; returns (succeeded, failed).
async fn run(exec: ActionExecutor, pairs: Vec<(CleanableItem, CleanAction)>) -> (usize, usize) {
    let mut rx = exec.execute_batch(pairs);
    while let Some(ev) = rx.recv().await {
        if let ActionEvent::BatchComplete {
            succeeded, failed, ..
        } = ev
        {
            return (succeeded, failed);
        }
    }
    panic!("batch never completed");
}

fn first_action_matching(
    item: &CleanableItem,
    pred: impl Fn(&ActionMethod) -> bool,
) -> CleanAction {
    item.available_actions
        .iter()
        .find(|a| pred(&a.method))
        .cloned()
        .unwrap_or_else(|| panic!("no matching action on {}", item.path.display()))
}

#[tokio::test]
async fn orchestrator_finds_each_cache_once_and_attributes_target_dirs_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    seed_projects(&home);

    let items = scan_tree(&home).await;

    assert_eq!(
        of_kind(&items, ArtifactKind::MavenTarget),
        vec![home.path("code/java-svc/target")]
    );
    assert_eq!(
        of_kind(&items, ArtifactKind::TargetDir),
        vec![
            home.path("code/both/target"),
            home.path("code/rust-app/target")
        ]
    );
    assert_eq!(
        of_kind(&items, ArtifactKind::FrameworkBuildCache),
        vec![home.path("code/web/.next")]
    );
    assert_eq!(
        of_kind(&items, ArtifactKind::NodeModules),
        vec![home.path("code/web/node_modules")]
    );
    assert_eq!(
        of_kind(&items, ArtifactKind::PythonToolCache),
        vec![
            home.path("code/py/.pytest_cache"),
            home.path("code/py/.ruff_cache")
        ]
    );
    assert_eq!(
        of_kind(&items, ArtifactKind::VenvDir),
        vec![home.path("code/scratch/.venv")]
    );

    // Each path is reported by exactly one scanner.
    let mut paths: Vec<&PathBuf> = items.iter().map(|i| &i.path).collect();
    let total = paths.len();
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), total, "a path was reported twice");

    // The ecosystem matches the kind: Maven is Java, targets are Rust.
    for item in &items {
        match item.kind {
            ArtifactKind::MavenTarget => assert_eq!(item.ecosystem, Ecosystem::Java),
            ArtifactKind::TargetDir => assert_eq!(item.ecosystem, Ecosystem::Rust),
            _ => {}
        }
    }
}

#[tokio::test]
async fn removing_found_caches_keeps_sources_and_manifests() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    seed_projects(&home);
    let items = scan_tree(&home).await;

    let wanted = [
        ArtifactKind::MavenTarget,
        ArtifactKind::FrameworkBuildCache,
        ArtifactKind::PythonToolCache,
        ArtifactKind::VenvDir,
    ];
    let pairs: Vec<(CleanableItem, CleanAction)> = items
        .iter()
        .filter(|i| wanted.contains(&i.kind))
        .map(|i| {
            let action = first_action_matching(i, |m| matches!(m, ActionMethod::RemoveDir { .. }));
            (i.clone(), action)
        })
        .collect();
    assert_eq!(pairs.len(), 5);

    let (ok, failed) = run(executor(&home, &tmp.path().join("trash")), pairs).await;
    assert_eq!((ok, failed), (5, 0));

    for gone in [
        "code/java-svc/target",
        "code/web/.next",
        "code/py/.pytest_cache",
        "code/py/.ruff_cache",
        "code/scratch/.venv",
    ] {
        assert!(!home.path(gone).exists(), "{gone} should be removed");
    }
    for kept in [
        "code/java-svc/pom.xml",
        "code/java-svc/src/Main.java",
        "code/rust-app/target/debug/app",
        "code/both/target/debug/both",
        "code/web/package.json",
        "code/web/app/page.tsx",
        "code/web/node_modules/react/index.js",
        "code/py/pyproject.toml",
        "code/py/src/py/__init__.py",
        "code/scratch/notes.txt",
    ] {
        assert!(home.path(kept).exists(), "{kept} should survive");
    }
}

#[tokio::test]
async fn cargo_registry_clean_keeps_the_index() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    home.sized_file(
        ".cargo/registry/cache/index.crates.io-1/serde-1.0.0.crate",
        300,
    );
    home.sized_file(
        ".cargo/registry/src/index.crates.io-1/serde-1.0.0/lib.rs",
        200,
    );
    home.sized_file(".cargo/registry/index/index.crates.io-1/config.json", 100);
    home.file(".cargo/config.toml", "[net]\n");

    let registry = home.path(".cargo/registry");
    let item = RustScanner.analyze(&registry).await.unwrap().unwrap();
    assert_eq!(item.kind, ArtifactKind::CargoRegistry);
    assert_eq!(item.size_bytes, 500);
    let action = item.available_actions[0].clone();
    let ActionMethod::RemoveDirs { paths } = &action.method else {
        panic!("expected RemoveDirs, got {:?}", action.method);
    };
    assert!(paths.iter().all(|p| !p.starts_with(registry.join("index"))));

    let (ok, failed) = run(
        executor(&home, &tmp.path().join("trash")),
        vec![(item, action)],
    )
    .await;
    assert_eq!((ok, failed), (1, 0));
    assert!(!home.path(".cargo/registry/cache").exists());
    assert!(!home.path(".cargo/registry/src").exists());
    assert!(home
        .path(".cargo/registry/index/index.crates.io-1/config.json")
        .exists());
    assert!(home.path(".cargo/config.toml").exists());
}

#[tokio::test]
async fn playwright_old_builds_removal_keeps_newest_and_mcp_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    for build in [
        "chromium-1140",
        "chromium-1217",
        "chromium_headless_shell-1217",
        "firefox-1466",
        "firefox-1482",
    ] {
        home.sized_file(format!(".cache/ms-playwright/{build}/bin"), 100);
    }
    home.sized_file(
        ".cache/ms-playwright/mcp-chrome-ac81000/Default/Cookies",
        10,
    );

    let root = home.path(".cache/ms-playwright");
    let item = NodeScanner.analyze(&root).await.unwrap().unwrap();
    assert_eq!(item.kind, ArtifactKind::PlaywrightBrowsers);
    let action = item.available_actions[0].clone();
    assert_eq!(action.label, "Remove old browser builds");
    assert_eq!(action.estimated_savings_bytes, 200);

    let (ok, failed) = run(
        executor(&home, &tmp.path().join("trash")),
        vec![(item, action)],
    )
    .await;
    assert_eq!((ok, failed), (1, 0));

    assert!(!root.join("chromium-1140").exists());
    assert!(!root.join("firefox-1466").exists());
    for kept in [
        "chromium-1217/bin",
        "chromium_headless_shell-1217/bin",
        "firefox-1482/bin",
        "mcp-chrome-ac81000/Default/Cookies",
    ] {
        assert!(root.join(kept).exists(), "{kept} should survive");
    }
}
