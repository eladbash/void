//! Git awareness: worktrees, branch state, commit recency.
//!
//! Shells out to the `git` CLI rather than linking libgit2 — the user's git is
//! the one that created the worktrees, and its own safety checks (refusing to
//! remove a dirty worktree, refusing `branch -d` on unmerged work) are the
//! last line of defence Void deliberately keeps.
//!
//! Every call runs with `GIT_TERMINAL_PROMPT=0` so a credential helper can
//! never block a scan waiting for input nobody will type. Read-only calls also
//! set `GIT_OPTIONAL_LOCKS=0`: `git status` otherwise refreshes the index and
//! takes `index.lock`, which can collide with an agent working in the same
//! worktree at that moment.
//!
//! The query helpers are synchronous (the scanner calls them from
//! `spawn_blocking`); the three mutating entry points the executor calls are
//! async and do their blocking work off the runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, TimeZone, Utc};
use tracing::{info, warn};

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// Commit the worktree's HEAD points at (absent for a bare entry).
    pub head: Option<String>,
    /// Checked-out branch with `refs/heads/` stripped; `None` when detached.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    /// `Some(reason)` when locked; the reason is empty if none was given.
    pub locked: Option<String>,
    /// `Some(reason)` when git considers the entry stale (directory gone).
    pub prunable: Option<String>,
}

/// Counts from `git status`. Ignored files (build artifacts) never count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusSummary {
    /// Tracked files changed in the working tree but not staged.
    pub modified: u32,
    /// Files git does not track and does not ignore.
    pub untracked: u32,
    /// Changes staged in the index.
    pub staged: u32,
}

impl StatusSummary {
    pub fn is_clean(&self) -> bool {
        self.modified == 0 && self.untracked == 0 && self.staged == 0
    }

    /// "2 modified, 1 untracked" — or "none".
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.modified > 0 {
            parts.push(format!("{} modified", self.modified));
        }
        if self.staged > 0 {
            parts.push(format!("{} staged", self.staged));
        }
        if self.untracked > 0 {
            parts.push(format!("{} untracked", self.untracked));
        }
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join(", ")
        }
    }
}

/// A `git` invocation in `dir`, never prompting.
fn git_cmd(dir: &Path, read_only: bool) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if read_only {
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
    }
    cmd.stdin(std::process::Stdio::null());
    cmd
}

/// Run git and return stdout, or a human error including git's stderr.
fn run(dir: &Path, args: &[&str], read_only: bool) -> Result<String, String> {
    let out = git_cmd(dir, read_only).args(args).output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            "`git` was not found in PATH".to_string()
        } else {
            format!("could not run git: {err}")
        }
    })?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a git command exits 0. Errors (git missing) are `None`.
fn succeeds(dir: &Path, args: &[&str]) -> Option<bool> {
    git_cmd(dir, true)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .map(|s| s.success())
}

/// Parse `git worktree list --porcelain` output.
///
/// Records are separated by blank lines; each starts with `worktree <path>`.
/// Unknown attributes are ignored so newer git versions do not break parsing.
pub fn parse_worktree_porcelain(text: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;

    for line in text.lines() {
        if line.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        let (key, value) = match line.split_once(' ') {
            Some((k, v)) => (k, Some(v)),
            None => (line, None),
        };
        if key == "worktree" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: PathBuf::from(value.unwrap_or_default()),
                ..Default::default()
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => entry.head = value.map(str::to_string),
            "branch" => {
                entry.branch = value.map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string())
            }
            "detached" => entry.detached = true,
            "bare" => entry.bare = true,
            "locked" => entry.locked = Some(value.unwrap_or_default().to_string()),
            "prunable" => entry.prunable = Some(value.unwrap_or_default().to_string()),
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

/// Every worktree `repo` knows about, the main one first.
pub fn list_worktrees(repo: &Path) -> Result<Vec<WorktreeEntry>, String> {
    run(repo, &["worktree", "list", "--porcelain"], true).map(|s| parse_worktree_porcelain(&s))
}

/// Parse `git status --porcelain=v1 -z` output.
///
/// Each record is `XY <path>\0`; renames and copies carry a second,
/// NUL-terminated original path that must be skipped.
pub fn parse_status_porcelain_z(text: &str) -> StatusSummary {
    let mut summary = StatusSummary::default();
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        let bytes = record.as_bytes();
        if bytes.len() < 3 {
            continue;
        }
        let (x, y) = (bytes[0], bytes[1]);
        match (x, y) {
            (b'?', b'?') => summary.untracked += 1,
            (b'!', b'!') => {}
            _ => {
                if x != b' ' {
                    summary.staged += 1;
                }
                if y != b' ' {
                    summary.modified += 1;
                }
                if matches!(x, b'R' | b'C') {
                    // The original path of a rename is its own record.
                    records.next();
                }
            }
        }
    }
    summary
}

/// Uncommitted work in `worktree`. Ignored files are not counted: a
/// `node_modules` is not work.
pub fn status_summary(worktree: &Path) -> Result<StatusSummary, String> {
    run(
        worktree,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        true,
    )
    .map(|s| parse_status_porcelain_z(&s))
}

/// Whether `rev` resolves in `dir`.
fn rev_exists(dir: &Path, rev: &str) -> bool {
    succeeds(dir, &["rev-parse", "--verify", "--quiet", rev]) == Some(true)
}

/// Commits reachable from the worktree's HEAD that exist nowhere else: not on
/// any remote-tracking branch and not on the local default branch.
///
/// A branch fully contained in `main` is not "unpushed work" even if it was
/// never pushed. `None` when git cannot answer — callers must treat that as
/// "unknown", never as zero.
pub fn unpushed_commits(worktree: &Path) -> Option<u32> {
    let mut args: Vec<String> = vec![
        "rev-list".into(),
        "--count".into(),
        "HEAD".into(),
        "--not".into(),
        "--remotes".into(),
    ];
    if let Some(default) = default_branch(worktree) {
        let local = format!("refs/heads/{default}");
        if rev_exists(worktree, &local) {
            args.push(local);
        }
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(worktree, &args, true).ok()?.trim().parse().ok()
}

/// Whether `branch`'s tip is contained in the default branch, locally or on
/// `origin`.
///
/// Squash- and rebase-merged PRs rewrite commits and so do not count; that
/// errs toward keeping the branch, which is the safe side.
pub fn is_merged(repo: &Path, branch: &str, default_branch: &str) -> bool {
    if branch.starts_with('-') {
        return false;
    }
    let targets = [
        format!("refs/heads/{default_branch}"),
        format!("refs/remotes/origin/{default_branch}"),
    ];
    targets.iter().any(|target| {
        rev_exists(repo, target)
            && succeeds(repo, &["merge-base", "--is-ancestor", branch, target]) == Some(true)
    })
}

/// The repository's default branch: what `origin/HEAD` points at, else
/// `main`, else `master`.
pub fn default_branch(repo: &Path) -> Option<String> {
    if let Ok(out) = run(
        repo,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        true,
    ) {
        if let Some(name) = out.trim().strip_prefix("refs/remotes/origin/") {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|name| rev_exists(repo, &format!("refs/heads/{name}")))
        .map(str::to_string)
}

/// Time of the most recent commit on HEAD in `dir`.
pub fn last_commit_time(dir: &Path) -> Option<DateTime<Utc>> {
    let out = run(dir, &["log", "-1", "--format=%ct"], true).ok()?;
    let secs: i64 = out.trim().parse().ok()?;
    Utc.timestamp_opt(secs, 0).single()
}

/// Where a linked worktree's `.git` *file* points (`gitdir: <path>`), resolved
/// against the worktree when relative. `None` if `.git` is not such a file.
///
/// Pure filesystem: works for orphans whose repository is gone.
pub fn resolve_gitdir_link(worktree_dir: &Path) -> Option<PathBuf> {
    let dotgit = worktree_dir.join(".git");
    if !dotgit.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&dotgit).ok()?;
    let target = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }
    let target = PathBuf::from(target);
    Some(if target.is_absolute() {
        target
    } else {
        worktree_dir.join(target)
    })
}

/// The main repository a linked worktree belongs to, via its admin
/// directory's `commondir` file. Pure filesystem; `None` if the chain is
/// broken anywhere.
///
/// For a normal repository this is the directory holding `.git`; for a bare
/// repository it is the bare directory itself.
pub fn main_repo_of(worktree_dir: &Path) -> Option<PathBuf> {
    let admin = resolve_gitdir_link(worktree_dir)?;
    let common = std::fs::read_to_string(admin.join("commondir"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                p
            } else {
                admin.join(p)
            }
        })
        // Without `commondir` the admin dir is conventionally
        // `<common>/worktrees/<name>`.
        .or_else(|| admin.parent()?.parent().map(Path::to_path_buf))?;
    let common = common.canonicalize().ok()?;
    if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().map(Path::to_path_buf)
    } else {
        Some(common)
    }
}

/// Local branches whose names start with one of `prefixes` and are fully
/// merged into `default`, excluding the default branch and any branch
/// checked out in a worktree (git would refuse to delete those anyway).
pub fn merged_branches(
    repo: &Path,
    default: &str,
    prefixes: &[&str],
) -> Result<Vec<String>, String> {
    let out = run(
        repo,
        &[
            "branch",
            "--merged",
            &format!("refs/heads/{default}"),
            "--format=%(refname:short)",
        ],
        true,
    )?;
    let checked_out: Vec<String> = list_worktrees(repo)?
        .into_iter()
        .filter_map(|e| e.branch)
        .collect();
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != default)
        .filter(|b| prefixes.iter().any(|p| b.starts_with(p)))
        .filter(|b| !checked_out.iter().any(|c| c == b))
        .map(str::to_string)
        .collect())
}

/// Compare paths the way the filesystem does: canonical where possible.
fn same_path(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

/// Every precondition for removing `worktree` from `repo`, checked now.
///
/// Returns the worktree's entry when removal is safe, or a sentence saying
/// why it is not.
fn verify_removable(repo: &Path, worktree: &Path) -> Result<WorktreeEntry, String> {
    let entries = list_worktrees(repo)?;
    let position = entries
        .iter()
        .position(|e| same_path(&e.path, worktree))
        .ok_or_else(|| {
            format!(
                "{} is not a worktree of {}",
                worktree.display(),
                repo.display()
            )
        })?;
    if position == 0 {
        return Err("refusing to remove the repository's main worktree".into());
    }
    let entry = entries[position].clone();
    if entry.bare {
        return Err("refusing to remove a bare repository entry".into());
    }
    if let Some(reason) = &entry.locked {
        return Err(if reason.is_empty() {
            "the worktree is locked — an agent may still be using it".into()
        } else {
            format!("the worktree is locked ({reason})")
        });
    }
    let status = status_summary(worktree)?;
    if !status.is_clean() {
        return Err(format!(
            "the worktree has uncommitted changes ({}) — nothing was removed",
            status.describe()
        ));
    }
    match unpushed_commits(worktree) {
        Some(0) => {}
        Some(n) => {
            // `unpushed_commits` already discounts the default branch, so
            // this only passes for work merged on origin's default branch.
            let merged = default_branch(repo).is_some_and(|d| {
                entry
                    .head
                    .as_deref()
                    .is_some_and(|head| is_merged(repo, head, &d))
            });
            if !merged {
                return Err(format!(
                    "the worktree has {n} commit{} that exist nowhere else — nothing was removed",
                    if n == 1 { "" } else { "s" }
                ));
            }
        }
        None => return Err("could not determine whether the worktree has unpushed commits".into()),
    }
    Ok(entry)
}

fn remove_worktree_checked_sync(
    repo: &Path,
    worktree: &Path,
    delete_branch: Option<&str>,
) -> Result<(), String> {
    let entry = verify_removable(repo, worktree)?;

    // No `--force`: git re-checks cleanliness itself and refuses to remove a
    // tree with untracked or modified files, closing the window between our
    // check and this call.
    let target = entry.path.to_string_lossy().into_owned();
    run(repo, &["worktree", "remove", &target], false)?;
    info!(worktree = %worktree.display(), "Removed git worktree");

    if let Some(branch) = delete_branch {
        // The worktree is already gone at this point, so failing the whole
        // action over the branch would misreport what happened. Lowercase
        // `-d` lets git refuse unmerged work; a refusal is logged and the
        // branch stays — it will surface as a leftover branch next scan.
        if branch.starts_with('-') {
            warn!(branch, "Not deleting a branch whose name looks like a flag");
        } else if let Err(err) = run(repo, &["branch", "-d", branch], false) {
            warn!(branch, "Worktree removed but its branch was kept: {err}");
        }
    }
    Ok(())
}

/// `git worktree remove` after re-verifying, at execution time, that the
/// worktree has no uncommitted, untracked or unpushed work and is not locked.
///
/// Returns an error — and removes nothing — if any check fails. When
/// `delete_branch` is given the branch is deleted with `git branch -d`
/// afterwards; if git refuses (unmerged), the removal still reports success
/// and the refusal is logged, because the worktree is already gone.
pub async fn remove_worktree_checked(
    repo: &Path,
    worktree: &Path,
    delete_branch: Option<&str>,
) -> Result<(), String> {
    let (repo, worktree) = (repo.to_path_buf(), worktree.to_path_buf());
    let branch = delete_branch.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        remove_worktree_checked_sync(&repo, &worktree, branch.as_deref())
    })
    .await
    .map_err(|err| err.to_string())?
}

/// `git worktree prune`: drops administrative entries for worktrees whose
/// directories are gone. Never touches a working directory.
pub async fn prune_worktrees(repo: &Path) -> Result<(), String> {
    let repo = repo.to_path_buf();
    tokio::task::spawn_blocking(move || run(&repo, &["worktree", "prune"], false).map(|_| ()))
        .await
        .map_err(|err| err.to_string())?
}

fn delete_merged_branches_sync(repo: &Path, branches: &[String]) -> Result<Vec<String>, String> {
    let default = default_branch(repo);
    let checked_out: Vec<String> = list_worktrees(repo)?
        .into_iter()
        .filter_map(|e| e.branch)
        .collect();

    let mut deleted = Vec::new();
    let mut refused = Vec::new();
    for branch in branches {
        if branch.is_empty() || branch.starts_with('-') {
            refused.push(format!("{branch}: not a valid branch name"));
        } else if default.as_deref() == Some(branch.as_str()) {
            refused.push(format!("{branch}: the default branch is never deleted"));
        } else if checked_out.contains(branch) {
            refused.push(format!("{branch}: checked out in a worktree"));
        } else {
            // Never `-D`: git refuses to drop unmerged commits, and that
            // refusal is the point.
            match run(repo, &["branch", "-d", branch], false) {
                Ok(_) => deleted.push(branch.clone()),
                Err(err) => refused.push(format!("{branch}: {err}")),
            }
        }
    }
    if !refused.is_empty() {
        warn!(repo = %repo.display(), "Branches kept: {}", refused.join("; "));
    }
    if deleted.is_empty() {
        return Err(format!("no branch was deleted: {}", refused.join("; ")));
    }
    Ok(deleted)
}

/// `git branch -d` for each branch. Git itself refuses unmerged branches;
/// the default branch and branches checked out anywhere are refused before
/// git is asked. Returns the branches actually deleted, or an error if none
/// could be.
pub async fn delete_merged_branches(
    repo: &Path,
    branches: &[String],
) -> Result<Vec<String>, String> {
    let repo = repo.to_path_buf();
    let branches = branches.to_vec();
    tokio::task::spawn_blocking(move || delete_merged_branches_sync(&repo, &branches))
        .await
        .map_err(|err| err.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    const PORCELAIN: &str = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.claude/worktrees/fix-login
HEAD 2222222222222222222222222222222222222222
branch refs/heads/worktree-fix-login
locked claude agent 4242 is running

worktree /repo/.claude/worktrees/gone
HEAD 3333333333333333333333333333333333333333
branch refs/heads/worktree-gone
prunable gitdir file points to non-existent location

worktree /tmp/detached
HEAD 4444444444444444444444444444444444444444
detached
locked

";

    #[test]
    fn porcelain_parses_every_attribute() {
        let entries = parse_worktree_porcelain(PORCELAIN);
        assert_eq!(entries.len(), 4);

        assert_eq!(entries[0].path, PathBuf::from("/repo"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert!(entries[0].locked.is_none() && entries[0].prunable.is_none());

        assert_eq!(entries[1].branch.as_deref(), Some("worktree-fix-login"));
        assert_eq!(
            entries[1].locked.as_deref(),
            Some("claude agent 4242 is running")
        );

        assert_eq!(
            entries[2].prunable.as_deref(),
            Some("gitdir file points to non-existent location")
        );

        assert!(entries[3].detached);
        assert_eq!(entries[3].branch, None);
        assert_eq!(entries[3].locked.as_deref(), Some(""));
        assert_eq!(
            entries[3].head.as_deref(),
            Some("4444444444444444444444444444444444444444")
        );
    }

    #[test]
    fn porcelain_parses_bare_and_missing_trailing_blank() {
        let entries = parse_worktree_porcelain(
            "worktree /srv/repo.git\nbare\n\nworktree /w\nHEAD abc\nbranch refs/heads/claude/x",
        );
        assert_eq!(entries.len(), 2);
        assert!(entries[0].bare);
        assert_eq!(entries[0].head, None);
        assert_eq!(entries[1].branch.as_deref(), Some("claude/x"));
    }

    #[test]
    fn porcelain_ignores_unknown_attributes() {
        let entries = parse_worktree_porcelain("worktree /a\nfuture-thing yes\nHEAD abc\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].head.as_deref(), Some("abc"));
    }

    #[test]
    fn status_counts_each_category_and_skips_rename_origins() {
        let text = " M src/a.rs\0M  src/b.rs\0MM src/c.rs\0?? new.txt\0R  new_name\0old_name\0";
        let s = parse_status_porcelain_z(text);
        assert_eq!(
            s,
            StatusSummary {
                modified: 2,
                untracked: 1,
                staged: 3
            }
        );
        assert!(!s.is_clean());
        assert_eq!(s.describe(), "2 modified, 3 staged, 1 untracked");
    }

    #[test]
    fn empty_status_is_clean() {
        let s = parse_status_porcelain_z("");
        assert!(s.is_clean());
        assert_eq!(s.describe(), "none");
    }

    #[test]
    fn gitdir_link_resolves_relative_and_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
        assert_eq!(
            resolve_gitdir_link(&wt),
            Some(wt.join("../repo/.git/worktrees/wt"))
        );
        std::fs::write(wt.join(".git"), "gitdir: /abs/path\n").unwrap();
        assert_eq!(resolve_gitdir_link(&wt), Some(PathBuf::from("/abs/path")));

        let full = tmp.path().join("full");
        std::fs::create_dir_all(full.join(".git")).unwrap();
        assert_eq!(resolve_gitdir_link(&full), None);
    }

    #[test]
    fn main_repo_follows_commondir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let admin = repo.join(".git/worktrees/wt");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert_eq!(main_repo_of(&wt), Some(repo.canonicalize().unwrap()));

        std::fs::remove_dir_all(repo.join(".git")).unwrap();
        assert_eq!(main_repo_of(&wt), None);
    }
}
