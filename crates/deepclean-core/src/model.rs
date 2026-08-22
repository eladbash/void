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
}

impl Ecosystem {
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
    /// The channel closing is not a usable completion signal — the frontend
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
