pub mod apple;
pub mod docker;
pub mod dotnet;
pub mod go;
pub mod homebrew;
pub mod java;
pub mod jetbrains;
pub mod node;
pub mod python;
pub mod rust;
pub mod system;

use std::path::{Path, PathBuf};
use std::sync::Arc;

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

    /// Global (non-project) locations to check.
    fn global_locations(&self) -> Vec<PathBuf> {
        vec![]
    }
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
            let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_analyses));

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

            // Phase 2: Analyze candidates concurrently
            let mut analyze_handles = vec![];

            for (scanner, path) in walk_candidates {
                let sem = semaphore.clone();
                let tx = tx.clone();
                let handle = tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    match scanner.analyze(&path).await {
                        Ok(Some(item)) => {
                            let _ = tx.send(ScanEvent::ItemFound { item }).await;
                        }
                        Ok(None) => {}
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
            for scanner in &self.scanners {
                for loc in scanner.global_locations() {
                    let scanner = scanner.clone();
                    let sem = semaphore.clone();
                    let tx = tx.clone();
                    let handle = tokio::spawn(async move {
                        let _permit = sem.acquire().await.unwrap();
                        match scanner.analyze(&loc).await {
                            Ok(Some(item)) => {
                                let _ = tx.send(ScanEvent::ItemFound { item }).await;
                            }
                            Ok(None) => {}
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
                    let tx = &tx;
                    let enabled = enabled.clone();

                    Box::new(move |entry| {
                        let entry = match entry {
                            Ok(e) => e,
                            Err(_) => return ignore::WalkState::Continue,
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
                                if let Ok(mut cands) = candidates_mutex.lock() {
                                    cands.push((scanner.clone(), path.to_path_buf()));
                                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::rust::RustScanner;

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
