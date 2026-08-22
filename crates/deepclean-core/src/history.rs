use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Ecosystem;

/// How a single item in a clean run turned out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RunOutcome {
    Succeeded,
    Failed { error: String },
}

/// One item acted on during a clean run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunItem {
    pub path: PathBuf,
    pub ecosystem: Ecosystem,
    pub action_label: String,
    /// The literal command, or a description of the filesystem operation.
    pub method_summary: String,
    /// Bytes the executor actually measured. Zero for commands and Docker prunes,
    /// which cannot report what they removed.
    pub bytes_freed: u64,
    /// The action's own estimate, used when `bytes_freed` is unavailable.
    pub estimated_bytes: u64,
    #[serde(flatten)]
    pub result: RunOutcome,
}

impl RunItem {
    /// Best available figure for how much this item freed.
    ///
    /// Prefers the measured value; falls back to the estimate for methods that
    /// return zero on success.
    pub fn effective_bytes(&self) -> u64 {
        match self.result {
            RunOutcome::Succeeded => {
                if self.bytes_freed > 0 {
                    self.bytes_freed
                } else {
                    self.estimated_bytes
                }
            }
            RunOutcome::Failed { .. } => 0,
        }
    }

    /// Whether this item's contribution is an estimate rather than a measurement.
    pub fn is_estimated(&self) -> bool {
        matches!(self.result, RunOutcome::Succeeded)
            && self.bytes_freed == 0
            && self.estimated_bytes > 0
    }
}

/// A completed clean run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanRun {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub items: Vec<RunItem>,
}

impl CleanRun {
    pub fn total(&self) -> usize {
        self.items.len()
    }

    pub fn succeeded(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.result, RunOutcome::Succeeded))
            .count()
    }

    pub fn failed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.result, RunOutcome::Failed { .. }))
            .count()
    }

    /// Total freed, measured where possible and estimated otherwise.
    pub fn bytes_freed(&self) -> u64 {
        self.items.iter().map(RunItem::effective_bytes).sum()
    }

    /// True when any contributing item could only be estimated, which means the
    /// run total must be presented as an estimate.
    pub fn is_estimated(&self) -> bool {
        self.items.iter().any(RunItem::is_estimated)
    }
}

/// Persisted list of clean runs, newest first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub runs: Vec<CleanRun>,
}

impl History {
    pub const FILE_NAME: &'static str = "history.json";

    /// Keep the file bounded. At one run per day this is roughly two years.
    pub const MAX_RUNS: usize = 500;

    /// Read history from `path`. A missing file is an empty history, not an error.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None)
            }
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!("could not read {}: {err}", path.display())),
                )
            }
        };

        match serde_json::from_str(&raw) {
            Ok(history) => (history, None),
            Err(err) => (
                Self::default(),
                Some(format!("could not parse {}: {err}", path.display())),
            ),
        }
    }

    /// Write history to `path` via a temp file and rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Prepend a run and trim to `MAX_RUNS`.
    pub fn push(&mut self, run: CleanRun) {
        self.runs.insert(0, run);
        self.runs.truncate(Self::MAX_RUNS);
    }

    /// Total reclaimed across every recorded run.
    pub fn total_bytes(&self) -> u64 {
        self.runs.iter().map(CleanRun::bytes_freed).sum()
    }

    /// Total reclaimed in runs started within the last `days`.
    pub fn bytes_since(&self, days: i64) -> u64 {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        self.runs
            .iter()
            .filter(|r| r.started_at >= cutoff)
            .map(CleanRun::bytes_freed)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(bytes: u64, estimated: u64, ok: bool) -> RunItem {
        RunItem {
            path: PathBuf::from("/tmp/x"),
            ecosystem: Ecosystem::Rust,
            action_label: "Remove target/".into(),
            method_summary: "Delete directory recursively".into(),
            bytes_freed: bytes,
            estimated_bytes: estimated,
            result: if ok {
                RunOutcome::Succeeded
            } else {
                RunOutcome::Failed {
                    error: "boom".into(),
                }
            },
        }
    }

    fn run(items: Vec<RunItem>) -> CleanRun {
        CleanRun {
            id: Uuid::new_v4(),
            started_at: Utc::now(),
            duration_ms: 1000,
            items,
        }
    }

    #[test]
    fn missing_history_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (history, err) = History::load(&tmp.path().join("nope.json"));
        assert!(err.is_none());
        assert!(history.runs.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("history.json");

        let mut history = History::default();
        history.push(run(vec![item(4096, 0, true)]));
        history.save(&path).unwrap();

        let (loaded, err) = History::load(&path);
        assert!(err.is_none());
        assert_eq!(loaded.runs.len(), 1);
        assert_eq!(loaded.runs[0].bytes_freed(), 4096);
    }

    #[test]
    fn falls_back_to_estimate_when_nothing_was_measured() {
        // `cargo clean` and `docker prune` always report zero bytes freed.
        let r = run(vec![item(0, 3_800_000_000, true)]);
        assert_eq!(r.bytes_freed(), 3_800_000_000);
        assert!(r.is_estimated(), "total must be flagged as an estimate");
    }

    #[test]
    fn prefers_measurement_over_estimate() {
        let r = run(vec![item(4096, 999_999, true)]);
        assert_eq!(r.bytes_freed(), 4096);
        assert!(!r.is_estimated());
    }

    #[test]
    fn failed_items_contribute_nothing() {
        let r = run(vec![item(4096, 0, true), item(8192, 1024, false)]);
        assert_eq!(r.bytes_freed(), 4096);
        assert_eq!(r.succeeded(), 1);
        assert_eq!(r.failed(), 1);
        assert_eq!(r.total(), 2);
    }

    #[test]
    fn newest_run_is_first_and_list_is_bounded() {
        let mut history = History::default();
        for _ in 0..(History::MAX_RUNS + 25) {
            history.push(run(vec![item(1, 0, true)]));
        }
        assert_eq!(history.runs.len(), History::MAX_RUNS);
    }

    #[test]
    fn bytes_since_excludes_older_runs() {
        let mut history = History::default();

        let mut old = run(vec![item(1000, 0, true)]);
        old.started_at = Utc::now() - chrono::Duration::days(90);
        history.push(old);

        history.push(run(vec![item(500, 0, true)]));

        assert_eq!(history.total_bytes(), 1500);
        assert_eq!(history.bytes_since(30), 500);
    }
}
