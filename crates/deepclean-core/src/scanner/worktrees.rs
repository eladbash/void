//! Git worktrees created by AI coding agents, orphaned worktrees, and merged agent branches.
//!
//! Agents create a worktree per session, and each one grows its own
//! `node_modules`, `.venv` and `target`. Twenty of them in a week is ordinary.
//! This scanner lists every worktree under the known agent roots with the
//! facts needed to decide — branch, dirty state, unpushed commits, merged or
//! not, locked or not — and offers, lowest risk first:
//!
//! 1. trimming the regenerable build artifacts while keeping the checkout;
//! 2. `git worktree remove`, only when git state proves nothing would be lost.
//!
//! It also reports worktree directories whose repository forgot them
//! (orphans), stale admin entries (`git worktree prune`), and merged branches
//! agents left behind.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use bytesize::ByteSize;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::error::ScanError;
use crate::git;
use crate::model::*;
use crate::safety::SafetyChecker;
use crate::scanner::EcosystemScanner;
use crate::staleness;

/// Agent worktree roots relative to home, with the agent that owns them.
const HOME_ROOTS: &[(&str, &str)] = &[
    (".cursor/worktrees", "cursor"),
    (".codex/worktrees", "codex"),
    ("conductor/workspaces", "conductor"),
];

/// Branch prefixes agents use; merged local branches with these are leftovers.
pub const AGENT_BRANCH_PREFIXES: &[&str] =
    &["worktree-", "claude/", "cursor/", "codex/", "conductor/"];

/// Upper bound on worktrees analysed per root, so a pathological directory
/// cannot turn one scan into thousands of git invocations.
const MAX_WORKTREES_PER_ROOT: usize = 256;

/// How deep the build-artifact walk goes inside a worktree.
const ARTIFACT_MAX_DEPTH: usize = 6;

/// Entries the build-artifact walk visits at most per worktree.
const ARTIFACT_MAX_ENTRIES: usize = 200_000;

pub struct WorktreeScanner {
    home: PathBuf,
    config: AppConfig,
    /// Known agent roots (absolute) and the agent id each belongs to.
    roots: Vec<(PathBuf, String)>,
    /// Repositories whose repo-level items (prunable refs, merged branches)
    /// were already emitted this scan — several roots can share one repo.
    repos_reported: Arc<Mutex<HashSet<PathBuf>>>,
}

impl WorktreeScanner {
    /// A scanner rooted at `home` — the real home in the app, a temp dir in
    /// tests and the sandbox.
    pub fn new(home: PathBuf, config: &AppConfig) -> Self {
        let mut roots: Vec<(PathBuf, String)> = HOME_ROOTS
            .iter()
            .map(|(rel, agent)| (home.join(rel), (*agent).to_string()))
            .collect();
        for extra in &config.ai.extra_worktree_roots {
            roots.push((extra.clone(), agent_from_path(extra)));
        }
        Self {
            home,
            config: config.clone(),
            roots,
            repos_reported: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// The agent owning a root, from its location.
    fn agent_for_root(&self, root: &Path) -> String {
        if is_claude_root(root) {
            return "claude".into();
        }
        self.roots
            .iter()
            .find(|(r, _)| paths_equal(r, root))
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| agent_from_path(root))
    }
}

/// `<repo>/.claude/worktrees`.
fn is_claude_root(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "worktrees")
        && path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == ".claude")
}

/// Best guess of the agent behind a user-configured root.
fn agent_from_path(path: &Path) -> String {
    let lower = path.to_string_lossy().to_lowercase();
    for agent in ["claude", "cursor", "codex", "conductor", "windsurf"] {
        if lower.contains(agent) {
            return agent.into();
        }
    }
    "other".into()
}

fn agent_display(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "cursor" => "Cursor",
        "codex" => "Codex",
        "conductor" => "Conductor",
        "windsurf" => "Windsurf",
        _ => "AI agent",
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    canonical(a) == canonical(b)
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// A linked worktree (or a remnant of one) has a `.git` *file*.
fn has_git_file(dir: &Path) -> bool {
    dir.join(".git").is_file()
}

/// Worktree directories under `root`, at depth 1 or 2 — Cursor and Conductor
/// nest one level per repository.
fn find_worktree_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    children.sort();
    for child in children {
        if found.len() >= MAX_WORKTREES_PER_ROOT {
            break;
        }
        if has_git_file(&child) {
            found.push(child);
            continue;
        }
        // A full clone is someone's repository, not an agent's scratch copy.
        if child.join(".git").exists() {
            continue;
        }
        let Ok(grand) = std::fs::read_dir(&child) else {
            continue;
        };
        let mut nested: Vec<PathBuf> = grand
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .filter(|p| has_git_file(p))
            .collect();
        nested.sort();
        found.extend(nested);
    }
    found.truncate(MAX_WORKTREES_PER_ROOT);
    found
}

/// Regenerable build output inside a worktree: removing it keeps every file
/// the agent wrote, and a reinstall or rebuild brings it back.
///
/// `dist` is deliberately absent — it is sometimes committed or hand-made.
fn is_build_artifact(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let parent = dir.parent().unwrap_or(dir);
    match name {
        "node_modules" | ".next" | ".turbo" | ".nuxt" | ".svelte-kit" | ".parcel-cache"
        | "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" | ".tox" => true,
        ".venv" | "venv" => dir.join("pyvenv.cfg").exists(),
        "target" => parent.join("Cargo.toml").exists() || parent.join("pom.xml").exists(),
        "build" => parent.join("build.gradle").exists() || parent.join("build.gradle.kts").exists(),
        ".gradle" => {
            parent.join("build.gradle").exists()
                || parent.join("build.gradle.kts").exists()
                || parent.join("settings.gradle").exists()
                || parent.join("settings.gradle.kts").exists()
        }
        _ => false,
    }
}

/// Bounded walk for build artifacts; does not descend into what it finds.
fn find_build_artifacts(worktree: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(worktree.to_path_buf(), 0)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > ARTIFACT_MAX_ENTRIES {
                return found;
            }
            // Symlinked directories are not followed: an artifact reached
            // through a link is not this worktree's to delete.
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            if entry.file_name() == ".git" {
                continue;
            }
            if is_build_artifact(&path) {
                found.push(path);
            } else if depth + 1 < ARTIFACT_MAX_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found
}

fn plural(n: u32, word: &str) -> String {
    let suffix = match (n, word.ends_with("ch") || word.ends_with('s')) {
        (1, _) => "",
        (_, true) => "es",
        _ => "s",
    };
    format!("{n} {word}{suffix}")
}

fn new_action(
    label: impl Into<String>,
    description: impl Into<String>,
    method: ActionMethod,
    risk: RiskLevel,
    savings: u64,
) -> CleanAction {
    CleanAction {
        id: Uuid::new_v4(),
        label: label.into(),
        description: description.into(),
        method,
        risk,
        estimated_savings_bytes: savings,
    }
}

fn base_item(
    path: &Path,
    kind: ArtifactKind,
    risk: RiskLevel,
    size: u64,
    last_modified: Option<DateTime<Utc>>,
) -> CleanableItem {
    CleanableItem {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::Worktrees,
        kind,
        risk,
        size_bytes: size,
        size_display: ByteSize(size).to_string(),
        last_modified,
        days_stale: last_modified.map(staleness::days_since),
        project_name: None,
        project_root: None,
        available_actions: Vec::new(),
        details: Vec::new(),
        agent: None,
    }
}

fn repo_name(repo: &Path) -> String {
    repo.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| repo.display().to_string())
}

/// Everything sync the scanner needs, so it runs in one `spawn_blocking`.
struct Ctx {
    home: PathBuf,
    idle_days: u64,
}

impl Ctx {
    /// One item per worktree directory, plus repo-level items for repos not
    /// reported yet.
    fn analyze_root(
        &self,
        root: &Path,
        agent: &str,
        reported: &Mutex<HashSet<PathBuf>>,
    ) -> Vec<CleanableItem> {
        let mut items = Vec::new();
        let mut repos: Vec<PathBuf> = Vec::new();

        // A `.claude/worktrees` root sits inside its repository, which may
        // hold prunable entries even when every worktree directory is gone.
        if is_claude_root(root) {
            if let Some(repo) = root.parent().and_then(Path::parent) {
                if repo.join(".git").exists() {
                    repos.push(canonical(repo));
                }
            }
        }

        for dir in find_worktree_dirs(root) {
            let (item, repo) = self.analyze_worktree(&dir, agent);
            if let Some(item) = item {
                items.push(item);
            }
            if let Some(repo) = repo {
                if !repos.contains(&repo) {
                    repos.push(repo);
                }
            }
        }

        for repo in repos {
            let first = reported
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(repo.clone());
            if first {
                items.extend(self.repo_items(&repo, agent));
            }
        }
        items
    }

    /// Classify one directory with a `.git` file. Returns the item (if any)
    /// and the main repository, when it still exists.
    fn analyze_worktree(
        &self,
        dir: &Path,
        agent: &str,
    ) -> (Option<CleanableItem>, Option<PathBuf>) {
        let admin = git::resolve_gitdir_link(dir);
        let Some(admin) = admin.filter(|a| a.is_dir()) else {
            // `<repo>/.git/worktrees/<name>`: the repo may well still exist
            // (its entry was pruned) and deserves its repo-level items.
            let repo = git::resolve_gitdir_link(dir)
                .and_then(|a| a.parent()?.parent().map(Path::to_path_buf))
                .filter(|common| common.file_name().is_some_and(|n| n == ".git") && common.is_dir())
                .and_then(|common| common.parent().map(canonical));
            let reason = match &repo {
                Some(r) => format!(
                    "{} has no record of it any more (its admin entry is gone)",
                    r.display()
                ),
                None => "Its .git file points at a repository that no longer exists".into(),
            };
            return (
                Some(self.orphan_item(dir, agent, &reason, repo.as_deref())),
                repo,
            );
        };
        let Some(repo) = git::main_repo_of(dir) else {
            let reason = "Its repository could not be found";
            return (Some(self.orphan_item(dir, agent, reason, None)), None);
        };

        // Git unavailable or the repo unreadable: say nothing rather than
        // call a live worktree an orphan.
        let Ok(entries) = git::list_worktrees(&repo) else {
            return (None, Some(repo));
        };
        let Some(entry) = entries.iter().skip(1).find(|e| paths_equal(&e.path, dir)) else {
            let reason = format!(
                "{} no longer lists it as a worktree (admin entry {} belongs elsewhere)",
                repo.display(),
                admin.display()
            );
            return (
                Some(self.orphan_item(dir, agent, &reason, Some(&repo))),
                Some(repo),
            );
        };

        (Some(self.live_item(dir, agent, &repo, entry)), Some(repo))
    }

    fn live_item(
        &self,
        dir: &Path,
        agent: &str,
        repo: &Path,
        entry: &git::WorktreeEntry,
    ) -> CleanableItem {
        let size = staleness::compute_dir_size_sync(dir);
        let last_commit = git::last_commit_time(dir);
        let last_edit = staleness::most_recent_modification(dir);
        let last_activity = last_commit.max(last_edit);

        let status = git::status_summary(dir).ok();
        let unpushed = git::unpushed_commits(dir);
        let default = git::default_branch(repo);
        let tip = entry.branch.clone().or_else(|| entry.head.clone());
        let merged = match (&default, &tip) {
            (Some(d), Some(t)) => git::is_merged(repo, t, d),
            _ => false,
        };
        let locked = entry.locked.is_some();

        let artifacts = if locked {
            Vec::new()
        } else {
            find_build_artifacts(dir)
        };
        let artifact_bytes: u64 = artifacts
            .iter()
            .map(|p| staleness::compute_dir_size_sync(p))
            .sum();

        let mut item = base_item(
            dir,
            ArtifactKind::AgentWorktree,
            RiskLevel::Caution,
            size,
            last_activity,
        );
        item.agent = Some(agent.to_string());
        item.project_name = Some(format!(
            "{} · {}",
            repo_name(repo),
            dir.file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ));
        item.project_root = Some(repo.to_path_buf());

        let default_label = default.clone().unwrap_or_else(|| "default branch".into());
        let d = &mut item.details;
        d.push(Detail::new("Agent", agent_display(agent)));
        d.push(Detail::new("Repository", repo.display().to_string()));
        d.push(Detail::new(
            "Branch",
            entry
                .branch
                .clone()
                .unwrap_or_else(|| "detached HEAD".into()),
        ));
        d.push(Detail::new(
            "Last activity",
            match last_activity {
                Some(t) => {
                    let days = staleness::days_since(t);
                    let idle = if days >= self.idle_days {
                        " (idle)"
                    } else {
                        ""
                    };
                    format!("{} — {days} days ago{idle}", t.format("%Y-%m-%d"))
                }
                None => "unknown".into(),
            },
        ));
        d.push(Detail::new(
            "Uncommitted changes",
            status.map_or_else(|| "unknown".into(), |s| s.describe()),
        ));
        d.push(Detail::new(
            "Unpushed commits",
            unpushed.map_or_else(|| "unknown".into(), |n| n.to_string()),
        ));
        d.push(Detail::new(
            format!("Merged into {default_label}"),
            if merged { "yes" } else { "no" },
        ));
        d.push(Detail::new(
            "Locked",
            match &entry.locked {
                Some(r) if !r.is_empty() => format!("yes — an agent is using it ({r})"),
                Some(_) => "yes — an agent is using it".into(),
                None => "no".into(),
            },
        ));
        d.push(Detail::new(
            "Build artifacts",
            if locked {
                "not inspected while locked".to_string()
            } else {
                format!("{} in {} dirs", ByteSize(artifact_bytes), artifacts.len())
            },
        ));

        if locked {
            // An agent is running in it: deleting its node_modules mid-build
            // is as disruptive as deleting the checkout. List it for size
            // visibility only.
            item.details.push(Detail::new(
                "Why no action",
                "Locked — an agent is using it. Unlock or finish the session first.",
            ));
            return item;
        }

        if !artifacts.is_empty() {
            let names: Vec<String> = artifacts
                .iter()
                .filter_map(|p| p.strip_prefix(dir).ok())
                .map(|p| p.display().to_string())
                .take(6)
                .collect();
            item.available_actions.push(new_action(
                "Trim build artifacts (keep worktree)",
                format!(
                    "Delete regenerable build output ({}{}) — the checkout, its changes and its branch stay",
                    names.join(", "),
                    if artifacts.len() > names.len() { ", …" } else { "" }
                ),
                ActionMethod::RemoveDirs { paths: artifacts },
                RiskLevel::Safe,
                artifact_bytes,
            ));
        }

        let blocker = match (status, unpushed) {
            (None, _) => Some("could not read git status".to_string()),
            (Some(s), _) if !s.is_clean() => {
                Some(format!("uncommitted changes ({})", s.describe()))
            }
            (_, None) => Some("could not count unpushed commits".to_string()),
            (_, Some(n)) if n > 0 && !merged => Some(format!(
                "{} not pushed or merged anywhere",
                plural(n, "commit")
            )),
            _ => None,
        };
        let safety = SafetyChecker::with_home(self.home.clone(), vec![]);
        let blocker = blocker.or_else(|| {
            (!safety.is_path_allowed(dir)).then(|| {
                "it holds protected files (.env, credentials, CLAUDE.md…) at its root — trim instead"
                    .to_string()
            })
        });

        match blocker {
            Some(why) => item.details.push(Detail::new(
                "Why it is kept",
                format!("Not removable: {why}"),
            )),
            None => {
                let risk = if merged {
                    RiskLevel::Safe
                } else {
                    RiskLevel::Caution
                };
                let delete_branch = entry.branch.clone().filter(|_| merged);
                let description = match (&delete_branch, merged) {
                    (Some(b), _) => format!(
                        "`git worktree remove` (no --force), then `git branch -d {b}` — the branch is merged into {default_label}"
                    ),
                    (None, true) => "`git worktree remove` (no --force) — its commits are merged".into(),
                    (None, false) => format!(
                        "`git worktree remove` (no --force). The branch is kept: its commits are pushed but not merged into {default_label}"
                    ),
                };
                item.available_actions.push(new_action(
                    "Remove worktree",
                    description,
                    ActionMethod::GitWorktreeRemove {
                        repo: repo.to_path_buf(),
                        worktree: dir.to_path_buf(),
                        delete_branch,
                    },
                    risk,
                    size,
                ));
            }
        }

        item.risk = item
            .available_actions
            .iter()
            .map(|a| a.risk)
            .min()
            .unwrap_or(RiskLevel::Caution);
        item
    }

    /// A worktree directory git no longer knows. Git state cannot vouch for
    /// its contents, so the recoverable Trash comes first.
    fn orphan_item(
        &self,
        dir: &Path,
        agent: &str,
        reason: &str,
        repo: Option<&Path>,
    ) -> CleanableItem {
        let size = staleness::compute_dir_size_sync(dir);
        let mut item = base_item(
            dir,
            ArtifactKind::OrphanWorktree,
            RiskLevel::Caution,
            size,
            staleness::most_recent_modification(dir),
        );
        item.agent = Some(agent.to_string());
        item.project_name = Some(
            dir.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        item.project_root = repo.map(Path::to_path_buf);
        item.details
            .push(Detail::new("Agent", agent_display(agent)));
        item.details.push(Detail::new("Orphaned because", reason));
        item.details.push(Detail::new(
            "Uncommitted changes",
            "unknown — git cannot inspect an orphaned worktree; Move to Trash keeps it recoverable",
        ));
        item.available_actions.push(new_action(
            "Move to Trash",
            "Move the orphaned worktree directory to the Trash (recoverable)",
            ActionMethod::MoveToTrash {
                path: dir.to_path_buf(),
            },
            RiskLevel::Caution,
            // Same convention as `trash::add_trash_alternatives`: the space
            // comes back when the Trash is emptied.
            size,
        ));
        item.available_actions.push(new_action(
            "Delete permanently",
            "Delete the orphaned worktree directory — any uncommitted work in it is lost",
            ActionMethod::RemoveDir {
                path: dir.to_path_buf(),
            },
            RiskLevel::Danger,
            size,
        ));
        item
    }

    /// Prunable admin entries and merged agent branches of one repository.
    fn repo_items(&self, repo: &Path, agent: &str) -> Vec<CleanableItem> {
        let mut items = Vec::new();
        let Ok(entries) = git::list_worktrees(repo) else {
            return items;
        };

        let prunable: Vec<&git::WorktreeEntry> =
            entries.iter().filter(|e| e.prunable.is_some()).collect();
        if !prunable.is_empty() {
            let mut item = base_item(
                repo,
                ArtifactKind::PrunableWorktreeRefs,
                RiskLevel::Safe,
                0,
                None,
            );
            item.agent = Some(agent.to_string());
            item.project_name = Some(repo_name(repo));
            item.project_root = Some(repo.to_path_buf());
            item.details
                .push(Detail::new("Repository", repo.display().to_string()));
            item.details.push(Detail::new(
                "Stale entries",
                prunable
                    .iter()
                    .map(|e| e.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
            item.available_actions.push(new_action(
                "Prune worktree records",
                "`git worktree prune` — forgets worktrees whose directories are already gone; touches no files you can see",
                ActionMethod::GitWorktreePrune {
                    repo: repo.to_path_buf(),
                },
                RiskLevel::Safe,
                0,
            ));
            items.push(item);
        }

        if let Some(default) = git::default_branch(repo) {
            // Prunable entries still "check out" their branch as far as git
            // is concerned, and `merged_branches` excludes those.
            if let Ok(branches) = git::merged_branches(repo, &default, AGENT_BRANCH_PREFIXES) {
                if !branches.is_empty() {
                    let mut item = base_item(
                        repo,
                        ArtifactKind::MergedAgentBranches,
                        RiskLevel::Safe,
                        0,
                        None,
                    );
                    item.agent = Some(agent.to_string());
                    item.project_name = Some(repo_name(repo));
                    item.project_root = Some(repo.to_path_buf());
                    item.details
                        .push(Detail::new("Repository", repo.display().to_string()));
                    item.details.push(Detail::new(
                        format!("Merged into {default}"),
                        branches.join("\n"),
                    ));
                    item.available_actions.push(new_action(
                        format!("Delete {}", plural(branches.len() as u32, "merged branch")),
                        format!(
                            "`git branch -d` each — git refuses any branch not fully merged into {default}"
                        ),
                        ActionMethod::GitDeleteBranches {
                            repo: repo.to_path_buf(),
                            branches,
                        },
                        RiskLevel::Safe,
                        0,
                    ));
                    items.push(item);
                }
            }
        }
        items
    }
}

#[async_trait]
impl EcosystemScanner for WorktreeScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Worktrees
    }

    /// Claims `.claude/worktrees` and the known agent roots, so the walk
    /// does not descend into them and hand their `node_modules` to the Node
    /// scanner as if they were separate projects.
    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        if file_name == "worktrees" && is_claude_root(path) {
            return true;
        }
        self.roots.iter().any(|(root, _)| root == path)
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        Ok(self.analyze_many(path).await?.into_iter().next())
    }

    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(Vec::new());
        }
        let ctx = Ctx {
            home: self.home.clone(),
            idle_days: self.config.ai.worktree_idle_days,
        };
        let agent = self.agent_for_root(path);
        let root = path.to_path_buf();
        let reported = self.repos_reported.clone();
        Ok(
            tokio::task::spawn_blocking(move || ctx.analyze_root(&root, &agent, &reported))
                .await
                .unwrap_or_default(),
        )
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|(root, _)| root.clone())
            .filter(|root| root.is_dir())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_claude_worktrees_and_known_roots_only() {
        let s = WorktreeScanner::new(PathBuf::from("/h"), &AppConfig::default());
        assert!(s.is_candidate("worktrees", Path::new("/code/app/.claude/worktrees")));
        assert!(s.is_candidate("worktrees", Path::new("/h/.cursor/worktrees")));
        assert!(s.is_candidate("workspaces", Path::new("/h/conductor/workspaces")));
        assert!(!s.is_candidate("worktrees", Path::new("/code/app/.git/worktrees")));
        assert!(!s.is_candidate("node_modules", Path::new("/code/app/node_modules")));
    }

    #[test]
    fn extra_roots_are_claimed_with_a_guessed_agent() {
        let mut config = AppConfig::default();
        config.ai.extra_worktree_roots = vec![PathBuf::from("/x/.windsurf/worktrees")];
        let s = WorktreeScanner::new(PathBuf::from("/h"), &config);
        assert!(s.is_candidate("worktrees", Path::new("/x/.windsurf/worktrees")));
        assert_eq!(
            s.agent_for_root(Path::new("/x/.windsurf/worktrees")),
            "windsurf"
        );
        assert_eq!(s.agent_for_root(Path::new("/h/.codex/worktrees")), "codex");
        assert_eq!(
            s.agent_for_root(Path::new("/r/.claude/worktrees")),
            "claude"
        );
        assert_eq!(agent_from_path(Path::new("/opt/agents")), "other");
    }

    #[test]
    fn build_artifacts_need_their_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        std::fs::create_dir_all(wt.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(wt.join("target")).unwrap(); // no Cargo.toml
        std::fs::create_dir_all(wt.join("crates/a/target")).unwrap();
        std::fs::write(wt.join("crates/a/Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(wt.join("venv")).unwrap(); // no pyvenv.cfg
        std::fs::create_dir_all(wt.join("dist")).unwrap();
        std::fs::create_dir_all(wt.join("src/__pycache__")).unwrap();
        let found = find_build_artifacts(wt);
        assert_eq!(
            found,
            vec![
                wt.join("crates/a/target"),
                wt.join("node_modules"),
                wt.join("src/__pycache__"),
            ]
        );
    }
}
