pub mod agents;
pub mod apple;
pub mod docker;
pub mod dotnet;
pub mod go;
pub mod homebrew;
pub mod java;
pub mod jetbrains;
pub mod models;
pub mod node;
pub mod projects;
pub mod python;
pub mod registry;
pub mod rust;
pub mod system;
pub mod worktrees;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use ignore::WalkBuilder;
use tokio::sync::{mpsc, Semaphore};
use tracing::{debug, warn};

use crate::config::AppConfig;
use crate::error::ScanError;
use crate::model::*;

/// Trait implemented by each ecosystem scanner.
#[async_trait]
pub trait EcosystemScanner: Send + Sync {
    /// Which ecosystem this scanner handles.
    fn ecosystem(&self) -> Ecosystem;

    /// Fast check during directory walk — is this entry a candidate?
    fn is_candidate(&self, file_name: &str, path: &Path) -> bool;

    /// Analyze a candidate path and produce a cleanable item.
    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError>;

    /// Analyze a candidate that may yield several items — a worktree root
    /// holding five worktrees, a model store holding twelve models.
    ///
    /// The orchestrator always calls this; the default wraps [`Self::analyze`]
    /// so single-item scanners need not know it exists.
    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        Ok(self.analyze(path).await?.into_iter().collect())
    }

    /// Global (non-project) locations to check.
    fn global_locations(&self) -> Vec<PathBuf> {
        vec![]
    }
}

/// Resolve fixed locations under the user's home directory, keeping the ones
/// that exist.
///
/// Seven scanners were each hand-rolling this same loop; the shape is always
/// "a known list of paths relative to home, minus the ones not installed".
pub fn existing_home_dirs<I, S>(relative: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<Path>,
{
    let Some(home) = crate::paths::home_dir() else {
        return Vec::new();
    };
    relative
        .into_iter()
        .map(|rel| home.join(rel))
        .filter(|path| path.is_dir())
        .collect()
}

/// Orchestrates scanning across all ecosystems.
pub struct ScanOrchestrator {
    scanners: Vec<Arc<dyn EcosystemScanner>>,
    config: AppConfig,
}

impl ScanOrchestrator {
    pub fn new(scanners: Vec<Arc<dyn EcosystemScanner>>, config: AppConfig) -> Self {
        Self { scanners, config }
    }

    /// Start scanning and return a channel of scan events.
    pub fn start_scan(self) -> mpsc::Receiver<ScanEvent> {
        let (tx, rx) = mpsc::channel(256);

        tokio::spawn(async move {
            let start = std::time::Instant::now();
            // `.max(1)`: a semaphore with zero permits never returns Err from
            // acquire, it pends forever — the scan would hang, not fail.
            let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_analyses.max(1)));

            // Notify scanners starting
            for scanner in &self.scanners {
                let _ = tx
                    .send(ScanEvent::ScannerStarted {
                        ecosystem: scanner.ecosystem(),
                    })
                    .await;
            }

            // Phase 1: Walk filesystem for project-local artifacts
            let walk_candidates = self.walk_phase(&tx).await;

            // A global location the walk already matched (an agent worktree
            // root under a scan root, say) must not be analyzed twice.
            let walked: HashSet<(Ecosystem, PathBuf)> = walk_candidates
                .iter()
                .map(|(s, p)| (s.ecosystem(), p.clone()))
                .collect();

            // Phase 2: Analyze candidates concurrently
            let mut analyze_handles = vec![];

            for (scanner, path) in walk_candidates {
                let sem = semaphore.clone();
                let tx = tx.clone();
                let handle = tokio::spawn(async move {
                    // Acquire fails only if the semaphore is closed, which
                    // happens when the orchestrator is being torn down. There
                    // is nothing useful to analyze at that point.
                    let Ok(_permit) = sem.acquire().await else {
                        return;
                    };
                    match scanner.analyze_many(&path).await {
                        Ok(items) if !items.is_empty() => {
                            for item in items {
                                let item = crate::trash::add_trash_alternatives(item);
                                let _ = tx.send(ScanEvent::ItemFound { item }).await;
                            }
                        }
                        Ok(_) => {
                            // A directory Void cannot read computes a size of
                            // zero and is dropped as "nothing here". Tell the
                            // difference before it vanishes silently.
                            if is_permission_denied(&path) {
                                let _ = tx
                                    .send(ScanEvent::PermissionDenied { path: path.clone() })
                                    .await;
                            }
                        }
                        Err(e) => {
                            warn!("Analysis error for {}: {}", path.display(), e);
                            let _ = tx
                                .send(ScanEvent::Error {
                                    message: format!("{}: {}", path.display(), e),
                                })
                                .await;
                        }
                    }
                });
                analyze_handles.push(handle);
            }

            // Phase 3: Check global locations
            let mut globals_seen: HashSet<(Ecosystem, PathBuf)> = HashSet::new();
            for scanner in &self.scanners {
                for loc in scanner.global_locations() {
                    let key = (scanner.ecosystem(), loc.clone());
                    if walked.contains(&key) || !globals_seen.insert(key) {
                        continue;
                    }
                    let scanner = scanner.clone();
                    let sem = semaphore.clone();
                    let tx = tx.clone();
                    let handle = tokio::spawn(async move {
                        let Ok(_permit) = sem.acquire().await else {
                            return;
                        };
                        match scanner.analyze_many(&loc).await {
                            Ok(items) if !items.is_empty() => {
                                for item in items {
                                    let item = crate::trash::add_trash_alternatives(item);
                                    let _ = tx.send(ScanEvent::ItemFound { item }).await;
                                }
                            }
                            Ok(_) => {
                                if is_permission_denied(&loc) {
                                    let _ = tx
                                        .send(ScanEvent::PermissionDenied { path: loc.clone() })
                                        .await;
                                }
                            }
                            Err(e) => {
                                warn!("Global analysis error for {}: {}", loc.display(), e);
                            }
                        }
                    });
                    analyze_handles.push(handle);
                }
            }

            // Wait for all analyses to complete
            for handle in analyze_handles {
                let _ = handle.await;
            }

            let elapsed = start.elapsed();
            let _ = tx
                .send(ScanEvent::ScanComplete {
                    summary: ScanSummary {
                        scan_duration_ms: elapsed.as_millis() as u64,
                        ..Default::default()
                    },
                })
                .await;
        });

        rx
    }

    /// Walk configured roots and collect candidate paths matched by scanners.
    async fn walk_phase(
        &self,
        tx: &mpsc::Sender<ScanEvent>,
    ) -> Vec<(Arc<dyn EcosystemScanner>, PathBuf)> {
        let scanners = self.scanners.clone();
        let config = self.config.clone();
        let tx = tx.clone();

        tokio::task::spawn_blocking(move || {
            let mut candidates: Vec<(Arc<dyn EcosystemScanner>, PathBuf)> = vec![];
            let candidates_mutex = std::sync::Mutex::new(&mut candidates);
            let paths_scanned = std::sync::atomic::AtomicU64::new(0);
            // One event per unreadable directory would be thousands on a Mac
            // without Full Disk Access; report distinct locations, bounded.
            let denied_seen: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());

            for root in &config.scan_roots {
                if !root.is_dir() {
                    continue;
                }

                let mut builder = WalkBuilder::new(root);
                builder
                    .hidden(false)
                    .ignore(false)
                    .git_ignore(false)
                    .git_global(false)
                    .git_exclude(false)
                    .threads(config.walker_threads)
                    .max_depth(Some(10));

                // Use parallel walker
                let enabled = Arc::new(config.enabled_ecosystems.clone());
                builder.build_parallel().run(|| {
                    let scanners = &scanners;
                    let candidates_mutex = &candidates_mutex;
                    let paths_scanned = &paths_scanned;
                    let denied_seen = &denied_seen;
                    let tx = &tx;
                    let enabled = enabled.clone();

                    Box::new(move |entry| {
                        let entry = match entry {
                            Ok(e) => e,
                            Err(err) => {
                                report_walk_error(&err, denied_seen, tx);
                                return ignore::WalkState::Continue;
                            }
                        };

                        let count =
                            paths_scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if count.is_multiple_of(5000) && count > 0 {
                            let _ = tx.blocking_send(ScanEvent::Progress {
                                message: format!("Scanning... {count} paths examined"),
                                paths_scanned: count,
                            });
                        }

                        let Some(file_name) = entry.file_name().to_str() else {
                            return ignore::WalkState::Continue;
                        };

                        let path = entry.path();

                        // Skip common uninteresting directories
                        if matches!(file_name, ".git" | ".hg" | ".svn") {
                            return ignore::WalkState::Skip;
                        }

                        for scanner in scanners {
                            if enabled.contains(&scanner.ecosystem())
                                && scanner.is_candidate(file_name, path)
                            {
                                debug!("Found candidate: {}", path.display());
                                let mut cands = candidates_mutex
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                cands.push((scanner.clone(), path.to_path_buf()));
                                // Don't descend into candidate directories
                                return ignore::WalkState::Skip;
                            }
                        }

                        ignore::WalkState::Continue
                    })
                });
            }

            candidates
        })
        .await
        .unwrap_or_default()
    }
}

/// Maximum distinct unreadable directories reported per scan.
///
/// A Mac without Full Disk Access denies thousands of paths; the user needs to
/// know it happened and roughly where, not a transcript.
const MAX_DENIED_REPORTS: usize = 40;

/// Whether a path exists but cannot be read.
///
/// Distinguishes "empty, nothing to clean" from "invisible because Void was
/// refused access", which otherwise look identical: both yield a size of zero.
fn is_permission_denied(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Err(err) => err.kind() == std::io::ErrorKind::PermissionDenied && path.exists(),
        Ok(_) => false,
    }
}

/// Dig the offending path out of a walker error.
///
/// `ignore::Error` nests: the path lives in a `WithPath` wrapper that may sit
/// under `WithDepth` or `WithLineNumber`.
fn walk_error_path(err: &ignore::Error) -> Option<&Path> {
    match err {
        ignore::Error::WithPath { path, .. } => Some(path.as_path()),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            walk_error_path(err)
        }
        _ => None,
    }
}

/// Surface a walker error as a permission event, deduplicated and bounded.
fn report_walk_error(
    err: &ignore::Error,
    seen: &Mutex<HashSet<PathBuf>>,
    tx: &mpsc::Sender<ScanEvent>,
) {
    let Some(io_err) = err.io_error() else {
        return;
    };
    if io_err.kind() != std::io::ErrorKind::PermissionDenied {
        return;
    }
    let Some(path) = walk_error_path(err) else {
        return;
    };

    // `ignore` reports a directory-open failure against the unreadable
    // directory itself, so this is already the right path. Walking up to the
    // parent would name a directory that read just fine.
    let dir = path.to_path_buf();

    {
        let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.len() >= MAX_DENIED_REPORTS || !seen.insert(dir.clone()) {
            return;
        }
    }

    let _ = tx.blocking_send(ScanEvent::PermissionDenied { path: dir });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::rust::RustScanner;

    #[test]
    fn empty_readable_directory_is_not_permission_denied() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_permission_denied(tmp.path()));
    }

    #[test]
    fn missing_directory_is_not_permission_denied() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_permission_denied(&tmp.path().join("nope")));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_reported_rather_than_looking_empty() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("secret.bin"), vec![0u8; 1024]).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let denied = is_permission_denied(&locked);
        // Root reads anything, so the distinction cannot be tested there.
        let is_root = std::fs::read_dir(&locked).is_ok();

        // Restore so the tempdir can clean itself up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        if !is_root {
            assert!(
                denied,
                "an unreadable directory must be distinguishable from an empty one"
            );
        }
    }

    #[tokio::test]
    async fn orchestrator_finds_rust_target() {
        // Set up a fake project tree
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myproject");
        std::fs::create_dir_all(project.join("target/debug")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        std::fs::write(project.join("target/debug/bin"), "fake").unwrap();

        let config = AppConfig {
            scan_roots: vec![tmp.path().to_path_buf()],
            enabled_ecosystems: vec![Ecosystem::Rust],
            walker_threads: 1,
            max_concurrent_analyses: 4,
            ..Default::default()
        };

        let scanners: Vec<Arc<dyn EcosystemScanner>> = vec![Arc::new(RustScanner)];
        let orchestrator = ScanOrchestrator::new(scanners, config);
        let mut rx = orchestrator.start_scan();

        let mut found_items = vec![];
        let mut got_complete = false;

        while let Some(event) = rx.recv().await {
            match event {
                ScanEvent::ItemFound { item } => found_items.push(item),
                ScanEvent::ScanComplete { .. } => {
                    got_complete = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(got_complete, "Should receive ScanComplete event");
        assert!(!found_items.is_empty(), "Should find at least one item");
        assert_eq!(found_items[0].ecosystem, Ecosystem::Rust);
        assert_eq!(found_items[0].kind, ArtifactKind::TargetDir);
    }
}
