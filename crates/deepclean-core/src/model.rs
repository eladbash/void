use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported developer ecosystems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ecosystem {
    Rust,
    Node,
    Apple,
    Docker,
    Go,
    System,
    Python,
    Java,
    Homebrew,
    JetBrains,
    DotNet,
    /// Git worktrees created by AI coding agents (Claude Code, Cursor, Codex,
    /// Conductor…), plus orphaned worktrees and leftover agent branches.
    Worktrees,
    /// Data AI agents and AI editors generate about themselves: transcripts,
    /// debug logs, file-history snapshots, editor state databases.
    AgentData,
    /// Local model stores: Ollama, Hugging Face, LM Studio, torch hub, ComfyUI.
    Models,
    /// Whole projects that look abandoned — no commits, no edits in a long time.
    Projects,
}

impl Ecosystem {
    /// Every ecosystem, in display order.
    pub const ALL: [Ecosystem; 15] = [
        Self::Worktrees,
        Self::AgentData,
        Self::Models,
        Self::Rust,
        Self::Node,
        Self::Python,
        Self::Apple,
        Self::Docker,
        Self::Go,
        Self::Java,
        Self::DotNet,
        Self::Homebrew,
        Self::JetBrains,
        Self::System,
        Self::Projects,
    ];

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Node => "Node.js",
            Self::Apple => "Apple / Xcode",
            Self::Docker => "Docker",
            Self::Go => "Go",
            Self::System => "System",
            Self::Python => "Python",
            Self::Java => "Java / JVM",
            Self::Homebrew => "Homebrew",
            Self::JetBrains => "JetBrains",
            Self::DotNet => ".NET",
            Self::Worktrees => "Agent worktrees",
            Self::AgentData => "AI agent data",
            Self::Models => "Local AI models",
            Self::Projects => "Stale projects",
        }
    }
}

/// How risky a clean action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Caution,
    Danger,
}

/// The kind of artifact found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    // Rust
    TargetDir,
    CargoRegistry,
    CargoGitCheckouts,
    // Node
    NodeModules,
    NpmCache,
    YarnCache,
    PnpmStore,
    // Apple
    DerivedData,
    Archives,
    DeviceSupport,
    CocoaPodsCache,
    SwiftPackageCache,
    // Docker
    DanglingImages,
    UnusedImages,
    BuildCache,
    StoppedContainers,
    UnusedVolumes,
    // Go
    GoBuildCache,
    GoModCache,
    GoTestCache,
    // System
    DownloadsDir,
    TrashBin,
    SystemLogs,
    // Python
    PipCache,
    PycacheDir,
    VenvDir,
    CondaCache,
    // Java
    GradleCache,
    MavenRepository,
    GradleBuildDir,
    // Homebrew
    HomebrewCache,
    // JetBrains
    JetBrainsCache,
    // .NET
    NuGetCache,
    DotNetBinObj,
    // Apple (additional)
    SimulatorDevices,
    SimulatorCaches,
    SimulatorRuntimes,
    // Python (additional)
    UvCache,
    PythonToolCache,
    // Node (additional)
    BunCache,
    FrameworkBuildCache,
    PlaywrightBrowsers,
    PuppeteerBrowsers,
    // Java (additional)
    MavenTarget,
    // Docker (additional)
    ContainerVmDisk,
    /// Everything `docker system df` reports as reclaimable, as one item.
    DockerData,
    // Worktrees
    /// A live git worktree created by an AI agent.
    AgentWorktree,
    /// A directory under an agent's worktree root whose `.git` link points
    /// at a repository that no longer knows about it.
    OrphanWorktree,
    /// Stale administrative entries git keeps for worktrees that are gone.
    PrunableWorktreeRefs,
    /// Local branches agents created that are fully merged.
    MergedAgentBranches,
    // Agent data
    AgentTranscripts,
    AgentFileHistory,
    AgentDebugLogs,
    AgentCache,
    EditorStateDb,
    EditorWorkspaceStorage,
    // Models
    OllamaModel,
    OllamaOrphanBlobs,
    HuggingFaceModel,
    LmStudioModel,
    TorchHubCache,
    ComfyUiModels,
    DuplicateModelFiles,
    // Projects
    StaleProject,
}

/// A discovered cleanable artifact on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanableItem {
    pub id: Uuid,
    pub path: PathBuf,
    pub ecosystem: Ecosystem,
    pub kind: ArtifactKind,
    pub risk: RiskLevel,
    pub size_bytes: u64,
    pub size_display: String,
    pub last_modified: Option<DateTime<Utc>>,
    pub days_stale: Option<u64>,
    pub project_name: Option<String>,
    pub project_root: Option<PathBuf>,
    pub available_actions: Vec<CleanAction>,
    /// Extra facts shown in the item drawer, in display order — a worktree's
    /// branch and dirty state, a model's tag, a duplicate group's members.
    #[serde(default)]
    pub details: Vec<Detail>,
    /// Which AI tool produced this item, when known ("claude", "cursor",
    /// "codex", "conductor", "ollama", …). Drives the usage-by-agent view.
    #[serde(default)]
    pub agent: Option<String>,
}

impl CleanableItem {
    /// Whether Void can run anything on this item. Informational items (a
    /// locked worktree, a container VM disk image) show size but free nothing.
    pub fn is_actionable(&self) -> bool {
        !self.available_actions.is_empty()
    }
}

/// Bytes a set of results could honestly free: informational items are
/// skipped, and an item nested inside another counted item (a stale
/// project's `node_modules`) is counted once, through its container.
///
/// An outer item only absorbs a nested one it is at least as large as — a
/// zero-byte "prune worktree refs" item at a repo root must not hide the
/// gigabytes of worktrees inside that repo. Mirrors the desktop app's
/// `outermost()` in `model/selection.rs`.
pub fn reclaimable_bytes<'a>(items: impl IntoIterator<Item = &'a CleanableItem>) -> u64 {
    let mut counted: Vec<&CleanableItem> =
        items.into_iter().filter(|i| i.is_actionable()).collect();
    counted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut open: Vec<&CleanableItem> = Vec::new();
    let mut total = 0u64;
    for item in counted {
        while open.last().is_some_and(|a| !item.path.starts_with(&a.path)) {
            open.pop();
        }
        let absorbed = open.iter().any(|a| a.size_bytes >= item.size_bytes);
        if !absorbed {
            total += item.size_bytes;
        }
        open.push(item);
    }
    total
}

#[cfg(test)]
mod reclaimable_tests {
    use super::*;

    fn it(path: &str, size: u64, actionable: bool) -> CleanableItem {
        CleanableItem {
            id: Uuid::new_v4(),
            path: PathBuf::from(path),
            ecosystem: Ecosystem::Node,
            kind: ArtifactKind::NodeModules,
            risk: RiskLevel::Safe,
            size_bytes: size,
            size_display: String::new(),
            last_modified: None,
            days_stale: None,
            project_name: None,
            project_root: None,
            available_actions: if actionable {
                vec![CleanAction {
                    id: Uuid::new_v4(),
                    label: "x".into(),
                    description: String::new(),
                    method: ActionMethod::RemoveDir {
                        path: PathBuf::from(path),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size,
                }]
            } else {
                Vec::new()
            },
            details: Vec::new(),
            agent: None,
        }
    }

    #[test]
    fn informational_items_do_not_count() {
        // The reported bug: Docker.raw (informational) on top of the Docker
        // data inside it.
        let items = [
            it("/var/run/docker.sock", 200, true),
            it(
                "/u/Library/Containers/com.docker.docker/Docker.raw",
                200,
                false,
            ),
        ];
        assert_eq!(reclaimable_bytes(&items), 200);
    }

    #[test]
    fn nested_items_count_once_but_small_parents_do_not_hide_big_children() {
        let items = [
            it("/u/proj", 5000, true),
            it("/u/proj/node_modules", 3000, true),
            it("/u/proj-b/node_modules", 700, true),
            it("/u/repo", 0, true),
            it("/u/repo/.claude/worktrees/a", 900, true),
        ];
        assert_eq!(reclaimable_bytes(&items), 5000 + 700 + 900);
    }
}

/// One labelled fact about an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    pub label: String,
    pub value: String,
}

impl Detail {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// An action that can be performed on a cleanable item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanAction {
    pub id: Uuid,
    pub label: String,
    pub description: String,
    pub method: ActionMethod,
    pub risk: RiskLevel,
    pub estimated_savings_bytes: u64,
}

/// How to execute a clean action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionMethod {
    Command {
        program: String,
        args: Vec<String>,
        working_dir: Option<PathBuf>,
    },
    RemoveDir {
        path: PathBuf,
    },
    RemoveFile {
        path: PathBuf,
    },
    DockerPrune {
        prune_type: String,
    },
    /// Move a file or directory to the system Trash (recoverable).
    MoveToTrash {
        path: PathBuf,
    },
    /// Delete several directories — e.g. the `node_modules`, `.venv` and
    /// `target` inside an idle worktree, leaving the checkout itself intact.
    RemoveDirs {
        paths: Vec<PathBuf>,
    },
    /// Delete several files — e.g. old transcripts or orphaned model blobs.
    RemoveFiles {
        paths: Vec<PathBuf>,
    },
    /// `git worktree remove`, re-verified at execution time: refused if the
    /// worktree has uncommitted or unpushed work, or is locked, unless the
    /// scanner explicitly marked it `force` (never done for dirty trees).
    GitWorktreeRemove {
        repo: PathBuf,
        worktree: PathBuf,
        /// Also delete the worktree's branch with `git branch -d` (which git
        /// itself refuses for unmerged branches).
        delete_branch: Option<String>,
    },
    /// `git worktree prune` — removes only stale administrative entries.
    GitWorktreePrune {
        repo: PathBuf,
    },
    /// `git branch -d` for each branch — git refuses unmerged ones.
    GitDeleteBranches {
        repo: PathBuf,
        branches: Vec<String>,
    },
    /// Delete Ollama blobs no manifest references — re-verified at execution
    /// time: every manifest in `store` is re-read and any blob a model now
    /// uses (a pull that started after the scan) is kept. Refused outright if
    /// any manifest cannot be parsed.
    RemoveOllamaOrphans {
        store: PathBuf,
        blobs: Vec<PathBuf>,
    },
    /// Replace byte-identical duplicates with copy-on-write clones (APFS) or
    /// hardlinks, keeping `keep` untouched. Contents are re-hashed at
    /// execution time; any mismatch aborts that group.
    DedupFiles {
        groups: Vec<DedupGroup>,
    },
}

/// A set of byte-identical files: one to keep, the rest to replace with links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DedupGroup {
    pub keep: PathBuf,
    pub duplicates: Vec<PathBuf>,
    /// Size of one copy; savings are `size_bytes * duplicates.len()`.
    pub size_bytes: u64,
    /// Hex digest recorded at scan time.
    pub hash: String,
}

/// Events emitted during scanning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScanEvent {
    ScannerStarted {
        ecosystem: Ecosystem,
    },
    Progress {
        message: String,
        paths_scanned: u64,
    },
    ItemFound {
        item: CleanableItem,
    },
    ScannerCompleted {
        ecosystem: Ecosystem,
        items_found: usize,
    },
    ScanComplete {
        summary: ScanSummary,
    },
    Error {
        message: String,
    },
    /// A directory could not be read.
    ///
    /// Without this the failure mode is invisible absence: an unreadable
    /// directory computes a size of zero and gets dropped from the results,
    /// so the user sees nothing and is told nothing.
    PermissionDenied {
        path: PathBuf,
    },
}

/// Events emitted during action execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionEvent {
    Started {
        item_id: Uuid,
        action_id: Uuid,
        label: String,
    },
    Completed {
        item_id: Uuid,
        action_id: Uuid,
        /// What the executor measured. Always zero for `Command` and
        /// `DockerPrune`, which cannot observe what they removed.
        bytes_freed: u64,
        /// The action's own estimate, carried so the UI can report an honest
        /// total without re-deriving it from the item list.
        estimated_bytes: u64,
    },
    Failed {
        item_id: Uuid,
        action_id: Uuid,
        error: String,
    },
    /// Terminal event for a batch.
    ///
    /// The channel closing is not a usable completion signal — the UI
    /// would have to count `Started` against terminal events to know it is
    /// done.
    BatchComplete {
        total: usize,
        succeeded: usize,
        failed: usize,
        /// Measured where possible, estimated where not.
        bytes_freed: u64,
        /// True when any contributing action could only be estimated.
        estimated: bool,
        /// True when the batch stopped early because it was cancelled.
        cancelled: bool,
    },
}

/// Summary of a completed scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    pub total_items: usize,
    pub total_bytes: u64,
    pub total_display: String,
    pub ecosystems: Vec<EcosystemSummary>,
    pub scan_duration_ms: u64,
}

/// Per-ecosystem scan summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EcosystemSummary {
    pub ecosystem: Ecosystem,
    pub item_count: usize,
    pub total_bytes: u64,
    pub total_display: String,
}
