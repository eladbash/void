//! The project graveyard: git projects with no commits and no edits in a long time.
//!
//! AI agents make it trivial to spin up a throwaway project — a prototype, a
//! spike, a "let me try this in a fresh repo". Most are never opened again,
//! and each keeps its `node_modules`, `target` or `.venv` forever. This
//! scanner lists git repositories nobody has committed to or edited for
//! `ai.stale_project_days`, and says plainly whether the work in each one
//! also exists somewhere else (a remote) before offering to remove it.
//!
//! It is purely global: it walks each scan root itself, after the main walk,
//! and never claims a directory during it — claiming a repo would hide its
//! `node_modules` and `target` from the Node and Rust scanners.

use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use bytesize::ByteSize;
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::error::ScanError;
use crate::model::*;
use crate::safety::SafetyChecker;
use crate::scanner::EcosystemScanner;
use crate::staleness;

/// How deep below a scan root a repository may sit (`~/code/org/app` is 3).
const MAX_REPO_DEPTH: usize = 4;

/// How deep inside a repository to look for build artifacts. Deep enough for
/// a monorepo's `packages/web/node_modules`, shallow enough to stay fast.
const MAX_ARTIFACT_DEPTH: usize = 3;

/// Directories never descended into while looking for repositories: they are
/// huge, and never hold a project of their own.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "Library",
    "conductor",
    "vendor",
    "Pods",
    "DerivedData",
];

/// Names of regenerable build output. Also ignored when working out when a
/// project was last *edited*: `npm install` touching `node_modules` is not
/// the user working on the project.
const ARTIFACT_NAMES: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    ".next",
    ".turbo",
    "__pycache__",
    "build",
    ".gradle",
    "DerivedData",
    "dist",
];

pub struct ProjectScanner {
    home: PathBuf,
    config: AppConfig,
}

impl ProjectScanner {
    /// A scanner rooted at `home` — the real home in the app, a temp dir in
    /// tests and the sandbox.
    pub fn new(home: PathBuf, config: &AppConfig) -> Self {
        Self {
            home,
            config: config.clone(),
        }
    }
}

#[async_trait]
impl EcosystemScanner for ProjectScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Projects
    }

    /// Never claims anything: the graveyard walks the roots itself, so the
    /// Node/Rust/Python scanners still see the artifacts inside each repo.
    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        false
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        Ok(self.analyze_many(path).await?.into_iter().next())
    }

    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        let root = path.to_path_buf();
        let home = self.home.clone();
        let config = self.config.clone();
        Ok(
            tokio::task::spawn_blocking(move || scan_root(&home, &config, &root))
                .await
                .unwrap_or_default(),
        )
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        self.config
            .scan_roots
            .iter()
            .filter(|p| p.is_dir())
            .cloned()
            .collect()
    }
}

/// Find and analyze every stale repository under `root`.
fn scan_root(home: &Path, config: &AppConfig, root: &Path) -> Vec<CleanableItem> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut repos = Vec::new();
    find_repos(root, 0, &mut repos);

    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let excluded: Vec<PathBuf> = std::iter::once(home)
        .chain(config.scan_roots.iter().map(PathBuf::as_path))
        .chain(std::iter::once(root))
        .map(canonical)
        .collect();

    let safety = SafetyChecker::with_home(home.to_path_buf(), config.blocked_paths.clone());

    repos
        .into_iter()
        // A dotfiles repo at `~` or a scan root that is itself a repo is not
        // an abandoned project; offering to trash it would be catastrophic.
        .filter(|repo| !excluded.contains(&canonical(repo)))
        .filter_map(|repo| analyze_repo(&repo, config.ai.stale_project_days, &safety))
        .collect()
}

/// Collect directories holding a `.git` *directory* (real repositories, not
/// worktrees, whose `.git` is a file). Does not descend into a repository
/// once found, except at depth 0 so a scan root that is itself a repo still
/// has its sub-projects found.
fn find_repos(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let git = dir.join(".git");
    // `symlink_metadata`: a `.git` symlink is not trusted as a repo.
    if let Ok(meta) = std::fs::symlink_metadata(&git) {
        if meta.is_dir() {
            out.push(dir.to_path_buf());
            if depth > 0 {
                return;
            }
        } else if depth > 0 {
            // A linked worktree or submodule: the worktree scanner's business.
            return;
        }
    }
    if depth >= MAX_REPO_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // `file_type` does not follow symlinks, so a link loop cannot trap us.
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Hidden directories cover `.cache`, `.Trash`, `.venv` and the agent
        // worktree roots (`.claude/worktrees`, `.cursor`, `.codex`).
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        find_repos(&entry.path(), depth + 1, out);
    }
}

/// What git knows about a repository. Every field is `None` when git could
/// not answer, which callers treat as "assume the worst".
#[derive(Debug, Default)]
struct RepoFacts {
    last_commit: Option<DateTime<Utc>>,
    remote: Option<String>,
    /// Commits on local branches that no remote-tracking ref contains.
    unpushed: Option<u64>,
    /// Changed or untracked paths, ignoring untracked build output.
    dirty: Option<u64>,
    stashes: Option<u64>,
}

fn analyze_repo(repo: &Path, stale_days: u64, safety: &SafetyChecker) -> Option<CleanableItem> {
    let facts = repo_facts(repo);
    let last_edit = last_edit_time(repo);
    let last_active = match (facts.last_commit, last_edit) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }?;
    let days = staleness::days_since(last_active);
    if days < stale_days {
        return None;
    }

    let artifacts = find_artifacts(repo);
    let artifact_sizes: Vec<(PathBuf, u64)> = artifacts
        .into_iter()
        .map(|p| {
            let size = staleness::compute_dir_size_sync(&p);
            (p, size)
        })
        .collect();
    let artifact_bytes: u64 = artifact_sizes.iter().map(|(_, s)| s).sum();
    let size_bytes = staleness::compute_dir_size_sync(repo);

    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| repo.display().to_string());

    // Only a project whose every commit is on a remote, with nothing
    // uncommitted and nothing stashed, can be trashed without losing work.
    let backed_up = facts.remote.is_some()
        && facts.unpushed == Some(0)
        && facts.dirty == Some(0)
        && facts.stashes == Some(0);

    let mut details = Vec::new();
    details.push(Detail::new(
        "Last commit",
        match facts.last_commit {
            Some(t) => format!(
                "{} ({})",
                t.format("%Y-%m-%d"),
                age_phrase(staleness::days_since(t))
            ),
            None => "none".into(),
        },
    ));
    if let Some(edit) = last_edit {
        details.push(Detail::new(
            "Last edit",
            format!(
                "{} ({})",
                edit.format("%Y-%m-%d"),
                age_phrase(staleness::days_since(edit))
            ),
        ));
    }
    details.push(Detail::new(
        "Remote",
        facts
            .remote
            .clone()
            .unwrap_or_else(|| "none — never pushed".into()),
    ));
    details.push(Detail::new(
        "Unpushed commits",
        count_or_unknown(facts.unpushed),
    ));
    details.push(Detail::new(
        "Uncommitted changes",
        match facts.dirty {
            Some(0) => "none".into(),
            Some(1) => "1 file".into(),
            Some(n) => format!("{n} files"),
            None => "unknown".into(),
        },
    ));
    if let Some(n) = facts.stashes.filter(|n| *n > 0) {
        details.push(Detail::new("Stashes", n.to_string()));
    }
    details.push(Detail::new(
        "Build artifacts",
        if artifact_sizes.is_empty() {
            "none".into()
        } else {
            ByteSize(artifact_bytes).to_string()
        },
    ));
    let markers = ai_markers(repo);
    if !markers.is_empty() {
        details.push(Detail::new(
            "AI-assisted",
            format!("yes ({})", markers.join(", ")),
        ));
    }

    let mut actions = Vec::new();
    if !artifact_sizes.is_empty() {
        let names: Vec<String> = artifact_sizes
            .iter()
            .filter_map(|(p, _)| p.strip_prefix(repo).ok())
            .map(|p| p.display().to_string())
            .collect();
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Strip build artifacts".into(),
            description: format!(
                "Delete regenerable build output ({}) and keep the source and git history.",
                names.join(", ")
            ),
            method: ActionMethod::RemoveDirs {
                paths: artifact_sizes.iter().map(|(p, _)| p.clone()).collect(),
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: artifact_bytes,
        });
    }

    // The executor refuses to trash directories holding credentials (`.env`,
    // keys); offering an action that is certain to fail is worse than saying
    // why it is missing. Agent docs (`CLAUDE.md`) do not block a Trash move.
    if safety.is_trash_allowed(repo) {
        let (risk, description) = if backed_up {
            (
                RiskLevel::Caution,
                "Move the whole project to the Trash. Every commit is on a remote and nothing is uncommitted, so it can be re-cloned.".to_string(),
            )
        } else {
            (
                RiskLevel::Danger,
                format!(
                    "Move the whole project to the Trash. It holds work that exists nowhere else ({}); recover it from the Trash if you need it.",
                    unsafe_reasons(&facts).join(", ")
                ),
            )
        };
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Move project to Trash".into(),
            description,
            method: ActionMethod::MoveToTrash {
                path: repo.to_path_buf(),
            },
            risk,
            estimated_savings_bytes: size_bytes,
        });
    } else {
        details.push(Detail::new(
            "Move to Trash",
            "unavailable — the project holds credentials (.env, keys); move it by hand",
        ));
    }

    let risk = actions.iter().map(|a| a.risk).min()?;

    Some(CleanableItem {
        id: Uuid::new_v4(),
        path: repo.to_path_buf(),
        ecosystem: Ecosystem::Projects,
        kind: ArtifactKind::StaleProject,
        risk,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified: Some(last_active),
        days_stale: Some(days),
        project_name: Some(name),
        project_root: Some(repo.to_path_buf()),
        available_actions: actions,
        details,
        agent: None,
    })
}

fn count_or_unknown(n: Option<u64>) -> String {
    n.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into())
}

fn age_phrase(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        n => format!("{n} days ago"),
    }
}

/// Why trashing this project could lose work, for the action description.
fn unsafe_reasons(facts: &RepoFacts) -> Vec<String> {
    let mut reasons = Vec::new();
    if facts.remote.is_none() {
        reasons.push("never pushed".to_string());
    }
    match facts.unpushed {
        Some(0) => {}
        Some(n) if facts.remote.is_some() => reasons.push(format!("{n} unpushed commits")),
        Some(_) => {}
        None => reasons.push("unpushed commits unknown".into()),
    }
    match facts.dirty {
        Some(0) => {}
        Some(n) => reasons.push(format!("{n} uncommitted changes")),
        None => reasons.push("uncommitted changes unknown".into()),
    }
    match facts.stashes {
        Some(0) => {}
        Some(n) => reasons.push(format!("{n} stashes")),
        None => reasons.push("stashes unknown".into()),
    }
    reasons
}

/// Run git read-only in `repo`. `GIT_OPTIONAL_LOCKS=0` stops `git status`
/// from rewriting the index (which would also bump its mtime), and
/// `GIT_TERMINAL_PROMPT=0` stops any credential prompt from hanging a scan.
fn git_out(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

fn repo_facts(repo: &Path) -> RepoFacts {
    let last_commit = git_out(repo, &["log", "-1", "--format=%ct"])
        .and_then(|s| s.trim().parse::<i64>().ok())
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single());

    let remote = git_out(repo, &["remote"]).and_then(|names| {
        let names: Vec<&str> = names
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let pick = names
            .iter()
            .find(|n| **n == "origin")
            .or_else(|| names.first())?;
        let url = git_out(repo, &["remote", "get-url", pick])
            .map(|u| redact_url(u.trim()))
            .unwrap_or_default();
        Some(if url.is_empty() {
            pick.to_string()
        } else {
            url
        })
    });

    // A repo with no commits has no branches to count: zero is accurate.
    let unpushed = if last_commit.is_none() {
        Some(0)
    } else {
        git_out(
            repo,
            &["rev-list", "--count", "--branches", "--not", "--remotes"],
        )
        .and_then(|s| s.trim().parse().ok())
    };

    let dirty = git_out(repo, &["status", "--porcelain", "--untracked-files=normal"])
        .map(|s| count_dirty(&s));

    let stashes = if last_commit.is_none() {
        Some(0)
    } else {
        git_out(repo, &["stash", "list"]).map(|s| s.lines().count() as u64)
    };

    RepoFacts {
        last_commit,
        remote,
        unpushed,
        dirty,
        stashes,
    }
}

/// Count `git status --porcelain` lines, ignoring untracked build output: an
/// un-ignored `node_modules/` is not work the user would lose.
fn count_dirty(porcelain: &str) -> u64 {
    porcelain
        .lines()
        .filter(|line| line.len() > 3)
        .filter(|line| {
            let (code, path) = line.split_at(3);
            if code.starts_with("??") {
                let first = path.trim_matches('"').split('/').next().unwrap_or("");
                !ARTIFACT_NAMES.contains(&first)
            } else {
                true
            }
        })
        .count() as u64
}

/// Strip credentials from a remote URL before showing it:
/// `https://user:token@host/x` → `https://host/x`.
fn redact_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((userinfo, host)) = rest.split_once('@') {
            if !userinfo.contains('/') {
                return format!("{scheme}://{host}");
            }
        }
    }
    url.to_string()
}

/// The newest mtime of the project's own entries: the repo's top level and
/// one level below, skipping `.git` and build output. Bounded, so a huge
/// repo costs a few hundred `stat`s, not a full walk.
fn last_edit_time(repo: &Path) -> Option<DateTime<Utc>> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut consider = |t: std::time::SystemTime| {
        if newest.is_none_or(|n| t > n) {
            newest = Some(t);
        }
    };
    let skip = |name: &str| name == ".git" || ARTIFACT_NAMES.contains(&name);

    let entries = std::fs::read_dir(repo).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if skip(&name.to_string_lossy()) {
            continue;
        }
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        if let Ok(t) = meta.modified() {
            consider(t);
        }
        if meta.is_dir() {
            let Ok(children) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for child in children.flatten() {
                if skip(&child.file_name().to_string_lossy()) {
                    continue;
                }
                if let Ok(t) = child.path().symlink_metadata().and_then(|m| m.modified()) {
                    consider(t);
                }
            }
        }
    }
    newest.map(DateTime::<Utc>::from)
}

/// Regenerable build directories inside `repo`, each confirmed by a marker
/// where the name alone is ambiguous (`target` without `Cargo.toml` could be
/// anything; so could `build`).
fn find_artifacts(repo: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_artifacts(repo, 0, &mut out);
    out.sort();
    out
}

fn collect_artifacts(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if name == ".git" {
            continue;
        }
        if is_artifact(dir, &name) {
            out.push(path);
            continue;
        }
        // Do not wander into hidden tool state or nested repositories.
        if depth + 1 < MAX_ARTIFACT_DEPTH && !name.starts_with('.') && !path.join(".git").exists() {
            collect_artifacts(&path, depth + 1, out);
        }
    }
}

fn is_artifact(parent: &Path, name: &str) -> bool {
    let has = |f: &str| parent.join(f).exists();
    match name {
        "node_modules" | ".next" | ".turbo" | "__pycache__" | ".gradle" | "DerivedData" => true,
        "target" => has("Cargo.toml"),
        ".venv" => true,
        "build" => {
            has("build.gradle")
                || has("build.gradle.kts")
                || has("settings.gradle")
                || has("settings.gradle.kts")
        }
        _ => false,
    }
}

/// Files and folders AI coding agents leave behind — a hint that the project
/// was an agent's spike rather than hand-built.
fn ai_markers(repo: &Path) -> Vec<&'static str> {
    [
        "CLAUDE.md",
        "AGENTS.md",
        ".claude",
        ".cursor",
        ".cursorrules",
        ".windsurfrules",
    ]
    .into_iter()
    .filter(|m| repo.join(m).exists())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_in_remote_urls() {
        assert_eq!(
            redact_url("https://me:ghp_secret@github.com/me/x.git"),
            "https://github.com/me/x.git"
        );
        assert_eq!(
            redact_url("git@github.com:me/x.git"),
            "git@github.com:me/x.git"
        );
        assert_eq!(redact_url("/tmp/remote.git"), "/tmp/remote.git");
    }

    #[test]
    fn untracked_build_output_is_not_dirty() {
        let porcelain = "?? node_modules/\n?? notes.md\n M src/main.rs\n?? target/\n";
        assert_eq!(count_dirty(porcelain), 2);
        assert_eq!(count_dirty(""), 0);
    }

    #[test]
    fn ambiguous_artifact_names_need_a_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        std::fs::create_dir_all(p.join("target")).unwrap();
        std::fs::create_dir_all(p.join("build")).unwrap();
        std::fs::create_dir_all(p.join("node_modules")).unwrap();
        assert_eq!(find_artifacts(p), vec![p.join("node_modules")]);

        std::fs::write(p.join("Cargo.toml"), "").unwrap();
        std::fs::write(p.join("build.gradle"), "").unwrap();
        assert_eq!(
            find_artifacts(p),
            vec![p.join("build"), p.join("node_modules"), p.join("target")]
        );
    }

    #[test]
    fn monorepo_artifacts_are_found_one_level_down() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        std::fs::create_dir_all(p.join("packages/web/node_modules/x")).unwrap();
        std::fs::create_dir_all(p.join("node_modules/y")).unwrap();
        assert_eq!(
            find_artifacts(p),
            vec![p.join("node_modules"), p.join("packages/web/node_modules")]
        );
    }

    #[test]
    fn repo_search_skips_hidden_and_artifact_dirs_and_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        std::fs::create_dir_all(p.join("code/a/.git")).unwrap();
        std::fs::create_dir_all(p.join("code/a/nested/.git")).unwrap();
        std::fs::create_dir_all(p.join(".claude/worktrees/w/.git")).unwrap();
        std::fs::create_dir_all(p.join("node_modules/pkg/.git")).unwrap();
        std::fs::create_dir_all(p.join("code/wt")).unwrap();
        std::fs::write(p.join("code/wt/.git"), "gitdir: /elsewhere").unwrap();
        std::fs::create_dir_all(p.join("a/b/c/d/e/.git")).unwrap();
        let mut found = Vec::new();
        find_repos(p, 0, &mut found);
        assert_eq!(found, vec![p.join("code/a")]);
    }
}
