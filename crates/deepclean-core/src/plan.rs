//! Dry-run plans: `scan` → `plan` → `apply(plan_id)`.
//!
//! Lets the CLI and the MCP server show exactly what would be cleaned before
//! anything is, and apply precisely that — never a fresh re-selection. An
//! agent that was shown plan `P` and got the user's "yes" must not end up
//! running something the user never saw, so a plan is persisted verbatim and
//! applied from disk by id.
//!
//! Plans expire after [`PLAN_TTL`]: an approval given an hour ago is about a
//! disk that has since changed. The executor still re-checks every path at
//! execution time, so expiry is about honesty, not the only safety net.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::action::ActionExecutor;
use crate::history::{CleanRun, RunItem, RunOutcome};
use crate::model::{
    ActionEvent, ActionMethod, ArtifactKind, CleanAction, CleanableItem, Ecosystem, RiskLevel,
};

/// How long a saved plan stays applicable.
pub const PLAN_TTL: Duration = Duration::from_secs(60 * 60);

/// A reviewed, persisted selection of (item, action) pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    /// The home the plan was built against. Applying it under a different
    /// home (sandbox vs. real) is refused by callers.
    pub home: PathBuf,
    /// Largest first.
    pub entries: Vec<PlanEntry>,
    /// Sum of the chosen actions' estimates.
    pub total_bytes: u64,
    /// True when some entry runs a method that cannot measure what it frees
    /// (a command, a Docker prune, a git prune), so the total is an estimate
    /// even after it is applied.
    pub estimated: bool,
}

/// One item and the single action chosen for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub item: CleanableItem,
    pub action: CleanAction,
}

impl Plan {
    /// When this plan stops being applicable.
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.created_at + chrono::Duration::from_std(PLAN_TTL).unwrap_or(chrono::Duration::hours(1))
    }

    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at()
    }

    /// The riskiest action in the plan (`Safe` for an empty plan).
    pub fn highest_risk(&self) -> RiskLevel {
        self.entries
            .iter()
            .map(|e| e.action.risk)
            .max()
            .unwrap_or(RiskLevel::Safe)
    }
}

/// Which items a plan may select, and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanFilter {
    /// Only these ecosystems (empty = any).
    pub ecosystems: Vec<Ecosystem>,
    /// Only these kinds (empty = any).
    pub kinds: Vec<ArtifactKind>,
    /// Never pick an action riskier than this.
    pub max_risk: RiskLevel,
    /// Only items at least this many days stale. Items whose staleness is
    /// unknown are excluded when this is set.
    pub min_days_stale: Option<u64>,
    /// Only items at or under one of these paths (empty = anywhere).
    pub paths: Vec<PathBuf>,
    /// Only these items (empty = any).
    pub item_ids: Vec<Uuid>,
    /// Prefer "Move to Trash" for recovery-worthy kinds when it ties on risk.
    pub prefer_trash: bool,
    /// Stop adding entries once the plan's total reaches this many bytes.
    pub limit_bytes: Option<u64>,
}

impl Default for PlanFilter {
    fn default() -> Self {
        Self {
            ecosystems: Vec::new(),
            kinds: Vec::new(),
            max_risk: RiskLevel::Safe,
            min_days_stale: None,
            paths: Vec::new(),
            item_ids: Vec::new(),
            prefer_trash: false,
            limit_bytes: None,
        }
    }
}

impl PlanFilter {
    fn admits(&self, item: &CleanableItem, roots: &[PathBuf]) -> bool {
        if !self.ecosystems.is_empty() && !self.ecosystems.contains(&item.ecosystem) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&item.kind) {
            return false;
        }
        if !self.item_ids.is_empty() && !self.item_ids.contains(&item.id) {
            return false;
        }
        if let Some(min) = self.min_days_stale {
            match item.days_stale {
                Some(days) if days >= min => {}
                _ => return false,
            }
        }
        if !roots.is_empty() {
            let path = canonical(&item.path);
            if !roots.iter().any(|root| path.starts_with(root)) {
                return false;
            }
        }
        true
    }
}

/// Canonicalize when possible so `/var/…` and `/private/var/…` compare equal.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the executor measures what this method frees. Commands and prunes
/// report zero, so plans relying on them can only estimate.
fn is_measured(method: &ActionMethod) -> bool {
    !matches!(
        method,
        ActionMethod::Command { .. }
            | ActionMethod::DockerPrune { .. }
            | ActionMethod::GitWorktreePrune { .. }
            | ActionMethod::GitDeleteBranches { .. }
    )
}

/// Pick the action a plan runs for `item`, if any is at or below `max_risk`.
///
/// Lowest risk wins. On a tie: "Move to Trash" when the user prefers it and
/// the kind is worth recovering; otherwise the largest estimate; then an
/// in-process filesystem method over an external command (it needs no tool
/// on PATH and its result is measured); then the scanner's own order, which
/// lists the lowest-risk option first.
pub fn choose_action(
    item: &CleanableItem,
    max_risk: RiskLevel,
    prefer_trash: bool,
) -> Option<CleanAction> {
    let eligible: Vec<&CleanAction> = item
        .available_actions
        .iter()
        .filter(|a| a.risk <= max_risk)
        .collect();
    let lowest = eligible.iter().map(|a| a.risk).min()?;
    let tied: Vec<&CleanAction> = eligible.into_iter().filter(|a| a.risk == lowest).collect();

    if prefer_trash && crate::trash::worth_recovering(item.kind) {
        if let Some(trash) = tied
            .iter()
            .find(|a| matches!(a.method, ActionMethod::MoveToTrash { .. }))
        {
            return Some((*trash).clone());
        }
    }

    let mut best: Option<&CleanAction> = None;
    for action in tied {
        best = match best {
            None => Some(action),
            Some(current) => {
                let better = action.estimated_savings_bytes > current.estimated_savings_bytes
                    || (action.estimated_savings_bytes == current.estimated_savings_bytes
                        && is_measured(&action.method)
                        && !is_measured(&current.method));
                Some(if better { action } else { current })
            }
        };
    }
    best.cloned()
}

/// Build a plan from scanned `items`.
///
/// Items with no action at or below `filter.max_risk` are skipped. An item
/// nested inside what another selected entry deletes is dropped (deleting
/// the outer one already covers it, and running both would fail the
/// second). Nesting is judged by what the chosen action removes, not the
/// item's path, so a repo-level `git worktree prune` does not hide the
/// worktrees inside that repo. Entries are
/// sorted largest first, and `limit_bytes` stops the plan once reached.
pub fn build_plan(items: &[CleanableItem], filter: &PlanFilter) -> Plan {
    let roots: Vec<PathBuf> = filter.paths.iter().map(|p| canonical(p)).collect();

    let mut candidates: Vec<PlanEntry> = items
        .iter()
        .filter(|item| filter.admits(item, &roots))
        .filter_map(|item| {
            choose_action(item, filter.max_risk, filter.prefer_trash).map(|action| PlanEntry {
                item: item.clone(),
                action,
            })
        })
        .collect();

    // Outermost footprints first, so an outer directory is kept before
    // anything inside it is considered. Entries that remove nothing from
    // the filesystem (a `git worktree prune`) cover nothing and go last.
    let depth = |e: &PlanEntry| {
        footprint(e)
            .iter()
            .map(|p| p.components().count())
            .min()
            .unwrap_or(usize::MAX)
    };
    candidates.sort_by_key(depth);
    let mut kept: Vec<PlanEntry> = Vec::new();
    let mut covered: Vec<PathBuf> = Vec::new();
    for entry in candidates {
        let prints: Vec<PathBuf> = footprint(&entry).iter().map(|p| canonical(p)).collect();
        let mut touches = prints.clone();
        touches.push(canonical(&entry.item.path));
        // `starts_with` is component-wise, so `/a/bc` is not inside `/a/b`;
        // it also catches the same directory reported by two scanners.
        let nested = touches
            .iter()
            .any(|t| covered.iter().any(|c| t.starts_with(c)));
        if nested {
            continue;
        }
        covered.extend(prints);
        kept.push(entry);
    }

    kept.sort_by(|a, b| {
        b.action
            .estimated_savings_bytes
            .cmp(&a.action.estimated_savings_bytes)
            .then_with(|| a.item.path.cmp(&b.item.path))
    });

    let mut entries = Vec::new();
    let mut total = 0u64;
    for entry in kept {
        if let Some(limit) = filter.limit_bytes {
            if total >= limit {
                break;
            }
        }
        total = total.saturating_add(entry.action.estimated_savings_bytes);
        entries.push(entry);
    }

    let estimated = entries.iter().any(|e| !is_measured(&e.action.method));
    Plan {
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        home: crate::paths::home_or_root(),
        entries,
        total_bytes: total,
        estimated,
    }
}

/// The paths an entry's action deletes (or replaces) wholesale. An entry
/// whose path lies inside another entry's footprint is redundant.
fn footprint(entry: &PlanEntry) -> Vec<PathBuf> {
    match &entry.action.method {
        ActionMethod::RemoveDir { path }
        | ActionMethod::RemoveFile { path }
        | ActionMethod::MoveToTrash { path } => vec![path.clone()],
        ActionMethod::RemoveDirs { paths } | ActionMethod::RemoveFiles { paths } => paths.clone(),
        ActionMethod::RemoveOllamaOrphans { blobs, .. } => blobs.clone(),
        ActionMethod::GitWorktreeRemove { worktree, .. } => vec![worktree.clone()],
        // `cargo clean` and friends empty the item they were offered for.
        ActionMethod::Command { .. } => vec![entry.item.path.clone()],
        ActionMethod::DockerPrune { .. }
        | ActionMethod::GitWorktreePrune { .. }
        | ActionMethod::GitDeleteBranches { .. }
        | ActionMethod::DedupFiles { .. } => Vec::new(),
    }
}

/// Human-readable summary of what an action method actually does.
///
/// For commands this is the literal command line — the only honest
/// disclosure available, since Void does not interpret what a command
/// removes.
pub fn describe_method(method: &ActionMethod) -> String {
    match method {
        ActionMethod::Command {
            program,
            args,
            working_dir,
        } => {
            let line = std::iter::once(program.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            match working_dir {
                Some(dir) => format!("{line} (in {})", dir.display()),
                None => line,
            }
        }
        ActionMethod::RemoveDir { path } => format!("delete directory {}", path.display()),
        ActionMethod::RemoveFile { path } => format!("delete file {}", path.display()),
        ActionMethod::DockerPrune { prune_type } => format!("docker {prune_type} prune -f"),
        ActionMethod::MoveToTrash { path } => format!("move {} to the Trash", path.display()),
        ActionMethod::RemoveDirs { paths } => {
            format!("delete {} directories: {}", paths.len(), join_paths(paths))
        }
        ActionMethod::RemoveFiles { paths } => {
            format!("delete {} files: {}", paths.len(), join_paths(paths))
        }
        ActionMethod::GitWorktreeRemove {
            repo,
            worktree,
            delete_branch,
        } => {
            let base = format!(
                "git -C {} worktree remove {} (refused if it has uncommitted or unpushed work)",
                repo.display(),
                worktree.display()
            );
            match delete_branch {
                Some(branch) => format!("{base}, then git branch -d {branch}"),
                None => base,
            }
        }
        ActionMethod::GitWorktreePrune { repo } => {
            format!("git -C {} worktree prune", repo.display())
        }
        ActionMethod::RemoveOllamaOrphans { store, blobs } => format!(
            "delete {} Ollama blobs no model in {} references (manifests re-read first): {}",
            blobs.len(),
            store.display(),
            join_paths(blobs)
        ),
        ActionMethod::GitDeleteBranches { repo, branches } => {
            format!("git -C {} branch -d {}", repo.display(), branches.join(" "))
        }
        ActionMethod::DedupFiles { groups } => format!(
            "replace {} duplicate files with copy-on-write clones or hardlinks (re-hashed first)",
            groups.iter().map(|g| g.duplicates.len()).sum::<usize>()
        ),
    }
}

fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Plans on disk, one JSON file per plan: `<dir>/<id>.json`.
#[derive(Debug, Clone)]
pub struct PlanStore {
    pub dir: PathBuf,
}

impl PlanStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path_for(&self, id: Uuid) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Persist `plan` atomically (temp file + rename).
    pub fn save(&self, plan: &Plan) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|err| format!("could not create {}: {err}", self.dir.display()))?;
        let json = serde_json::to_string_pretty(plan).map_err(|err| err.to_string())?;
        let path = self.path_for(plan.id);
        let tmp = self.dir.join(format!(".{}.json.tmp", plan.id));
        std::fs::write(&tmp, json)
            .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        Ok(path)
    }

    /// Load plan `id`, refusing unknown, corrupt and expired plans.
    pub fn load(&self, id: Uuid) -> Result<Plan, String> {
        let path = self.path_for(id);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "no plan with id {id} (plans are single-use and expire after an hour; run `void plan` again)"
                ))
            }
            Err(err) => return Err(format!("could not read {}: {err}", path.display())),
        };
        let plan: Plan =
            serde_json::from_str(&raw).map_err(|err| format!("plan {id} is unreadable: {err}"))?;
        if plan.id != id {
            return Err(format!("plan file for {id} holds a different plan"));
        }
        if plan.is_expired() {
            return Err(format!(
                "plan {id} expired at {} — the disk may have changed since it was reviewed; run `void plan` again",
                plan.expires_at().format("%Y-%m-%d %H:%M UTC")
            ));
        }
        Ok(plan)
    }

    /// Forget plan `id` (after it was applied). Missing is not an error.
    pub fn remove(&self, id: Uuid) -> Result<(), String> {
        match std::fs::remove_file(self.path_for(id)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }

    /// Delete plans created more than `age` ago (and unreadable plan files
    /// last modified that long ago). Returns how many were removed.
    pub fn prune_older_than(&self, age: Duration) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let cutoff = Utc::now() - chrono::Duration::from_std(age).unwrap_or(chrono::Duration::MAX);
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let created = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Plan>(&raw).ok())
                .map(|p| p.created_at)
                .or_else(|| {
                    entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .map(DateTime::<Utc>::from)
                });
            if created.is_some_and(|c| c < cutoff) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

/// One entry that did not apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyFailure {
    pub path: PathBuf,
    pub error: String,
}

/// What applying a plan did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyReport {
    pub plan_id: Uuid,
    pub succeeded: usize,
    pub failed: Vec<ApplyFailure>,
    /// Measured where possible, estimated where not.
    pub bytes_freed: u64,
    pub estimated: bool,
    /// The run in `history.json` form, so callers can record it where the
    /// desktop app's History screen will show it.
    pub run: CleanRun,
}

/// Execute every entry of `plan` through `executor` (which runs the safety
/// checker before each action) and tally the events.
pub async fn apply_plan(plan: &Plan, executor: ActionExecutor) -> ApplyReport {
    let started_at = Utc::now();
    let start = std::time::Instant::now();

    let pairs: Vec<(CleanableItem, CleanAction)> = plan
        .entries
        .iter()
        .map(|e| (e.item.clone(), e.action.clone()))
        .collect();
    let by_item: std::collections::HashMap<Uuid, &PlanEntry> =
        plan.entries.iter().map(|e| (e.item.id, e)).collect();

    let mut rx = executor.execute_batch(pairs);
    let mut succeeded = 0;
    let mut failed = Vec::new();
    let mut items: Vec<RunItem> = Vec::new();
    let mut batch_bytes: Option<(u64, bool)> = None;

    let record =
        |entry: &PlanEntry, bytes_freed: u64, estimated: u64, result: RunOutcome| RunItem {
            path: entry.item.path.clone(),
            ecosystem: entry.item.ecosystem,
            action_label: entry.action.label.clone(),
            method_summary: describe_method(&entry.action.method),
            bytes_freed,
            estimated_bytes: estimated,
            result,
        };

    while let Some(event) = rx.recv().await {
        match event {
            ActionEvent::Completed {
                item_id,
                bytes_freed,
                estimated_bytes,
                ..
            } => {
                succeeded += 1;
                if let Some(entry) = by_item.get(&item_id) {
                    items.push(record(
                        entry,
                        bytes_freed,
                        estimated_bytes,
                        RunOutcome::Succeeded,
                    ));
                }
            }
            ActionEvent::Failed { item_id, error, .. } => {
                if let Some(entry) = by_item.get(&item_id) {
                    failed.push(ApplyFailure {
                        path: entry.item.path.clone(),
                        error: error.clone(),
                    });
                    items.push(record(
                        entry,
                        0,
                        entry.action.estimated_savings_bytes,
                        RunOutcome::Failed { error },
                    ));
                }
            }
            ActionEvent::BatchComplete {
                bytes_freed,
                estimated,
                ..
            } => batch_bytes = Some((bytes_freed, estimated)),
            ActionEvent::Started { .. } => {}
        }
    }

    let run = CleanRun {
        id: Uuid::new_v4(),
        started_at,
        duration_ms: start.elapsed().as_millis() as u64,
        items,
        trigger: None,
    };
    let (bytes_freed, estimated) =
        batch_bytes.unwrap_or_else(|| (run.bytes_freed(), run.is_estimated()));

    ApplyReport {
        plan_id: plan.id,
        succeeded,
        failed,
        bytes_freed,
        estimated,
        run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(method: ActionMethod, risk: RiskLevel, bytes: u64) -> CleanAction {
        CleanAction {
            id: Uuid::new_v4(),
            label: "act".into(),
            description: String::new(),
            method,
            risk,
            estimated_savings_bytes: bytes,
        }
    }

    fn item(path: &str, kind: ArtifactKind, actions: Vec<CleanAction>) -> CleanableItem {
        CleanableItem {
            id: Uuid::new_v4(),
            path: PathBuf::from(path),
            ecosystem: Ecosystem::Node,
            kind,
            risk: RiskLevel::Safe,
            size_bytes: 0,
            size_display: String::new(),
            last_modified: None,
            days_stale: Some(10),
            project_name: None,
            project_root: None,
            available_actions: actions,
            details: Vec::new(),
            agent: None,
        }
    }

    fn rm(path: &str) -> ActionMethod {
        ActionMethod::RemoveDir { path: path.into() }
    }

    #[test]
    fn picks_the_lowest_risk_action_within_the_cap() {
        let it = item(
            "/p/node_modules",
            ArtifactKind::NodeModules,
            vec![
                action(rm("/p/node_modules"), RiskLevel::Caution, 100),
                action(rm("/p/node_modules/.cache"), RiskLevel::Safe, 10),
                action(rm("/p"), RiskLevel::Danger, 1000),
            ],
        );
        let chosen = choose_action(&it, RiskLevel::Caution, false).unwrap();
        assert_eq!(chosen.risk, RiskLevel::Safe);
        assert!(choose_action(&it, RiskLevel::Safe, false).is_some());

        let only_danger = item(
            "/q",
            ArtifactKind::StaleProject,
            vec![action(rm("/q"), RiskLevel::Danger, 5)],
        );
        assert!(choose_action(&only_danger, RiskLevel::Caution, false).is_none());
    }

    #[test]
    fn tie_prefers_trash_for_recovery_worthy_kinds_only_when_asked() {
        let trash = ActionMethod::MoveToTrash { path: "/p".into() };
        let it = item(
            "/p",
            ArtifactKind::StaleProject,
            vec![
                action(rm("/p"), RiskLevel::Caution, 100),
                action(trash, RiskLevel::Caution, 100),
            ],
        );
        let chosen = choose_action(&it, RiskLevel::Caution, true).unwrap();
        assert!(matches!(chosen.method, ActionMethod::MoveToTrash { .. }));
        let chosen = choose_action(&it, RiskLevel::Caution, false).unwrap();
        assert!(matches!(chosen.method, ActionMethod::RemoveDir { .. }));
    }

    #[test]
    fn tie_prefers_largest_estimate_then_filesystem_over_command() {
        let cmd = ActionMethod::Command {
            program: "cargo".into(),
            args: vec!["clean".into()],
            working_dir: None,
        };
        let it = item(
            "/p/target",
            ArtifactKind::TargetDir,
            vec![
                action(cmd.clone(), RiskLevel::Safe, 50),
                action(rm("/p/target"), RiskLevel::Safe, 50),
                action(rm("/p/target/release"), RiskLevel::Safe, 20),
            ],
        );
        let chosen = choose_action(&it, RiskLevel::Safe, false).unwrap();
        assert!(
            matches!(chosen.method, ActionMethod::RemoveDir { ref path } if path == Path::new("/p/target"))
        );

        let bigger_cmd = item(
            "/p/target",
            ArtifactKind::TargetDir,
            vec![
                action(rm("/p/target"), RiskLevel::Safe, 50),
                action(cmd, RiskLevel::Safe, 60),
            ],
        );
        let chosen = choose_action(&bigger_cmd, RiskLevel::Safe, false).unwrap();
        assert!(matches!(chosen.method, ActionMethod::Command { .. }));
    }

    #[test]
    fn nested_items_are_dropped_and_entries_sorted_largest_first() {
        let items = vec![
            item(
                "/w/a/node_modules",
                ArtifactKind::NodeModules,
                vec![action(rm("/w/a/node_modules"), RiskLevel::Safe, 10)],
            ),
            item(
                "/w/a",
                ArtifactKind::AgentWorktree,
                vec![action(rm("/w/a"), RiskLevel::Safe, 30)],
            ),
            item(
                "/w/b/target",
                ArtifactKind::TargetDir,
                vec![action(rm("/w/b/target"), RiskLevel::Safe, 50)],
            ),
        ];
        let plan = build_plan(&items, &PlanFilter::default());
        let paths: Vec<_> = plan.entries.iter().map(|e| e.item.path.clone()).collect();
        assert_eq!(
            paths,
            vec![PathBuf::from("/w/b/target"), PathBuf::from("/w/a")]
        );
        assert_eq!(plan.total_bytes, 80);
        assert!(!plan.estimated);
    }

    #[test]
    fn a_repo_level_prune_does_not_swallow_worktrees_inside_the_repo() {
        let items = vec![
            item(
                "/r",
                ArtifactKind::PrunableWorktreeRefs,
                vec![action(
                    ActionMethod::GitWorktreePrune { repo: "/r".into() },
                    RiskLevel::Safe,
                    0,
                )],
            ),
            item(
                "/r",
                ArtifactKind::MergedAgentBranches,
                vec![action(
                    ActionMethod::GitDeleteBranches {
                        repo: "/r".into(),
                        branches: vec!["b".into()],
                    },
                    RiskLevel::Safe,
                    0,
                )],
            ),
            item(
                "/r/.claude/worktrees/x/node_modules",
                ArtifactKind::NodeModules,
                vec![action(
                    rm("/r/.claude/worktrees/x/node_modules"),
                    RiskLevel::Safe,
                    5,
                )],
            ),
            item(
                "/r/.claude/worktrees/x",
                ArtifactKind::AgentWorktree,
                vec![action(
                    ActionMethod::RemoveDirs {
                        paths: vec!["/r/.claude/worktrees/x/node_modules".into()],
                    },
                    RiskLevel::Safe,
                    5,
                )],
            ),
        ];
        let plan = build_plan(&items, &PlanFilter::default());
        let kinds: Vec<_> = plan.entries.iter().map(|e| e.item.kind).collect();
        assert_eq!(plan.entries.len(), 3, "{kinds:?}");
        assert!(kinds.contains(&ArtifactKind::PrunableWorktreeRefs));
        assert!(kinds.contains(&ArtifactKind::MergedAgentBranches));
        assert_eq!(
            kinds
                .iter()
                .filter(|k| matches!(k, ArtifactKind::NodeModules | ArtifactKind::AgentWorktree))
                .count(),
            1,
            "the same node_modules is not removed twice"
        );
    }

    #[test]
    fn filters_apply() {
        let mut a = item(
            "/x/a",
            ArtifactKind::NodeModules,
            vec![action(rm("/x/a"), RiskLevel::Safe, 10)],
        );
        a.days_stale = Some(2);
        let mut b = item(
            "/y/b",
            ArtifactKind::TargetDir,
            vec![action(rm("/y/b"), RiskLevel::Safe, 20)],
        );
        b.ecosystem = Ecosystem::Rust;
        b.days_stale = None;
        let items = vec![a.clone(), b.clone()];

        let by_eco = build_plan(
            &items,
            &PlanFilter {
                ecosystems: vec![Ecosystem::Rust],
                ..Default::default()
            },
        );
        assert_eq!(by_eco.entries.len(), 1);
        assert_eq!(by_eco.entries[0].item.id, b.id);

        let by_kind = build_plan(
            &items,
            &PlanFilter {
                kinds: vec![ArtifactKind::NodeModules],
                ..Default::default()
            },
        );
        assert_eq!(by_kind.entries[0].item.id, a.id);

        let stale = build_plan(
            &items,
            &PlanFilter {
                min_days_stale: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(stale.entries.len(), 1, "unknown staleness is excluded");

        let by_path = build_plan(
            &items,
            &PlanFilter {
                paths: vec!["/y".into()],
                ..Default::default()
            },
        );
        assert_eq!(by_path.entries[0].item.id, b.id);

        let by_id = build_plan(
            &items,
            &PlanFilter {
                item_ids: vec![a.id],
                ..Default::default()
            },
        );
        assert_eq!(by_id.entries.len(), 1);
        assert_eq!(by_id.entries[0].item.id, a.id);
    }

    #[test]
    fn limit_stops_once_reached() {
        let items: Vec<_> = [40u64, 30, 20, 10]
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let p = format!("/l/{i}");
                item(
                    &p,
                    ArtifactKind::NodeModules,
                    vec![action(rm(&p), RiskLevel::Safe, *b)],
                )
            })
            .collect();
        let plan = build_plan(
            &items,
            &PlanFilter {
                limit_bytes: Some(60),
                ..Default::default()
            },
        );
        assert_eq!(plan.total_bytes, 70);
        assert_eq!(plan.entries.len(), 2);
    }

    #[test]
    fn commands_mark_the_plan_estimated() {
        let items = vec![item(
            "/c",
            ArtifactKind::NpmCache,
            vec![action(
                ActionMethod::Command {
                    program: "npm".into(),
                    args: vec!["cache".into(), "clean".into()],
                    working_dir: None,
                },
                RiskLevel::Safe,
                10,
            )],
        )];
        assert!(build_plan(&items, &PlanFilter::default()).estimated);
    }

    #[test]
    fn describe_shows_the_literal_command() {
        let d = describe_method(&ActionMethod::Command {
            program: "cargo".into(),
            args: vec!["clean".into()],
            working_dir: Some("/p".into()),
        });
        assert_eq!(d, "cargo clean (in /p)");
    }
}
