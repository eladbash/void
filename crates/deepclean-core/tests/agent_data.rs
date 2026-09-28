//! AI agent data hygiene: the real scanner against a fake home, and the real
//! executor (safety checker included) deleting from it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::model::*;
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::agents::AgentDataScanner;
use deepclean_core::scanner::EcosystemScanner;
use deepclean_core::testkit::agents::{claude_home, codex_home, editor_user};
use deepclean_core::testkit::{age_tree, FakeHome};
use deepclean_core::trash::TrashBackend;

const MB: usize = 1024 * 1024;

fn config() -> AppConfig {
    let mut config = AppConfig::default();
    config.ai.agent_data_retention_days = 30;
    config.ai.large_session_file_mb = 1;
    config
}

fn scanner(home: &FakeHome) -> AgentDataScanner {
    AgentDataScanner::new(home.root().to_path_buf(), &config())
}

async fn scan(scanner: &AgentDataScanner) -> Vec<CleanableItem> {
    let mut items = Vec::new();
    for loc in scanner.global_locations() {
        items.extend(scanner.analyze_many(&loc).await.unwrap());
    }
    items
}

fn action_paths(action: &CleanAction) -> Vec<PathBuf> {
    match &action.method {
        ActionMethod::RemoveFiles { paths } | ActionMethod::RemoveDirs { paths } => paths.clone(),
        ActionMethod::RemoveFile { path } | ActionMethod::RemoveDir { path } => vec![path.clone()],
        _ => vec![],
    }
}

fn all_paths(items: &[CleanableItem]) -> Vec<PathBuf> {
    items
        .iter()
        .flat_map(|i| i.available_actions.iter().flat_map(action_paths))
        .collect()
}

fn snapshot(paths: &[PathBuf]) -> HashMap<PathBuf, Vec<u8>> {
    paths
        .iter()
        .map(|p| (p.clone(), std::fs::read(p).unwrap()))
        .collect()
}

fn executor(home: &FakeHome, trash: &Path) -> ActionExecutor {
    ActionExecutor::with_trash(
        SafetyChecker::with_home(home.root().to_path_buf(), vec![]),
        TrashBackend::Directory(trash.to_path_buf()),
    )
}

async fn run(exec: ActionExecutor, pairs: Vec<(CleanableItem, CleanAction)>) -> Vec<ActionEvent> {
    let mut rx = exec.execute_batch(pairs);
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    events
}

fn failures(events: &[ActionEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, ActionEvent::Failed { .. }))
        .count()
}

fn detail<'a>(item: &'a CleanableItem, label: &str) -> Option<&'a str> {
    item.details
        .iter()
        .find(|d| d.label == label)
        .map(|d| d.value.as_str())
}

#[tokio::test]
async fn claude_categories_are_found_with_the_right_kind_agent_and_risk() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fx = claude_home(&home, Some(14), 2 * MB);

    let items = scan(&scanner(&home)).await;
    assert!(!items.is_empty());
    for item in &items {
        assert_eq!(item.ecosystem, Ecosystem::AgentData);
        assert_eq!(item.agent.as_deref(), Some("claude"));
        assert!(item.path.starts_with(&fx.root));
        assert!(item.size_bytes > 0);
        let action = &item.available_actions[0];
        assert_eq!(action.estimated_savings_bytes, item.size_bytes);
        assert_eq!(action.risk, item.risk);
    }

    let transcripts: Vec<_> = items
        .iter()
        .filter(|i| i.kind == ArtifactKind::AgentTranscripts)
        .collect();
    assert_eq!(transcripts.len(), 2, "session files + subagent sidecars");
    for t in &transcripts {
        assert_eq!(t.risk, RiskLevel::Caution);
        assert_eq!(t.project_name.as_deref(), Some("~/code/my-app"));
        assert_eq!(t.project_root.as_deref(), Some(fx.project_path.as_path()));
    }
    let files_item = transcripts
        .iter()
        .find(|i| {
            matches!(
                i.available_actions[0].method,
                ActionMethod::RemoveFiles { .. }
            )
        })
        .unwrap();
    let mut offered = action_paths(&files_item.available_actions[0]);
    offered.sort();
    let mut expected = fx.old_transcripts.clone();
    expected.sort();
    assert_eq!(offered, expected);
    assert_eq!(
        detail(files_item, "Claude Code cleanupPeriodDays"),
        Some("14")
    );
    assert!(detail(files_item, "Warning").is_none());
    assert!(detail(files_item, "Large session")
        .unwrap()
        .contains(deepclean_core::testkit::agents::BIG_SESSION));

    let sidecar_item = transcripts
        .iter()
        .find(|i| {
            matches!(
                i.available_actions[0].method,
                ActionMethod::RemoveDirs { .. }
            )
        })
        .unwrap();
    let mut offered = action_paths(&sidecar_item.available_actions[0]);
    offered.sort();
    let mut expected = fx.old_sidecars.clone();
    expected.sort();
    assert_eq!(
        offered, expected,
        "old + orphaned sidecars, not the recent one"
    );

    let by_kind = |k| items.iter().filter(move |i: &&CleanableItem| i.kind == k);
    let fh: Vec<_> = by_kind(ArtifactKind::AgentFileHistory).collect();
    assert_eq!(fh.len(), 1);
    assert_eq!(fh[0].risk, RiskLevel::Caution);
    assert_eq!(
        action_paths(&fh[0].available_actions[0]),
        vec![fx.old_file_history.clone()]
    );

    let debug: Vec<_> = by_kind(ArtifactKind::AgentDebugLogs).collect();
    assert_eq!(debug.len(), 1);
    assert_eq!(debug[0].risk, RiskLevel::Safe);
    assert_eq!(
        action_paths(&debug[0].available_actions[0]),
        vec![fx.old_debug.clone()]
    );

    let caches: Vec<_> = by_kind(ArtifactKind::AgentCache).collect();
    assert!(caches.iter().all(|c| c.risk == RiskLevel::Safe));
    let mut cache_paths: Vec<_> = caches
        .iter()
        .flat_map(|c| action_paths(&c.available_actions[0]))
        .collect();
    cache_paths.sort();
    let mut expected = fx.old_caches.clone();
    expected.sort();
    assert_eq!(cache_paths, expected);

    // Nothing recent or protected is ever offered.
    let offered = all_paths(&items);
    for keep in fx
        .recent_transcripts
        .iter()
        .chain([
            &fx.recent_sidecar,
            &fx.recent_file_history,
            &fx.recent_debug,
        ])
        .chain(fx.protected.iter())
    {
        assert!(
            !offered.iter().any(|p| keep.starts_with(p) || p == keep),
            "{} was offered",
            keep.display()
        );
    }
}

#[tokio::test]
async fn nothing_is_offered_when_nothing_is_old() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fx = claude_home(&home, None, 1024);
    let codex = codex_home(&home);
    age_tree(&fx.root, 1);
    age_tree(&codex.root, 1);
    // Caches wait a day; make them brand new.
    for c in &fx.old_caches {
        age_tree(c, 0);
    }

    let items = scan(&scanner(&home)).await;
    assert!(items.is_empty(), "got {items:#?}");
}

#[tokio::test]
async fn executing_the_actions_removes_only_old_generated_data() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fx = claude_home(&home, None, 2 * MB);
    let codex = codex_home(&home);

    let must_survive: Vec<PathBuf> = fx
        .protected
        .iter()
        .chain(codex.protected.iter())
        .chain(fx.recent_transcripts.iter())
        .chain(codex.recent_sessions.iter())
        .chain([&fx.recent_debug])
        .cloned()
        .chain([
            fx.recent_sidecar.join("subagents/agent-c.jsonl"),
            fx.recent_file_history.join("def@v1"),
        ])
        .collect();
    let before = snapshot(&must_survive);

    let items = scan(&scanner(&home)).await;
    let removed = all_paths(&items);
    assert!(!removed.is_empty());
    let pairs: Vec<_> = items
        .into_iter()
        .map(|i| {
            let a = i.available_actions[0].clone();
            (i, a)
        })
        .collect();
    let events = run(executor(&home, &tmp.path().join("trash")), pairs).await;
    assert_eq!(failures(&events), 0, "{events:#?}");

    for p in &removed {
        assert!(!p.exists(), "{} should be gone", p.display());
    }
    for p in fx
        .old_transcripts
        .iter()
        .chain(fx.old_sidecars.iter())
        .chain(codex.old_sessions.iter())
        .chain(codex.old_archived.iter())
        .chain([&fx.old_debug, &fx.old_file_history])
    {
        assert!(!p.exists(), "{} should be gone", p.display());
    }
    assert_eq!(snapshot(&must_survive), before, "survivors changed");
    // The containers themselves stay: Claude Code expects them.
    assert!(fx.project_dir.is_dir());
    assert!(fx.root.join("debug").is_dir());
}

#[tokio::test]
async fn hand_crafted_deletes_of_protected_files_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fx = claude_home(&home, None, 1024);
    let codex = codex_home(&home);

    let settings = fx.root.join("settings.json");
    let auth = codex.root.join("auth.json");
    let before = snapshot(&[settings.clone(), auth.clone(), fx.old_debug.clone()]);

    let mut pairs = Vec::new();
    for paths in [
        vec![fx.old_debug.clone(), settings.clone()],
        vec![auth.clone()],
        vec![home.path(".claude.json")],
        vec![home.path(".claude/history.jsonl")],
    ] {
        let action = CleanAction {
            id: uuid::Uuid::new_v4(),
            label: "evil".into(),
            description: String::new(),
            method: ActionMethod::RemoveFiles { paths },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: 0,
        };
        let item = CleanableItem {
            id: uuid::Uuid::new_v4(),
            path: fx.root.clone(),
            ecosystem: Ecosystem::AgentData,
            kind: ArtifactKind::AgentCache,
            risk: RiskLevel::Safe,
            size_bytes: 0,
            size_display: "0 B".into(),
            last_modified: None,
            days_stale: None,
            project_name: None,
            project_root: None,
            available_actions: vec![action.clone()],
            details: vec![],
            agent: Some("claude".into()),
        };
        pairs.push((item, action));
    }
    let events = run(executor(&home, &tmp.path().join("trash")), pairs).await;
    assert_eq!(failures(&events), 4, "{events:#?}");
    // The whole action is refused up front, so even the harmless debug log
    // listed beside settings.json survives.
    assert_eq!(snapshot(&[settings, auth, fx.old_debug.clone()]), before);
    assert!(home.path(".claude.json").exists());
    assert!(home.path(".claude/history.jsonl").exists());
}

#[tokio::test]
async fn cleanup_period_zero_warns_and_missing_notes_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("zero"));
    claude_home(&home, Some(0), 1024);
    let items = scan(&scanner(&home)).await;
    let t = items
        .iter()
        .find(|i| {
            i.kind == ArtifactKind::AgentTranscripts
                && matches!(
                    i.available_actions[0].method,
                    ActionMethod::RemoveFiles { .. }
                )
        })
        .unwrap();
    assert!(detail(t, "Warning")
        .unwrap()
        .contains("cleanupPeriodDays is 0"));
    // Small transcripts are not flagged as large.
    assert!(detail(t, "Large session").is_none());

    let home = FakeHome::at(tmp.path().join("unset"));
    claude_home(&home, None, 1024);
    let items = scan(&scanner(&home)).await;
    let t = items
        .iter()
        .find(|i| i.kind == ArtifactKind::AgentTranscripts)
        .unwrap();
    assert!(detail(t, "Claude Code cleanupPeriodDays")
        .unwrap()
        .contains("default"));
    assert!(detail(t, "Warning").is_none());
}

#[tokio::test]
async fn codex_old_sessions_and_archives_are_separate_items() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fx = codex_home(&home);
    let items = scan(&scanner(&home)).await;
    assert_eq!(items.len(), 2, "{items:#?}");
    for i in &items {
        assert_eq!(i.agent.as_deref(), Some("codex"));
        assert_eq!(i.kind, ArtifactKind::AgentTranscripts);
        assert_eq!(i.risk, RiskLevel::Caution);
    }
    let archived = items
        .iter()
        .find(|i| detail(i, "Archived").is_some())
        .unwrap();
    assert_eq!(
        action_paths(&archived.available_actions[0]),
        fx.old_archived
    );
    let live = items
        .iter()
        .find(|i| detail(i, "Archived").is_none())
        .unwrap();
    assert_eq!(action_paths(&live.available_actions[0]), fx.old_sessions);
    let offered = all_paths(&items);
    for keep in fx.recent_sessions.iter().chain(fx.protected.iter()) {
        assert!(!offered.contains(keep));
    }
}

#[tokio::test]
async fn cursor_state_db_is_only_ever_vacuumed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let fx = editor_user(&home, "Cursor", 64 * 1024);

    let items = scan(&scanner(&home).with_state_db_threshold(32 * 1024)).await;
    for i in &items {
        assert_eq!(i.agent.as_deref(), Some("cursor"));
        assert_eq!(i.ecosystem, Ecosystem::AgentData);
    }

    let vacuum = items
        .iter()
        .find(|i| i.kind == ArtifactKind::EditorStateDb && i.path == fx.state_db)
        .expect("vacuum item");
    assert_eq!(vacuum.risk, RiskLevel::Caution);
    assert_eq!(vacuum.available_actions.len(), 1);
    let action = &vacuum.available_actions[0];
    assert_eq!(action.estimated_savings_bytes, 0);
    match &action.method {
        ActionMethod::Command { program, args, .. } => {
            assert_eq!(program, "sqlite3");
            assert_eq!(
                args,
                &vec![fx.state_db.to_string_lossy().to_string(), "VACUUM;".into()]
            );
        }
        other => panic!("expected a sqlite3 command, got {other:?}"),
    }
    assert!(detail(vacuum, "Before you run it")
        .unwrap()
        .contains("Quit Cursor"));
    assert!(
        !all_paths(&items).contains(&fx.state_db),
        "state.vscdb must never be deleted"
    );

    let backup = items.iter().find(|i| i.path == fx.backup).unwrap();
    assert_eq!(backup.kind, ArtifactKind::EditorStateDb);
    assert_eq!(backup.risk, RiskLevel::Caution);

    let ws: Vec<_> = items
        .iter()
        .filter(|i| i.kind == ArtifactKind::EditorWorkspaceStorage)
        .collect();
    assert_eq!(ws.len(), 1);
    assert_eq!(ws[0].risk, RiskLevel::Caution);
    assert_eq!(
        action_paths(&ws[0].available_actions[0]),
        vec![fx.orphan_workspace.clone()],
        "live and remote workspaces are kept"
    );

    // Executing the backup + orphan removals leaves the database and live
    // workspace alone.
    let db_before = snapshot(std::slice::from_ref(&fx.state_db));
    let pairs: Vec<_> = items
        .iter()
        .filter(|i| i.path != fx.state_db)
        .map(|i| (i.clone(), i.available_actions[0].clone()))
        .collect();
    let events = run(executor(&home, &tmp.path().join("trash")), pairs).await;
    assert_eq!(failures(&events), 0, "{events:#?}");
    assert!(!fx.backup.exists());
    assert!(!fx.orphan_workspace.exists());
    assert!(fx.live_workspace.join("workspace.json").exists());
    assert!(fx.remote_workspace.exists());
    assert_eq!(snapshot(std::slice::from_ref(&fx.state_db)), db_before);
}

#[tokio::test]
async fn small_state_db_gets_no_vacuum_offer_and_other_editors_are_labelled() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let cursor = editor_user(&home, "Cursor", 1024);
    editor_user(&home, "Code", 1024);
    editor_user(&home, "Windsurf", 1024);

    // Default 100 MB threshold.
    let items = scan(&scanner(&home)).await;
    assert!(!items.iter().any(|i| i.path == cursor.state_db));
    let mut agents: Vec<_> = items
        .iter()
        .filter(|i| i.kind == ArtifactKind::EditorWorkspaceStorage)
        .filter_map(|i| i.agent.clone())
        .collect();
    agents.sort();
    assert_eq!(agents, vec!["cursor", "vscode", "windsurf"]);
}

#[tokio::test]
async fn orchestrator_finds_agent_data_via_the_registry() {
    use deepclean_core::scanner::{registry, ScanOrchestrator};

    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    claude_home(&home, None, 1024);
    let mut config = config();
    config.enabled_ecosystems = vec![Ecosystem::AgentData];
    config.scan_roots = vec![];
    let scanners = registry::build_with_home(&config, home.root().to_path_buf());
    let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
    let mut kinds = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let ScanEvent::ItemFound { item } = ev {
            kinds.push(item.kind);
        }
    }
    assert!(kinds.contains(&ArtifactKind::AgentTranscripts));
    assert!(kinds.contains(&ArtifactKind::AgentDebugLogs));
}
