//! Agent worktree manager: real git repositories and worktrees in a fake
//! home, the real scanner, and the real executor (safety checker included)
//! acting on them. Every destructive test asserts both what went and what
//! stayed.

use std::path::{Path, PathBuf};

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::model::*;
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::worktrees::WorktreeScanner;
use deepclean_core::scanner::{registry, EcosystemScanner, ScanOrchestrator};
use deepclean_core::testkit::worktrees as wt;
use deepclean_core::testkit::{git, FakeHome};
use deepclean_core::trash::TrashBackend;

struct Env {
    _tmp: tempfile::TempDir,
    home: FakeHome,
    repo: PathBuf,
    trash: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let repo = wt::repo(&home.path("code/webapp"));
    let trash = tmp.path().join("trash");
    Env {
        home,
        repo,
        trash,
        _tmp: tmp,
    }
}

fn scanner(home: &FakeHome) -> WorktreeScanner {
    WorktreeScanner::new(home.root().to_path_buf(), &AppConfig::default())
}

/// What the orchestrator would do: the repo's `.claude/worktrees` (a walk
/// candidate) plus every global agent root.
async fn scan(env: &Env) -> Vec<CleanableItem> {
    let s = scanner(&env.home);
    let mut items = Vec::new();
    let claude_root = env.repo.join(".claude/worktrees");
    if claude_root.is_dir() {
        assert!(s.is_candidate("worktrees", &claude_root));
        items.extend(s.analyze_many(&claude_root).await.unwrap());
    }
    for loc in s.global_locations() {
        items.extend(s.analyze_many(&loc).await.unwrap());
    }
    items
}

fn find<'a>(items: &'a [CleanableItem], path: &Path) -> &'a CleanableItem {
    items
        .iter()
        .find(|i| i.path == path)
        .unwrap_or_else(|| panic!("no item for {}", path.display()))
}

fn of_kind(items: &[CleanableItem], kind: ArtifactKind) -> Vec<&CleanableItem> {
    items.iter().filter(|i| i.kind == kind).collect()
}

fn detail<'a>(item: &'a CleanableItem, label: &str) -> &'a str {
    item.details
        .iter()
        .find(|d| d.label == label)
        .map(|d| d.value.as_str())
        .unwrap_or_else(|| panic!("no detail {label:?} in {:?}", item.details))
}

fn remove_action(item: &CleanableItem) -> Option<&CleanAction> {
    item.available_actions
        .iter()
        .find(|a| matches!(a.method, ActionMethod::GitWorktreeRemove { .. }))
}

fn trim_action(item: &CleanableItem) -> Option<&CleanAction> {
    item.available_actions
        .iter()
        .find(|a| matches!(a.method, ActionMethod::RemoveDirs { .. }))
}

fn executor(env: &Env) -> ActionExecutor {
    ActionExecutor::with_trash(
        SafetyChecker::with_home(env.home.root().to_path_buf(), vec![]),
        TrashBackend::Directory(env.trash.clone()),
    )
}

/// Run one action; `Ok(bytes)` on success, `Err(message)` on failure.
async fn run(env: &Env, item: &CleanableItem, action: &CleanAction) -> Result<u64, String> {
    let mut rx = executor(env).execute_batch(vec![(item.clone(), action.clone())]);
    let mut outcome = Err("no terminal event".to_string());
    while let Some(event) = rx.recv().await {
        match event {
            ActionEvent::Completed { bytes_freed, .. } => outcome = Ok(bytes_freed),
            ActionEvent::Failed { error, .. } => outcome = Err(error),
            _ => {}
        }
    }
    outcome
}

fn worktree_listed(repo: &Path, path: &Path) -> bool {
    let out = git(repo, &["worktree", "list", "--porcelain"]);
    out.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| {
            Path::new(p)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(p))
                == path
        })
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    !git(repo, &["branch", "--list", branch]).trim().is_empty()
}

#[tokio::test]
async fn clean_merged_worktree_is_safe_to_remove_with_its_branch() {
    let env = env();
    let w = wt::clean_merged(&env.repo, "fix-login");
    wt::add_artifacts(&w);
    let items = scan(&env).await;
    let item = find(&items, &w);

    assert_eq!(item.kind, ArtifactKind::AgentWorktree);
    assert_eq!(item.agent.as_deref(), Some("claude"));
    assert_eq!(item.risk, RiskLevel::Safe);
    assert_eq!(detail(item, "Agent"), "Claude Code");
    assert_eq!(detail(item, "Branch"), "worktree-fix-login");
    assert_eq!(detail(item, "Uncommitted changes"), "none");
    assert_eq!(detail(item, "Unpushed commits"), "0");
    assert_eq!(detail(item, "Merged into main"), "yes");
    assert_eq!(detail(item, "Locked"), "no");
    assert!(
        item.size_bytes > 12_000,
        "full dir size, got {}",
        item.size_bytes
    );

    // Lowest risk first: trim, then remove.
    let actions = &item.available_actions;
    assert!(matches!(actions[0].method, ActionMethod::RemoveDirs { .. }));
    let remove = remove_action(item).unwrap();
    assert_eq!(remove.risk, RiskLevel::Safe);
    match &remove.method {
        ActionMethod::GitWorktreeRemove {
            repo,
            worktree,
            delete_branch,
        } => {
            assert_eq!(repo, &env.repo);
            assert_eq!(worktree, &w);
            assert_eq!(delete_branch.as_deref(), Some("worktree-fix-login"));
        }
        _ => unreachable!(),
    }

    let freed = run(&env, item, remove).await.unwrap();
    assert!(freed > 0);
    assert!(!w.exists(), "worktree directory is gone");
    assert!(!worktree_listed(&env.repo, &w), "git no longer lists it");
    assert!(
        !branch_exists(&env.repo, "worktree-fix-login"),
        "merged branch deleted"
    );
    // The merged work itself is safe on main.
    assert!(env.repo.join("fix-login.txt").exists());
    assert!(env.repo.join("README.md").exists());
}

#[tokio::test]
async fn pushed_but_unmerged_worktree_is_removable_with_caution_and_keeps_branch() {
    let env = env();
    let w = wt::clean_unmerged_pushed(&env.repo, "add-search");
    let items = scan(&env).await;
    let item = find(&items, &w);
    assert_eq!(detail(item, "Merged into main"), "no");
    assert_eq!(detail(item, "Unpushed commits"), "0");
    let remove = remove_action(item).expect("pushed work may be removed");
    assert_eq!(remove.risk, RiskLevel::Caution);
    assert!(matches!(
        &remove.method,
        ActionMethod::GitWorktreeRemove {
            delete_branch: None,
            ..
        }
    ));

    run(&env, item, remove).await.unwrap();
    assert!(!w.exists());
    assert!(
        branch_exists(&env.repo, "worktree-add-search"),
        "branch kept"
    );
}

#[tokio::test]
async fn dirty_untracked_unpushed_and_locked_worktrees_are_never_offered_removal() {
    let env = env();
    let dirty = wt::dirty(&env.repo, "dark-mode");
    wt::add_artifacts(&dirty);
    let untracked = wt::untracked(&env.repo, "notes");
    let unpushed = wt::unpushed(&env.repo, "refactor-db");
    let locked = wt::locked(&env.repo, "running");
    wt::add_artifacts(&locked);
    let items = scan(&env).await;

    for (path, why) in [
        (&dirty, "uncommitted changes (1 modified)"),
        (&untracked, "uncommitted changes (1 untracked)"),
        (&unpushed, "1 commit not pushed or merged anywhere"),
    ] {
        let item = find(&items, path);
        assert_eq!(item.kind, ArtifactKind::AgentWorktree);
        assert!(
            remove_action(item).is_none(),
            "{} offered removal",
            path.display()
        );
        assert!(
            detail(item, "Why it is kept").contains(why),
            "{:?}",
            item.details
        );
    }
    assert_eq!(
        detail(find(&items, &dirty), "Uncommitted changes"),
        "1 modified"
    );
    assert_eq!(detail(find(&items, &unpushed), "Unpushed commits"), "1");
    // A dirty worktree may still have its build artifacts trimmed.
    let dirty_item = find(&items, &dirty);
    assert_eq!(trim_action(dirty_item).unwrap().risk, RiskLevel::Safe);
    assert_eq!(dirty_item.risk, RiskLevel::Safe);

    // Locked: listed for visibility, nothing offered at all.
    let locked_item = find(&items, &locked);
    assert!(locked_item.available_actions.is_empty());
    assert!(detail(locked_item, "Locked").starts_with("yes — an agent is using it"));
    assert!(locked_item.size_bytes > 0);
}

#[tokio::test]
async fn trim_removes_artifacts_but_keeps_checkout_changes_and_branch() {
    let env = env();
    let w = wt::dirty(&env.repo, "dark-mode");
    wt::add_artifacts(&w);
    let items = scan(&env).await;
    let item = find(&items, &w);
    let trim = trim_action(item).unwrap();
    match &trim.method {
        ActionMethod::RemoveDirs { paths } => {
            assert_eq!(paths, &vec![w.join("node_modules"), w.join("target")]);
        }
        _ => unreachable!(),
    }
    assert!(trim.estimated_savings_bytes >= 12_800);

    run(&env, item, trim).await.unwrap();
    assert!(!w.join("node_modules").exists());
    assert!(!w.join("target").exists());
    assert!(w.join("package.json").exists());
    assert_eq!(
        std::fs::read_to_string(w.join("README.md")).unwrap(),
        "# edited by an agent\n",
        "the agent's uncommitted edit survives"
    );
    assert!(worktree_listed(&env.repo, &w));
    assert!(branch_exists(&env.repo, "worktree-dark-mode"));
}

#[tokio::test]
async fn removal_rechecks_at_execution_time() {
    let env = env();
    let w = wt::clean_merged(&env.repo, "race");
    let items = scan(&env).await;
    let item = find(&items, &w);
    let remove = remove_action(item).unwrap().clone();

    // An agent edits a file after the scan.
    std::fs::write(w.join("README.md"), "# late edit\n").unwrap();

    let err = run(&env, item, &remove).await.unwrap_err();
    assert!(err.contains("uncommitted changes"), "{err}");
    assert!(w.exists());
    assert_eq!(
        std::fs::read_to_string(w.join("README.md")).unwrap(),
        "# late edit\n"
    );
    assert!(worktree_listed(&env.repo, &w));
    assert!(branch_exists(&env.repo, "worktree-race"));
}

#[tokio::test]
async fn removal_refuses_a_worktree_locked_after_the_scan() {
    let env = env();
    let w = wt::clean_merged(&env.repo, "late-lock");
    let items = scan(&env).await;
    let item = find(&items, &w);
    let remove = remove_action(item).unwrap().clone();
    git(&env.repo, &["worktree", "lock", &w.to_string_lossy()]);

    let err = run(&env, item, &remove).await.unwrap_err();
    assert!(err.contains("locked"), "{err}");
    assert!(w.exists() && worktree_listed(&env.repo, &w));
}

#[tokio::test]
async fn removal_refuses_a_commit_made_after_the_scan() {
    let env = env();
    let w = wt::clean_merged(&env.repo, "late-commit");
    let items = scan(&env).await;
    let item = find(&items, &w);
    let remove = remove_action(item).unwrap().clone();
    wt::commit_in(&w, "more.txt");

    let err = run(&env, item, &remove).await.unwrap_err();
    assert!(err.contains("exist nowhere else"), "{err}");
    assert!(w.join("more.txt").exists());
    assert!(branch_exists(&env.repo, "worktree-late-commit"));
}

#[tokio::test]
async fn worktree_with_env_file_cannot_be_removed_but_can_be_trimmed() {
    let env = env();
    let w = wt::clean_merged(&env.repo, "with-env");
    wt::add_env(&w);
    wt::add_artifacts(&w);
    let items = scan(&env).await;
    let item = find(&items, &w);
    assert!(
        remove_action(item).is_none(),
        "scanner does not offer a doomed removal"
    );
    assert!(detail(item, "Why it is kept").contains("protected files"));

    // Even a hand-built removal is refused by the executor's safety gate.
    let forced = CleanAction {
        id: uuid::Uuid::new_v4(),
        label: "Remove worktree".into(),
        description: String::new(),
        method: ActionMethod::GitWorktreeRemove {
            repo: env.repo.clone(),
            worktree: w.clone(),
            delete_branch: None,
        },
        risk: RiskLevel::Safe,
        estimated_savings_bytes: 0,
    };
    assert!(run(&env, item, &forced).await.is_err());
    assert!(w.join(".env").exists());
    assert!(worktree_listed(&env.repo, &w));

    run(&env, item, trim_action(item).unwrap()).await.unwrap();
    assert!(!w.join("node_modules").exists());
    assert!(w.join(".env").exists(), "credentials survive trimming");
}

#[tokio::test]
async fn orphans_are_detected_and_go_to_the_trash_first() {
    let env = env();
    let forgotten = wt::orphan_forgotten(&env.repo, "forgotten");
    let lost = wt::orphan_missing_repo(&env.home.path(".codex/worktrees/a1b2/api"));
    let items = scan(&env).await;

    let f = find(&items, &forgotten);
    assert_eq!(f.kind, ArtifactKind::OrphanWorktree);
    assert_eq!(f.agent.as_deref(), Some("claude"));
    assert!(detail(f, "Orphaned because").contains("no record"));
    let l = find(&items, &lost);
    assert_eq!(l.kind, ArtifactKind::OrphanWorktree);
    assert_eq!(l.agent.as_deref(), Some("codex"));

    for item in [f, l] {
        assert_eq!(item.risk, RiskLevel::Caution);
        let first = &item.available_actions[0];
        assert!(matches!(first.method, ActionMethod::MoveToTrash { .. }));
        assert_eq!(first.risk, RiskLevel::Caution);
        assert!(item
            .available_actions
            .iter()
            .any(|a| matches!(a.method, ActionMethod::RemoveDir { .. })
                && a.risk == RiskLevel::Danger));
        assert!(remove_action(item).is_none());
    }

    run(&env, f, &f.available_actions[0].clone()).await.unwrap();
    assert!(!forgotten.exists());
    let trashed: Vec<_> = std::fs::read_dir(&env.trash).unwrap().flatten().collect();
    assert_eq!(trashed.len(), 1);
    assert!(
        trashed[0].path().join("scratch.txt").exists(),
        "recoverable from the trash"
    );
    assert!(env.repo.join("README.md").exists(), "main repo untouched");
}

#[tokio::test]
async fn prune_removes_only_the_stale_admin_entry() {
    let env = env();
    let gone = wt::prunable(&env.repo, "deleted-by-hand");
    let live = wt::clean_merged(&env.repo, "live");
    let items = scan(&env).await;

    let prunable = of_kind(&items, ArtifactKind::PrunableWorktreeRefs);
    assert_eq!(prunable.len(), 1);
    let item = prunable[0];
    assert_eq!(item.risk, RiskLevel::Safe);
    assert!(detail(item, "Stale entries").contains("deleted-by-hand"));
    assert!(
        worktree_listed(&env.repo, &gone) || {
            // The path no longer exists, so compare by name.
            git(&env.repo, &["worktree", "list"]).contains("deleted-by-hand")
        }
    );

    run(&env, item, &item.available_actions[0]).await.unwrap();
    assert!(!git(&env.repo, &["worktree", "list"]).contains("deleted-by-hand"));
    assert!(
        live.exists() && worktree_listed(&env.repo, &live),
        "live worktree kept"
    );
    assert!(env.repo.join("README.md").exists());
}

#[tokio::test]
async fn merged_branch_deletion_leaves_unmerged_and_checked_out_branches() {
    let env = env();
    let (merged, unmerged) =
        wt::agent_branches(&env.repo, "claude/old-experiment", "claude/unfinished");
    git(&env.repo, &["branch", "feature/mine", "main"]); // not an agent prefix
    let live = wt::claude_worktree(&env.repo, "live"); // merged but checked out
    let items = scan(&env).await;

    let found = of_kind(&items, ArtifactKind::MergedAgentBranches);
    assert_eq!(found.len(), 1);
    let item = found[0];
    let action = &item.available_actions[0];
    match &action.method {
        ActionMethod::GitDeleteBranches { branches, .. } => {
            assert_eq!(branches, &vec![merged.clone()]);
        }
        other => panic!("unexpected {other:?}"),
    }

    run(&env, item, action).await.unwrap();
    assert!(!branch_exists(&env.repo, &merged));
    assert!(branch_exists(&env.repo, &unmerged), "unmerged work kept");
    assert!(branch_exists(&env.repo, "feature/mine"));
    assert!(branch_exists(&env.repo, "worktree-live"));
    assert!(branch_exists(&env.repo, "main"));
    assert!(live.exists());
}

#[tokio::test]
async fn branch_deletion_refuses_unmerged_default_and_checked_out() {
    let env = env();
    let (_, unmerged) = wt::agent_branches(&env.repo, "claude/a", "claude/b");
    wt::claude_worktree(&env.repo, "busy");
    let err = deepclean_core::git::delete_merged_branches(
        &env.repo,
        &[unmerged.clone(), "main".into(), "worktree-busy".into()],
    )
    .await
    .unwrap_err();
    assert!(err.contains("no branch was deleted"), "{err}");
    assert!(branch_exists(&env.repo, &unmerged));
    assert!(branch_exists(&env.repo, "main"));
    assert!(branch_exists(&env.repo, "worktree-busy"));
}

#[tokio::test]
async fn cursor_worktrees_under_home_are_found() {
    let env = env();
    let c = wt::cursor_worktree(&env.home, &env.repo, "k3j2");
    wt::add_artifacts(&c);
    let items = scan(&env).await;
    let item = find(&items, &c);
    assert_eq!(item.kind, ArtifactKind::AgentWorktree);
    assert_eq!(item.agent.as_deref(), Some("cursor"));
    assert_eq!(detail(item, "Agent"), "Cursor");
    assert_eq!(detail(item, "Branch"), "cursor/k3j2");
    assert_eq!(item.project_root.as_deref(), Some(env.repo.as_path()));
    assert!(remove_action(item).is_some());
}

#[tokio::test]
async fn seed_builds_every_variant() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    wt::seed(&home);
    let s = scanner(&home);
    let mut items = Vec::new();
    items.extend(
        s.analyze_many(&home.path("code/webapp/.claude/worktrees"))
            .await
            .unwrap(),
    );
    for loc in s.global_locations() {
        items.extend(s.analyze_many(&loc).await.unwrap());
    }
    for kind in [
        ArtifactKind::AgentWorktree,
        ArtifactKind::OrphanWorktree,
        ArtifactKind::PrunableWorktreeRefs,
        ArtifactKind::MergedAgentBranches,
    ] {
        assert!(!of_kind(&items, kind).is_empty(), "seed lacks {kind:?}");
    }
    assert!(items.iter().any(|i| i.agent.as_deref() == Some("cursor")));
    assert!(items.iter().any(|i| i.agent.as_deref() == Some("codex")));
}

#[tokio::test]
async fn orchestrator_gives_worktree_node_modules_to_the_worktree_scanner() {
    let env = env();
    let w = wt::clean_merged(&env.repo, "x");
    wt::add_artifacts(&w);
    env.home
        .sized_file("code/webapp/node_modules/react/index.js", 2048);

    let config = AppConfig {
        scan_roots: vec![env.home.root().to_path_buf()],
        enabled_ecosystems: vec![Ecosystem::Worktrees, Ecosystem::Node],
        ..Default::default()
    };
    let scanners = registry::build_with_home(&config, env.home.root().to_path_buf());
    let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
    let mut items = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            ScanEvent::ItemFound { item } => items.push(item),
            ScanEvent::ScanComplete { .. } => break,
            _ => {}
        }
    }

    let node: Vec<&CleanableItem> = items
        .iter()
        .filter(|i| i.kind == ArtifactKind::NodeModules)
        .collect();
    assert_eq!(
        node.len(),
        1,
        "{:?}",
        node.iter().map(|i| &i.path).collect::<Vec<_>>()
    );
    assert_eq!(node[0].path, env.repo.join("node_modules"));
    assert!(!items
        .iter()
        .any(|i| i.path.starts_with(&w) && i.kind != ArtifactKind::AgentWorktree));

    let worktrees: Vec<&CleanableItem> = items
        .iter()
        .filter(|i| i.kind == ArtifactKind::AgentWorktree)
        .collect();
    assert_eq!(worktrees.len(), 1, "analyzed once, not per walk + global");
    assert_eq!(worktrees[0].path, w);
}
