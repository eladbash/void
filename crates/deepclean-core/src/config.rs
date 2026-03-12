use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::Ecosystem;

/// Application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Root directories to scan for projects.
    pub scan_roots: Vec<PathBuf>,
    /// Paths to never touch.
    pub blocked_paths: Vec<PathBuf>,
    /// Which ecosystems to scan.
    pub enabled_ecosystems: Vec<Ecosystem>,
    /// Days since last modification before an artifact is considered stale.
    pub staleness_threshold_days: u64,
    /// Max CPU usage percentage for background scanning.
    pub cpu_threshold_percent: u8,
    /// Number of threads for the parallel directory walker.
    pub walker_threads: usize,
    /// Max concurrent analysis tasks.
    pub max_concurrent_analyses: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        Self {
            scan_roots: vec![home.clone()],
            blocked_paths: Vec::new(),
            enabled_ecosystems: vec![
                Ecosystem::Rust,
                Ecosystem::Node,
                Ecosystem::Apple,
                Ecosystem::Docker,
                Ecosystem::Go,
                Ecosystem::System,
                Ecosystem::Python,
                Ecosystem::Java,
                Ecosystem::Homebrew,
                Ecosystem::JetBrains,
                Ecosystem::DotNet,
            ],
            staleness_threshold_days: 30,
            cpu_threshold_percent: 70,
            walker_threads: (cpus / 2).max(1),
            max_concurrent_analyses: 8,
        }
    }
}
