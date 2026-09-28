//! Data AI agents and AI editors generate about themselves (Claude Code, Codex, Cursor).
//!
//! Agents write a lot about their own work and clean up little of it: Claude
//! Code's `cleanupPeriodDays` sweep misses subagent transcripts and has let
//! `file-history/` reach hundreds of gigabytes; Codex never rotates
//! `sessions/`; Cursor's `state.vscdb` grows past 50 GB because SQLite never
//! gives freed pages back.
//!
//! Everything here is conversation history or editor state someone might want
//! back, so the scanner is conservative by construction:
//!
//! - Only *generated* data is offered, entry by entry, and only when older
//!   than `ai.agent_data_retention_days`. Recent sessions are always kept.
//! - Configuration, credentials and memory (`settings.json`, `auth.json`,
//!   `memory/`, `CLAUDE.md`…) are filtered out here *and* refused by the
//!   safety checker at execution time.
//! - Cursor's state database is never deleted — that erases chat history
//!   permanently. The only offer is a SQLite `VACUUM`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytesize::ByteSize;
use chrono::{DateTime, Utc};
use tracing::debug;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

/// A `state.vscdb` smaller than this is not worth a vacuum prompt.
pub const STATE_DB_VACUUM_THRESHOLD_BYTES: u64 = 100 * 1024 * 1024;

/// Cache entries younger than this may belong to a session that is running
/// right now (a paste being composed, a shell snapshot in use), so they wait.
pub const CACHE_MIN_AGE_DAYS: u64 = 1;

/// How many of the largest workspaces to name in a workspace-storage item.
const LARGEST_WORKSPACES_SHOWN: usize = 5;

/// Claude Code's own default for `cleanupPeriodDays` when settings omit it.
const CLAUDE_DEFAULT_CLEANUP_DAYS: i64 = 30;

/// Directory names, anywhere under an agent root, that hold user-authored or
/// credential data. Never offered, whatever else matches.
const PROTECTED_DIRS: &[&str] = &[
    "memory", "memories", "skills", "agents", "commands", "plugins",
];

/// File names never offered, anywhere under an agent root.
const PROTECTED_FILES: &[&str] = &[
    "settings.json",
    "settings.local.json",
    ".claude.json",
    "auth.json",
    "config.toml",
    "history.jsonl",
    "CLAUDE.md",
    "AGENTS.md",
];

/// Claude Code's generated caches: rebuilt or simply unneeded after the
/// session that wrote them ends.
const CLAUDE_CACHE_DIRS: &[&str] = &["paste-cache", "image-cache", "shell-snapshots", "statsig"];

/// VS Code-family editors whose `User/` directory has the same layout.
/// (agent id, application directory name, display name)
const EDITORS: &[(&str, &str, &str)] = &[
    ("cursor", "Cursor", "Cursor"),
    ("vscode", "Code", "VS Code"),
    ("windsurf", "Windsurf", "Windsurf"),
];

pub struct AgentDataScanner {
    home: PathBuf,
    config: AppConfig,
    state_db_threshold: u64,
}

impl AgentDataScanner {
    /// A scanner rooted at `home` — the real home in the app, a temp dir in
    /// tests and the sandbox.
    pub fn new(home: PathBuf, config: &AppConfig) -> Self {
        Self {
            home,
            config: config.clone(),
            state_db_threshold: STATE_DB_VACUUM_THRESHOLD_BYTES,
        }
    }

    /// Override the size above which an editor state database is offered a
    /// vacuum. Tests use this: sizes are allocation-aware, so a sparse
    /// "100 MB" fixture would not look large.
    pub fn with_state_db_threshold(mut self, bytes: u64) -> Self {
        self.state_db_threshold = bytes;
        self
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            home: self.home.clone(),
            retention_days: self.config.ai.agent_data_retention_days,
            large_bytes: self
                .config
                .ai
                .large_session_file_mb
                .saturating_mul(1024 * 1024),
            state_db_threshold: self.state_db_threshold,
        }
    }
}

/// Every editor `User/` directory candidate under `home`, per platform layout.
///
/// All layouts are checked on every platform (whichever exists wins), so a
/// fixture built on one OS scans the same on another.
fn editor_user_dirs(home: &Path) -> Vec<(&'static str, &'static str, PathBuf)> {
    let bases = ["Library/Application Support", ".config", "AppData/Roaming"];
    let mut out = Vec::new();
    for (agent, app, display) in EDITORS {
        for base in bases {
            out.push((*agent, *display, home.join(base).join(app).join("User")));
        }
    }
    out
}

#[async_trait]
impl EcosystemScanner for AgentDataScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::AgentData
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        // Purely global: agent data lives at fixed locations under home.
        false
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        Ok(self.analyze_many(path).await?.into_iter().next())
    }

    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        let ctx = self.ctx();
        let root = path.to_path_buf();
        Ok(tokio::task::spawn_blocking(move || ctx.scan_root(&root))
            .await
            .unwrap_or_default())
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs: Vec<PathBuf> = [".claude", ".codex"]
            .iter()
            .map(|rel| self.home.join(rel))
            .filter(|p| p.is_dir())
            .collect();
        locs.extend(
            editor_user_dirs(&self.home)
                .into_iter()
                .map(|(_, _, p)| p)
                .filter(|p| p.is_dir()),
        );
        locs
    }
}

/// The scanner's settings, detached from `self` so the filesystem work can
/// run on a blocking thread.
#[derive(Clone)]
struct Ctx {
    home: PathBuf,
    retention_days: u64,
    large_bytes: u64,
    state_db_threshold: u64,
}

/// One file or directory offered for removal.
struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

impl Ctx {
    fn scan_root(&self, root: &Path) -> Vec<CleanableItem> {
        if root == self.home.join(".claude") {
            return self.scan_claude(root);
        }
        if root == self.home.join(".codex") {
            return self.scan_codex(root);
        }
        for (agent, display, user_dir) in editor_user_dirs(&self.home) {
            if root == user_dir {
                return self.scan_editor(root, agent, display);
            }
        }
        Vec::new()
    }

    fn retention_cutoff(&self) -> SystemTime {
        cutoff(self.retention_days)
    }

    // ── Claude Code ────────────────────────────────────────────────────────

    fn scan_claude(&self, root: &Path) -> Vec<CleanableItem> {
        let mut items = Vec::new();
        let cleanup = read_cleanup_period(&root.join("settings.json"));

        let projects = root.join("projects");
        for project in list_dirs(&projects) {
            items.extend(self.claude_project(root, &project, cleanup));
        }

        let old = self.retention_cutoff();
        let retention = self.retention_days;

        // file-history: per-session snapshots of every file Claude edited.
        let (files, dirs) = old_entries(root, &root.join("file-history"), old);
        for (paths, is_dir) in [(files, false), (dirs, true)] {
            items.extend(self.batch_item(Batch {
                root,
                container: &root.join("file-history"),
                entries: paths,
                dirs: is_dir,
                kind: ArtifactKind::AgentFileHistory,
                risk: RiskLevel::Caution,
                agent: "claude",
                name: "Claude Code file history".into(),
                label: "Remove old file-history snapshots".into(),
                description: format!(
                    "Delete pre-edit file snapshots older than {retention} days. \
                     Claude Code uses them to rewind edits in those sessions."
                ),
                details: vec![Detail::new(
                    "Kept",
                    format!("Snapshots from the last {retention} days"),
                )],
            }));
        }

        // debug/: plain diagnostic logs.
        let (files, _) = old_entries(root, &root.join("debug"), old);
        items.extend(self.batch_item(Batch {
            root,
            container: &root.join("debug"),
            entries: files,
            dirs: false,
            kind: ArtifactKind::AgentDebugLogs,
            risk: RiskLevel::Safe,
            agent: "claude",
            name: "Claude Code debug logs".into(),
            label: "Remove old debug logs".into(),
            description: format!(
                "Delete Claude Code debug logs older than {retention} days. \
                 Only useful when filing a bug report."
            ),
            details: vec![Detail::new(
                "Kept",
                format!("Logs from the last {retention} days"),
            )],
        }));

        // Caches: safe to drop once the session that wrote them is over.
        let cache_cutoff = cutoff(CACHE_MIN_AGE_DAYS);
        for name in CLAUDE_CACHE_DIRS {
            let container = root.join(name);
            let (files, dirs) = old_entries(root, &container, cache_cutoff);
            for (paths, is_dir) in [(files, false), (dirs, true)] {
                items.extend(self.batch_item(Batch {
                    root,
                    container: &container,
                    entries: paths,
                    dirs: is_dir,
                    kind: ArtifactKind::AgentCache,
                    risk: RiskLevel::Safe,
                    agent: "claude",
                    name: format!("Claude Code {name}"),
                    label: format!("Clear {name}"),
                    description: format!(
                        "Delete Claude Code's {name} entries older than {CACHE_MIN_AGE_DAYS} day. \
                         Regenerated as needed."
                    ),
                    details: vec![Detail::new(
                        "Kept",
                        "Entries from the last day (may belong to a running session)",
                    )],
                }));
            }
        }

        items
    }

    /// Old transcripts in one `projects/<encoded-path>/` directory, plus the
    /// `<session-uuid>/` sidecar directories (subagent transcripts, tool
    /// results) of sessions that are old or already gone.
    fn claude_project(
        &self,
        root: &Path,
        project: &Path,
        cleanup: CleanupPeriod,
    ) -> Vec<CleanableItem> {
        let old = self.retention_cutoff();
        let retention = self.retention_days;
        let encoded = file_name(project);
        let (label, decoded) = decode_project_dir(&encoded, &self.home);

        let mut old_sessions = Vec::new();
        let mut kept = 0usize;
        let mut large = Vec::new();
        let mut recent_ids = std::collections::HashSet::new();

        for (path, meta) in list_entries(project) {
            if !meta.is_file() || path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            if !is_offerable(root, &path) {
                continue;
            }
            let size = staleness::on_disk_len(&meta);
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if self.large_bytes > 0 && size > self.large_bytes {
                large.push((path.clone(), size, modified < old));
            }
            if modified < old {
                old_sessions.push(Entry {
                    path,
                    size,
                    modified,
                });
            } else {
                kept += 1;
                if let Some(stem) = path.file_stem() {
                    recent_ids.insert(stem.to_string_lossy().to_string());
                }
            }
        }

        // Sidecars: kept while their main transcript is recent; otherwise
        // offered once everything inside them is old too. Claude Code's own
        // cleanup misses these, so they are often orphaned.
        let mut sidecars = Vec::new();
        for (path, meta) in list_entries(project) {
            let name = file_name(&path);
            if !meta.is_dir() || !looks_like_uuid(&name) || recent_ids.contains(&name) {
                continue;
            }
            if !is_offerable(root, &path) {
                continue;
            }
            let modified = newest_mtime_tree(&path).unwrap_or(SystemTime::UNIX_EPOCH);
            if modified < old {
                sidecars.push(Entry {
                    size: staleness::compute_dir_size_sync(&path),
                    path,
                    modified,
                });
            }
        }

        let project_root = decoded.filter(|p| p.is_dir());
        let mut items = Vec::new();

        if !old_sessions.is_empty() {
            let mut details = vec![
                Detail::new("Agent", "Claude Code"),
                Detail::new("Project", label.clone()),
                Detail::new("Old sessions", old_sessions.len().to_string()),
                Detail::new(
                    "Kept",
                    format!("{kept} session(s) newer than {retention} days"),
                ),
            ];
            details.extend(cleanup.details());
            for (path, size, is_old) in &large {
                details.push(Detail::new(
                    "Large session",
                    format!(
                        "{} — {}{}",
                        file_name(path),
                        ByteSize(*size),
                        if *is_old { "" } else { " (recent, kept)" }
                    ),
                ));
            }
            items.extend(self.batch_item(Batch {
                root,
                container: project,
                entries: old_sessions,
                dirs: false,
                kind: ArtifactKind::AgentTranscripts,
                risk: RiskLevel::Caution,
                agent: "claude",
                name: label.clone(),
                label: "Remove old transcripts".into(),
                description: format!(
                    "Delete Claude Code conversation transcripts older than {retention} days \
                     for this project. Those sessions can no longer be resumed."
                ),
                details,
            }));
        }

        if !sidecars.is_empty() {
            items.extend(self.batch_item(Batch {
                root,
                container: project,
                entries: sidecars,
                dirs: true,
                kind: ArtifactKind::AgentTranscripts,
                risk: RiskLevel::Caution,
                agent: "claude",
                name: label.clone(),
                label: "Remove old subagent transcripts".into(),
                description: format!(
                    "Delete subagent transcripts and tool results of sessions older than \
                     {retention} days. Claude Code's own cleanup does not remove these."
                ),
                details: vec![
                    Detail::new("Agent", "Claude Code"),
                    Detail::new("Project", label),
                    Detail::new(
                        "Kept",
                        format!(
                            "Sidecars of sessions newer than {retention} days, and any \
                             with recent activity"
                        ),
                    ),
                ],
            }));
        }

        if let Some(pr) = project_root {
            for item in &mut items {
                item.project_root = Some(pr.clone());
            }
        }
        items
    }

    // ── Codex ──────────────────────────────────────────────────────────────

    fn scan_codex(&self, root: &Path) -> Vec<CleanableItem> {
        let old = self.retention_cutoff();
        let retention = self.retention_days;
        let mut items = Vec::new();

        for (sub, archived) in [("sessions", false), ("archived_sessions", true)] {
            let container = root.join(sub);
            let mut files = Vec::new();
            collect_files(&container, 5, &mut files);

            let mut old_files = Vec::new();
            let mut kept = 0usize;
            let mut large = Vec::new();
            for (path, meta) in files {
                if path.extension().is_none_or(|e| e != "jsonl") || !is_offerable(root, &path) {
                    continue;
                }
                let size = staleness::on_disk_len(&meta);
                let Ok(modified) = meta.modified() else {
                    continue;
                };
                if self.large_bytes > 0 && size > self.large_bytes {
                    large.push((path.clone(), size, modified < old));
                }
                if modified < old {
                    old_files.push(Entry {
                        path,
                        size,
                        modified,
                    });
                } else {
                    kept += 1;
                }
            }
            if old_files.is_empty() {
                continue;
            }

            let what = if archived {
                "archived sessions"
            } else {
                "sessions"
            };
            let mut details = vec![
                Detail::new("Agent", "Codex"),
                Detail::new("Old rollouts", old_files.len().to_string()),
                Detail::new(
                    "Kept",
                    format!("{kept} rollout(s) newer than {retention} days"),
                ),
            ];
            if archived {
                details.push(Detail::new("Archived", "yes — archived in Codex"));
            }
            for (path, size, is_old) in &large {
                details.push(Detail::new(
                    "Large session",
                    format!(
                        "{} — {}{}",
                        file_name(path),
                        ByteSize(*size),
                        if *is_old { "" } else { " (recent, kept)" }
                    ),
                ));
            }
            items.extend(self.batch_item(Batch {
                root,
                container: &container,
                entries: old_files,
                dirs: false,
                kind: ArtifactKind::AgentTranscripts,
                risk: RiskLevel::Caution,
                agent: "codex",
                name: format!("Codex {what}"),
                label: format!("Remove old {what}"),
                description: format!(
                    "Delete Codex rollout files ({what}) older than {retention} days. \
                     Those sessions can no longer be resumed."
                ),
                details,
            }));
        }
        items
    }

    // ── Cursor / VS Code / Windsurf ────────────────────────────────────────

    fn scan_editor(&self, root: &Path, agent: &str, display: &str) -> Vec<CleanableItem> {
        let mut items = Vec::new();
        let global = root.join("globalStorage");

        let db = global.join("state.vscdb");
        if let Ok(meta) = std::fs::symlink_metadata(&db) {
            let size = staleness::on_disk_len(&meta);
            if meta.is_file() && size > self.state_db_threshold {
                items.push(self.vacuum_item(&db, &meta, size, agent, display));
            }
        }

        let backup = global.join("state.vscdb.backup");
        if let Ok(meta) = std::fs::symlink_metadata(&backup) {
            let size = staleness::on_disk_len(&meta);
            if meta.is_file() && size > 0 {
                items.push(item(
                    &backup,
                    ArtifactKind::EditorStateDb,
                    RiskLevel::Caution,
                    size,
                    meta.modified().ok(),
                    agent,
                    format!("{display} state backup"),
                    vec![CleanAction {
                        id: Uuid::new_v4(),
                        label: "Remove state backup".into(),
                        description: format!(
                            "Delete {display}'s backup copy of its state database. \
                             The live database, including chat history, is kept."
                        ),
                        method: ActionMethod::RemoveFile {
                            path: backup.clone(),
                        },
                        risk: RiskLevel::Caution,
                        estimated_savings_bytes: size,
                    }],
                    vec![
                        Detail::new("Kept", "state.vscdb (the live database)"),
                        Detail::new("Note", format!("{display} writes a new backup later")),
                    ],
                ));
            }
        }

        items.extend(self.workspace_storage(root, agent, display));
        items
    }

    fn vacuum_item(
        &self,
        db: &Path,
        meta: &std::fs::Metadata,
        size: u64,
        agent: &str,
        display: &str,
    ) -> CleanableItem {
        item(
            db,
            ArtifactKind::EditorStateDb,
            RiskLevel::Caution,
            size,
            meta.modified().ok(),
            agent,
            format!("{display} state database"),
            vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Compact (SQLite VACUUM)".into(),
                description: format!(
                    "Rebuild {display}'s state database without its free pages. \
                     Quit {display} first. Chat history is preserved."
                ),
                method: ActionMethod::Command {
                    program: "sqlite3".into(),
                    args: vec![db.to_string_lossy().to_string(), "VACUUM;".into()],
                    working_dir: None,
                },
                risk: RiskLevel::Caution,
                // How much a vacuum reclaims depends on the free-page count,
                // which only SQLite knows. Zero is honest.
                estimated_savings_bytes: 0,
            }],
            vec![
                Detail::new("Agent", display),
                Detail::new("Before you run it", format!("Quit {display} first")),
                Detail::new(
                    "Never deleted",
                    "Deleting state.vscdb erases chat history permanently; Void only compacts it",
                ),
                Detail::new("Savings", "Unknown until SQLite runs — often large"),
                Detail::new("Requires", "sqlite3 on PATH"),
            ],
        )
    }

    /// `workspaceStorage/<hash>/` directories whose workspace folder no
    /// longer exists — one per project ever opened, never cleaned up.
    fn workspace_storage(&self, root: &Path, agent: &str, display: &str) -> Vec<CleanableItem> {
        let container = root.join("workspaceStorage");
        let mut orphans = Vec::new();
        let mut live: Vec<(String, u64)> = Vec::new();

        for dir in list_dirs(&container) {
            if !is_offerable(root, &dir) {
                continue;
            }
            let target = read_workspace_target(&dir.join("workspace.json"));
            match target {
                // Unreadable or remote workspaces are never called orphans.
                None => continue,
                Some(folder) if folder.exists() => {
                    live.push((
                        folder.display().to_string(),
                        staleness::compute_dir_size_sync(&dir),
                    ));
                }
                Some(folder) => {
                    let size = staleness::compute_dir_size_sync(&dir);
                    let modified = newest_mtime_tree(&dir).unwrap_or(SystemTime::UNIX_EPOCH);
                    orphans.push((
                        Entry {
                            path: dir,
                            size,
                            modified,
                        },
                        folder,
                    ));
                }
            }
        }
        if orphans.is_empty() {
            return Vec::new();
        }

        let mut details = vec![
            Detail::new("Agent", display),
            Detail::new("Orphaned workspaces", orphans.len().to_string()),
            Detail::new(
                "Kept",
                format!("{} workspace(s) whose folder still exists", live.len()),
            ),
        ];
        orphans.sort_by_key(|(e, _)| std::cmp::Reverse(e.size));
        for (entry, folder) in orphans.iter().take(LARGEST_WORKSPACES_SHOWN) {
            details.push(Detail::new(
                "Missing folder",
                format!("{} — {}", folder.display(), ByteSize(entry.size)),
            ));
        }
        live.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
        for (folder, size) in live.iter().take(LARGEST_WORKSPACES_SHOWN) {
            details.push(Detail::new(
                "Largest live workspace",
                format!("{folder} — {} (kept)", ByteSize(*size)),
            ));
        }

        self.batch_item(Batch {
            root,
            container: &container,
            entries: orphans.into_iter().map(|(e, _)| e).collect(),
            dirs: true,
            kind: ArtifactKind::EditorWorkspaceStorage,
            risk: RiskLevel::Caution,
            agent,
            name: format!("{display} orphaned workspaces"),
            label: "Remove orphaned workspace storage".into(),
            description: format!(
                "Delete {display}'s per-workspace state (including its chat history) for \
                 folders that no longer exist."
            ),
            details,
        })
        .into_iter()
        .collect()
    }

    /// One item that removes `entries` with a single `RemoveFiles` or
    /// `RemoveDirs` action; `None` when there is nothing to remove.
    fn batch_item(&self, batch: Batch<'_>) -> Option<CleanableItem> {
        // Defense in depth: every path must sit inside the agent root and be
        // none of the protected names, whatever the caller collected.
        let entries: Vec<Entry> = batch
            .entries
            .into_iter()
            .filter(|e| is_offerable(batch.root, &e.path))
            .collect();
        if entries.is_empty() {
            return None;
        }
        let size: u64 = entries.iter().map(|e| e.size).sum();
        let newest = entries.iter().map(|e| e.modified).max();
        let paths: Vec<PathBuf> = entries.into_iter().map(|e| e.path).collect();
        let count = paths.len();
        let method = if batch.dirs {
            ActionMethod::RemoveDirs { paths }
        } else {
            ActionMethod::RemoveFiles { paths }
        };
        debug!(
            "Agent data: {} {} entries in {} ({})",
            batch.agent,
            count,
            batch.container.display(),
            ByteSize(size)
        );
        Some(item(
            batch.container,
            batch.kind,
            batch.risk,
            size,
            newest,
            batch.agent,
            batch.name,
            vec![CleanAction {
                id: Uuid::new_v4(),
                label: batch.label,
                description: batch.description,
                method,
                risk: batch.risk,
                estimated_savings_bytes: size,
            }],
            batch.details,
        ))
    }
}

/// Everything `batch_item` needs, named so call sites stay readable.
struct Batch<'a> {
    root: &'a Path,
    container: &'a Path,
    entries: Vec<Entry>,
    dirs: bool,
    kind: ArtifactKind,
    risk: RiskLevel,
    agent: &'a str,
    name: String,
    label: String,
    description: String,
    details: Vec<Detail>,
}

#[allow(clippy::too_many_arguments)]
fn item(
    path: &Path,
    kind: ArtifactKind,
    risk: RiskLevel,
    size: u64,
    modified: Option<SystemTime>,
    agent: &str,
    name: String,
    actions: Vec<CleanAction>,
    details: Vec<Detail>,
) -> CleanableItem {
    let last_modified = modified.map(DateTime::<Utc>::from);
    CleanableItem {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::AgentData,
        kind,
        risk,
        size_bytes: size,
        size_display: ByteSize(size).to_string(),
        last_modified,
        days_stale: last_modified.map(staleness::days_since),
        project_name: Some(name),
        project_root: None,
        available_actions: actions,
        details,
        agent: Some(agent.to_string()),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn cutoff(days: u64) -> SystemTime {
    SystemTime::now()
        .checked_sub(Duration::from_secs(days.saturating_mul(86_400)))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Direct children with their (non-followed) metadata. Symlinks are skipped:
/// following one could lead outside the agent root.
fn list_entries(dir: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<_> = read
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let meta = std::fs::symlink_metadata(&path).ok()?;
            (!meta.file_type().is_symlink()).then_some((path, meta))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    list_entries(dir)
        .into_iter()
        .filter(|(_, m)| m.is_dir())
        .map(|(p, _)| p)
        .collect()
}

/// Regular files under `dir`, at most `depth` levels down.
fn collect_files(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, std::fs::Metadata)>) {
    for (path, meta) in list_entries(dir) {
        if meta.is_file() {
            out.push((path, meta));
        } else if meta.is_dir() && depth > 0 {
            collect_files(&path, depth - 1, out);
        }
    }
}

/// Direct children of `container` older than `cutoff`, split into files and
/// directories (a directory's age is that of the newest thing inside it).
fn old_entries(root: &Path, container: &Path, cutoff: SystemTime) -> (Vec<Entry>, Vec<Entry>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for (path, meta) in list_entries(container) {
        if !is_offerable(root, &path) {
            continue;
        }
        if meta.is_file() {
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if modified < cutoff {
                files.push(Entry {
                    size: staleness::on_disk_len(&meta),
                    path,
                    modified,
                });
            }
        } else if meta.is_dir() {
            let modified = newest_mtime_tree(&path).unwrap_or(SystemTime::UNIX_EPOCH);
            if modified < cutoff {
                dirs.push(Entry {
                    size: staleness::compute_dir_size_sync(&path),
                    path,
                    modified,
                });
            }
        }
    }
    (files, dirs)
}

/// Newest modification time of `path` or anything beneath it.
fn newest_mtime_tree(path: &Path) -> Option<SystemTime> {
    ignore::WalkBuilder::new(path)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

/// True when `path` is strictly inside `root` and touches none of the
/// protected names. The safety checker refuses these too; filtering here
/// means they are never even offered.
fn is_offerable(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    if rel.as_os_str().is_empty() {
        return false;
    }
    let comps: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if comps
        .iter()
        .any(|c| c == ".." || PROTECTED_DIRS.contains(&c.as_str()))
    {
        return false;
    }
    !comps
        .last()
        .is_some_and(|name| PROTECTED_FILES.contains(&name.as_str()))
}

fn looks_like_uuid(name: &str) -> bool {
    name.len() == 36 && Uuid::parse_str(name).is_ok()
}

/// Claude Code's `cleanupPeriodDays`, as found in `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupPeriod {
    /// Not set (or settings unreadable): Claude Code uses its default.
    Default,
    Days(i64),
}

impl CleanupPeriod {
    fn details(self) -> Vec<Detail> {
        match self {
            Self::Default => vec![Detail::new(
                "Claude Code cleanupPeriodDays",
                format!("{CLAUDE_DEFAULT_CLEANUP_DAYS} (default — not set in settings.json)"),
            )],
            Self::Days(0) => vec![
                Detail::new("Claude Code cleanupPeriodDays", "0"),
                Detail::new(
                    "Warning",
                    "cleanupPeriodDays is 0 — Claude Code silently stops saving new \
                     transcripts. Set it to a positive number of days in ~/.claude/settings.json.",
                ),
            ],
            Self::Days(n) => vec![Detail::new("Claude Code cleanupPeriodDays", n.to_string())],
        }
    }
}

/// Read `cleanupPeriodDays` from Claude Code's settings. Read-only; a missing
/// or invalid file is treated as "not set".
fn read_cleanup_period(settings: &Path) -> CleanupPeriod {
    std::fs::read_to_string(settings)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("cleanupPeriodDays")?.as_i64())
        .map_or(CleanupPeriod::Default, CleanupPeriod::Days)
}

/// Turn a Claude Code project directory name back into a path.
///
/// Claude Code encodes the project's absolute path by replacing `/` (and `.`)
/// with `-`, which is lossy: `my-app` and `my/app` encode the same. Decoding
/// therefore walks the real filesystem, joining tokens with `-` whenever that
/// names an existing directory. Returns a display label (`~/code/app`) and the
/// best-guess path.
fn decode_project_dir(encoded: &str, home: &Path) -> (String, Option<PathBuf>) {
    let home_encoded = encode_path(home);
    let (base, rest, tilde) = match encoded.strip_prefix(&home_encoded) {
        Some("") => return ("~".into(), Some(home.to_path_buf())),
        Some(rest) if rest.starts_with('-') => (home.to_path_buf(), rest, true),
        _ => (PathBuf::from("/"), encoded, false),
    };
    let tokens: Vec<&str> = rest.split('-').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return (encoded.to_string(), None);
    }
    let mut segments: Vec<String> = Vec::new();
    for token in tokens {
        if let Some(last) = segments.last() {
            // Joining with `-` wins only when that names something real: the
            // hyphen was part of a directory name, not a separator.
            let parent = base.join(segments[..segments.len() - 1].join("/"));
            let merged = format!("{last}-{token}");
            if parent.join(&merged).exists() {
                if let Some(l) = segments.last_mut() {
                    *l = merged;
                }
                continue;
            }
        }
        segments.push(token.to_string());
    }
    let rel = segments.join("/");
    let path = base.join(&rel);
    let label = if tilde {
        format!("~/{rel}")
    } else {
        path.display().to_string()
    };
    (label, Some(path))
}

/// Claude Code's project-directory encoding of an absolute path.
fn encode_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if c == '/' || c == '.' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// The local folder a `workspaceStorage/<hash>/workspace.json` points at.
///
/// `None` for remote workspaces (`vscode-remote://…`) and unreadable files:
/// Void cannot tell whether those still exist, so they are never orphans.
fn read_workspace_target(workspace_json: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(workspace_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let uri = value
        .get("folder")
        .or_else(|| value.get("workspace"))?
        .as_str()?;
    file_uri_to_path(uri)
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Authority (host) is empty for local files.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest)?;
    // Windows: file:///c:/Users/... → c:/Users/...
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic() {
        return Some(PathBuf::from(&decoded[1..]));
    }
    if !decoded.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode_to_paths() {
        assert_eq!(
            file_uri_to_path("file:///Users/me/My%20Project"),
            Some(PathBuf::from("/Users/me/My Project"))
        );
        assert_eq!(
            file_uri_to_path("file:///c%3A/Users/me/app"),
            Some(PathBuf::from("c:/Users/me/app"))
        );
        assert_eq!(
            file_uri_to_path("vscode-remote://ssh-remote+box/home/me"),
            None
        );
        assert_eq!(file_uri_to_path("file:///bad%zz"), None);
    }

    #[test]
    fn protected_names_are_never_offerable() {
        let root = Path::new("/h/.claude");
        assert!(is_offerable(root, Path::new("/h/.claude/debug/a.txt")));
        assert!(!is_offerable(root, Path::new("/h/.claude")));
        assert!(!is_offerable(root, Path::new("/h/.claude/settings.json")));
        assert!(!is_offerable(root, Path::new("/h/.claude/memory/x.md")));
        assert!(!is_offerable(
            root,
            Path::new("/h/.claude/projects/p/memory/MEMORY.md")
        ));
        assert!(!is_offerable(
            root,
            Path::new("/h/.claude/projects/p/CLAUDE.md")
        ));
        assert!(!is_offerable(root, Path::new("/h/.claude/history.jsonl")));
        assert!(!is_offerable(root, Path::new("/h/.codex/auth.json")));
        assert!(!is_offerable(root, Path::new("/h/.claude/../.ssh/id")));
    }

    #[test]
    fn uuid_names_are_recognised() {
        assert!(looks_like_uuid("0b5e8f3a-1c2d-4e5f-8a9b-0c1d2e3f4a5b"));
        assert!(!looks_like_uuid("memory"));
        assert!(!looks_like_uuid("0b5e8f3a1c2d4e5f8a9b0c1d2e3f4a5b"));
    }

    #[test]
    fn project_dirs_decode_against_the_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("code/my-app")).unwrap();
        std::fs::create_dir_all(home.join("code/plain")).unwrap();

        let enc = |rel: &str| encode_path(&home.join(rel));
        let (label, path) = decode_project_dir(&enc("code/my-app"), &home);
        assert_eq!(label, "~/code/my-app");
        assert_eq!(path, Some(home.join("code/my-app")));

        let (label, _) = decode_project_dir(&enc("code/plain"), &home);
        assert_eq!(label, "~/code/plain");

        // A project that no longer exists still gets a readable label.
        let (label, _) = decode_project_dir(&enc("gone/app"), &home);
        assert_eq!(label, "~/gone/app");

        let (label, _) = decode_project_dir("-tmp-x", &home);
        assert_eq!(label, "/tmp/x");
    }

    #[test]
    fn cleanup_period_is_read_tolerantly() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("settings.json");
        assert_eq!(read_cleanup_period(&p), CleanupPeriod::Default);
        std::fs::write(&p, "not json").unwrap();
        assert_eq!(read_cleanup_period(&p), CleanupPeriod::Default);
        std::fs::write(&p, r#"{"cleanupPeriodDays": 0}"#).unwrap();
        assert_eq!(read_cleanup_period(&p), CleanupPeriod::Days(0));
        assert!(CleanupPeriod::Days(0)
            .details()
            .iter()
            .any(|d| d.label == "Warning"));
    }
}
