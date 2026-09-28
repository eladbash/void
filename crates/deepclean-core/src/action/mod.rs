use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::error::ActionError;
use crate::model::{ActionEvent, ActionMethod, CleanAction, CleanableItem};
use crate::safety::SafetyChecker;
use crate::staleness;
use crate::trash::TrashBackend;

/// Executes clean actions with safety checks.
pub struct ActionExecutor {
    safety: SafetyChecker,
    trash: TrashBackend,
}

impl ActionExecutor {
    pub fn new(safety: SafetyChecker) -> Self {
        Self::with_trash(safety, TrashBackend::System)
    }

    /// An executor whose "Move to Trash" goes to `trash` — tests and the
    /// sandbox pass a directory so nothing reaches the real Trash.
    pub fn with_trash(safety: SafetyChecker, trash: TrashBackend) -> Self {
        Self { safety, trash }
    }

    /// Execute a batch of (item, action) pairs, returning a channel of events.
    ///
    /// Processes pairs sequentially, checking safety before each. A failure
    /// does not stop the batch — the remaining items still run, and the
    /// terminal [`ActionEvent::BatchComplete`] reports the tally.
    pub fn execute_batch(
        self,
        items: Vec<(CleanableItem, CleanAction)>,
    ) -> mpsc::Receiver<ActionEvent> {
        self.execute_batch_with_cancel(items, Arc::new(AtomicBool::new(false)))
    }

    /// As [`Self::execute_batch`], but stops before the next item when
    /// `cancel` is set.
    ///
    /// Cancellation is checked between items, never mid-action: interrupting a
    /// half-finished `rm -rf` would leave the user worse off than letting it
    /// finish.
    pub fn execute_batch_with_cancel(
        self,
        items: Vec<(CleanableItem, CleanAction)>,
        cancel: Arc<AtomicBool>,
    ) -> mpsc::Receiver<ActionEvent> {
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let total = items.len();
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            let mut bytes_freed = 0u64;
            let mut estimated = false;
            let mut cancelled = false;

            for (item, action) in items {
                if cancel.load(Ordering::Relaxed) {
                    cancelled = true;
                    info!("Batch cancelled before item {}", item.path.display());
                    break;
                }

                let item_id = item.id;
                let action_id = action.id;
                let estimate = action.estimated_savings_bytes;

                let _ = tx
                    .send(ActionEvent::Started {
                        item_id,
                        action_id,
                        label: action.label.clone(),
                    })
                    .await;

                // Safety gate: check paths before executing
                if let Err(err) = self.check_safety(&action.method) {
                    warn!(
                        item_id = %item_id,
                        action_id = %action_id,
                        "Action blocked by safety checker: {}",
                        err
                    );
                    failed += 1;
                    let _ = tx
                        .send(ActionEvent::Failed {
                            item_id,
                            action_id,
                            error: err.to_string(),
                        })
                        .await;
                    continue;
                }

                match self.execute_action(&action.method).await {
                    Ok(measured) => {
                        info!(
                            item_id = %item_id,
                            action_id = %action_id,
                            bytes_freed = measured,
                            "Action completed successfully"
                        );
                        succeeded += 1;
                        // Commands and Docker prunes cannot observe what they
                        // removed, so their estimate is the only figure there is.
                        if measured > 0 {
                            bytes_freed += measured;
                        } else {
                            bytes_freed += estimate;
                            if estimate > 0 {
                                estimated = true;
                            }
                        }
                        let _ = tx
                            .send(ActionEvent::Completed {
                                item_id,
                                action_id,
                                bytes_freed: measured,
                                estimated_bytes: estimate,
                            })
                            .await;
                    }
                    Err(err) => {
                        error!(
                            item_id = %item_id,
                            action_id = %action_id,
                            "Action failed: {}",
                            err
                        );
                        failed += 1;
                        let _ = tx
                            .send(ActionEvent::Failed {
                                item_id,
                                action_id,
                                error: err.to_string(),
                            })
                            .await;
                    }
                }
            }

            let _ = tx
                .send(ActionEvent::BatchComplete {
                    total,
                    succeeded,
                    failed,
                    bytes_freed,
                    estimated,
                    cancelled,
                })
                .await;
        });

        rx
    }

    /// Check safety for an action method. Returns an error if the path is blocked.
    fn check_safety(&self, method: &ActionMethod) -> Result<(), ActionError> {
        match method {
            ActionMethod::RemoveDir { path } => {
                if !self.safety.is_path_allowed(path) {
                    return Err(ActionError::PathBlocked(path.display().to_string()));
                }
            }
            ActionMethod::RemoveFile { path } => {
                if !self.safety.is_path_allowed(path) {
                    return Err(ActionError::PathBlocked(path.display().to_string()));
                }
            }
            ActionMethod::Command {
                program,
                args,
                working_dir,
            } => {
                if let Some(dir) = working_dir {
                    if !self.safety.is_workdir_allowed(dir) {
                        return Err(ActionError::PathBlocked(dir.display().to_string()));
                    }
                }
                // The command's arguments are where the deleting actually
                // happens; checking only the working directory left `rm -rf`
                // and the `osascript` trash one-liner ungated.
                self.safety
                    .is_command_allowed(program, args, working_dir.as_deref())
                    .map_err(ActionError::PathBlocked)?;
            }
            ActionMethod::DockerPrune { .. } => {
                // Docker prune doesn't operate on filesystem paths directly
            }
            ActionMethod::MoveToTrash { path } => {
                if !self.safety.is_trash_allowed(path) {
                    return Err(ActionError::PathBlocked(path.display().to_string()));
                }
            }
            ActionMethod::RemoveDirs { paths } | ActionMethod::RemoveFiles { paths } => {
                if paths.is_empty() {
                    return Err(ActionError::CommandFailed("nothing to remove".into()));
                }
                for path in paths {
                    self.require_allowed(path)?;
                }
            }
            ActionMethod::GitWorktreeRemove { repo, worktree, .. } => {
                if !self.safety.is_workdir_allowed(repo) {
                    return Err(ActionError::PathBlocked(repo.display().to_string()));
                }
                // Full check, sentinels included: a worktree holding a `.env`
                // or credentials is not removed wholesale — trim its build
                // artifacts instead.
                self.require_allowed(worktree)?;
            }
            ActionMethod::GitWorktreePrune { repo }
            | ActionMethod::GitDeleteBranches { repo, .. } => {
                if !self.safety.is_workdir_allowed(repo) {
                    return Err(ActionError::PathBlocked(repo.display().to_string()));
                }
            }
            ActionMethod::RemoveOllamaOrphans { store, blobs } => {
                if !self.safety.is_workdir_allowed(store) {
                    return Err(ActionError::PathBlocked(store.display().to_string()));
                }
                for blob in blobs {
                    self.require_allowed(blob)?;
                }
            }
            ActionMethod::DedupFiles { groups } => {
                for group in groups {
                    self.require_allowed(&group.keep)?;
                    for dup in &group.duplicates {
                        self.require_allowed(dup)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn require_allowed(&self, path: &Path) -> Result<(), ActionError> {
        if self.safety.is_path_allowed(path) {
            Ok(())
        } else {
            Err(ActionError::PathBlocked(path.display().to_string()))
        }
    }

    /// Execute a single action method.
    async fn execute_action(&self, method: &ActionMethod) -> Result<u64, ActionError> {
        match method {
            ActionMethod::Command {
                program,
                args,
                working_dir,
            } => {
                info!(program, ?args, ?working_dir, "Running command");
                let mut cmd = tokio::process::Command::new(program);
                cmd.args(args);
                if let Some(dir) = working_dir {
                    cmd.current_dir(dir);
                }
                let output = cmd.output().await.map_err(|err| {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        ActionError::CommandFailed(format!("`{program}` was not found in PATH"))
                    } else {
                        ActionError::Io(err)
                    }
                })?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(ActionError::CommandFailed(format!(
                        "{} exited with {}: {}",
                        program,
                        output.status,
                        stderr.trim()
                    )));
                }
                Ok(0)
            }
            ActionMethod::RemoveDir { path } => {
                info!(path = %path.display(), "Removing directory");
                let size = staleness::compute_dir_size(path).await;
                tokio::fs::remove_dir_all(path).await?;
                Ok(size)
            }
            ActionMethod::RemoveFile { path } => {
                info!(path = %path.display(), "Removing file");
                let size = file_size(path).await?;
                tokio::fs::remove_file(path).await?;
                Ok(size)
            }
            ActionMethod::DockerPrune { prune_type } => {
                info!(prune_type, "Running docker prune");
                let output = tokio::process::Command::new("docker")
                    .args([prune_type.as_str(), "prune", "-f"])
                    .output()
                    .await?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(ActionError::CommandFailed(format!(
                        "docker {} prune -f failed: {}",
                        prune_type,
                        stderr.trim()
                    )));
                }
                Ok(0)
            }
            ActionMethod::MoveToTrash { path } => {
                info!(path = %path.display(), "Moving to Trash");
                let size = if path.is_dir() {
                    staleness::compute_dir_size(path).await
                } else {
                    file_size(path).await.unwrap_or(0)
                };
                let trash = self.trash.clone();
                let target = path.clone();
                tokio::task::spawn_blocking(move || trash.trash(&target))
                    .await
                    .map_err(|err| ActionError::CommandFailed(err.to_string()))?
                    .map_err(ActionError::CommandFailed)?;
                Ok(size)
            }
            ActionMethod::RemoveDirs { paths } => {
                let mut freed = 0;
                for path in paths {
                    // A listed directory already gone (the user ran
                    // `npm ci` since the scan) is not a failure.
                    if !path.exists() {
                        continue;
                    }
                    info!(path = %path.display(), "Removing directory");
                    freed += staleness::compute_dir_size(path).await;
                    tokio::fs::remove_dir_all(path).await?;
                }
                Ok(freed)
            }
            ActionMethod::RemoveFiles { paths } => {
                let mut freed = 0;
                for path in paths {
                    if !path.exists() {
                        continue;
                    }
                    info!(path = %path.display(), "Removing file");
                    freed += file_size(path).await?;
                    tokio::fs::remove_file(path).await?;
                }
                Ok(freed)
            }
            ActionMethod::GitWorktreeRemove {
                repo,
                worktree,
                delete_branch,
            } => {
                info!(worktree = %worktree.display(), "Removing git worktree");
                let size = staleness::compute_dir_size(worktree).await;
                crate::git::remove_worktree_checked(repo, worktree, delete_branch.as_deref())
                    .await
                    .map_err(ActionError::CommandFailed)?;
                Ok(size)
            }
            ActionMethod::GitWorktreePrune { repo } => {
                crate::git::prune_worktrees(repo)
                    .await
                    .map_err(ActionError::CommandFailed)?;
                Ok(0)
            }
            ActionMethod::GitDeleteBranches { repo, branches } => {
                crate::git::delete_merged_branches(repo, branches)
                    .await
                    .map_err(ActionError::CommandFailed)?;
                Ok(0)
            }
            ActionMethod::RemoveOllamaOrphans { store, blobs } => {
                let (store, blobs) = (store.clone(), blobs.clone());
                tokio::task::spawn_blocking(move || {
                    crate::scanner::models::remove_orphans_checked(&store, &blobs)
                })
                .await
                .map_err(|err| ActionError::CommandFailed(err.to_string()))?
                .map_err(ActionError::CommandFailed)
            }
            ActionMethod::DedupFiles { groups } => crate::dedup::apply_groups(groups)
                .await
                .map_err(ActionError::CommandFailed),
        }
    }
}

/// Get the size of a file in bytes.
async fn file_size(path: &Path) -> Result<u64, ActionError> {
    let meta = tokio::fs::metadata(path).await?;
    Ok(meta.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use uuid::Uuid;

    use crate::model::{ArtifactKind, Ecosystem, RiskLevel};

    /// Helper to build a minimal CleanableItem for testing.
    fn test_item(path: PathBuf) -> CleanableItem {
        CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.clone(),
            ecosystem: Ecosystem::Rust,
            kind: ArtifactKind::TargetDir,
            risk: RiskLevel::Safe,
            size_bytes: 0,
            size_display: "0 B".into(),
            last_modified: None,
            days_stale: None,
            project_name: None,
            project_root: None,
            available_actions: vec![],
        }
    }

    /// Drain the terminal batch event and assert its tally.
    async fn expect_batch_complete(
        rx: &mut mpsc::Receiver<ActionEvent>,
        succeeded: usize,
        failed: usize,
        bytes: u64,
    ) {
        match rx.recv().await.expect("expected a BatchComplete event") {
            ActionEvent::BatchComplete {
                succeeded: s,
                failed: f,
                bytes_freed,
                ..
            } => {
                assert_eq!(s, succeeded, "succeeded count");
                assert_eq!(f, failed, "failed count");
                assert_eq!(bytes_freed, bytes, "bytes freed");
            }
            other => panic!("Expected BatchComplete, got: {other:?}"),
        }
    }

    /// Helper to build a CleanAction with a given method.
    fn test_action(method: ActionMethod) -> CleanAction {
        CleanAction {
            id: Uuid::new_v4(),
            label: "test action".into(),
            description: "test".into(),
            method,
            risk: RiskLevel::Safe,
            estimated_savings_bytes: 0,
        }
    }

    #[tokio::test]
    async fn refuses_to_delete_blocked_path() {
        let home = dirs::home_dir().unwrap();
        let blocked_path = home.join(".ssh");

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(blocked_path.clone());
        let action = test_action(ActionMethod::RemoveDir { path: blocked_path });

        let mut rx = executor.execute_batch(vec![(item, action)]);

        // First event should be Started
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, ActionEvent::Started { .. }));

        // Second event should be Failed due to safety block
        let event = rx.recv().await.unwrap();
        match event {
            ActionEvent::Failed { error, .. } => {
                assert!(
                    error.contains("blocked") || error.contains("Path blocked"),
                    "Expected safety block error, got: {error}"
                );
            }
            other => panic!("Expected Failed event, got: {other:?}"),
        }

        // ...then the terminal batch event, then close.
        let event = rx.recv().await.unwrap();
        assert!(matches!(
            event,
            ActionEvent::BatchComplete {
                total: 1,
                succeeded: 0,
                failed: 1,
                ..
            }
        ));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn remove_dir_works_and_reports_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("a.bin"), vec![0u8; 1024]).unwrap();
        std::fs::write(target.join("b.bin"), vec![0u8; 2048]).unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(target.clone());
        let action = test_action(ActionMethod::RemoveDir {
            path: target.clone(),
        });

        let mut rx = executor.execute_batch(vec![(item, action)]);

        // Started
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, ActionEvent::Started { .. }));

        // Completed with bytes freed
        let event = rx.recv().await.unwrap();
        match event {
            ActionEvent::Completed { bytes_freed, .. } => {
                assert_eq!(bytes_freed, 1024 + 2048);
            }
            other => panic!("Expected Completed event, got: {other:?}"),
        }

        // Directory should be gone
        assert!(!target.exists());

        expect_batch_complete(&mut rx, 1, 0, 1024 + 2048).await;
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn remove_file_works_and_reports_size() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("big_file.dat");
        std::fs::write(&file, vec![0u8; 4096]).unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(file.clone());
        let action = test_action(ActionMethod::RemoveFile { path: file.clone() });

        let mut rx = executor.execute_batch(vec![(item, action)]);

        // Started
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, ActionEvent::Started { .. }));

        // Completed
        let event = rx.recv().await.unwrap();
        match event {
            ActionEvent::Completed { bytes_freed, .. } => {
                assert_eq!(bytes_freed, 4096);
            }
            other => panic!("Expected Completed event, got: {other:?}"),
        }

        // File should be gone
        assert!(!file.exists());

        expect_batch_complete(&mut rx, 1, 0, 4096).await;
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn refuses_command_with_blocked_working_dir() {
        let home = dirs::home_dir().unwrap();
        let blocked_dir = home.join(".ssh");

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(blocked_dir.clone());
        let action = test_action(ActionMethod::Command {
            program: "echo".into(),
            args: vec!["hello".into()],
            working_dir: Some(blocked_dir),
        });

        let mut rx = executor.execute_batch(vec![(item, action)]);

        // Started
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, ActionEvent::Started { .. }));

        // Should fail due to safety
        let event = rx.recv().await.unwrap();
        assert!(
            matches!(event, ActionEvent::Failed { .. }),
            "Expected Failed event for blocked working_dir"
        );
    }

    #[tokio::test]
    async fn allows_command_in_project_root_holding_secrets() {
        // `cargo clean` runs in the project root; a `.env` sitting there means
        // "don't delete this directory", not "don't run in it".
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("trading");
        std::fs::create_dir_all(project.join("target")).unwrap();
        std::fs::write(project.join(".env"), "SECRET=x").unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(project.join("target"));
        let action = test_action(ActionMethod::Command {
            program: "true".into(),
            args: vec![],
            working_dir: Some(project),
        });

        let mut rx = executor.execute_batch(vec![(item, action)]);

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, ActionEvent::Started { .. }));

        let event = rx.recv().await.unwrap();
        assert!(
            matches!(event, ActionEvent::Completed { .. }),
            "Expected Completed event, got: {event:?}"
        );
    }

    #[tokio::test]
    async fn reports_missing_program_by_name() {
        // A GUI-launched app gets a bare PATH, so `cargo` and friends may not
        // resolve. The user needs to read which program is missing, not
        // "No such file or directory (os error 2)".
        let tmp = tempfile::tempdir().unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(tmp.path().to_path_buf());
        let action = test_action(ActionMethod::Command {
            program: "void-no-such-program".into(),
            args: vec!["clean".into()],
            working_dir: Some(tmp.path().to_path_buf()),
        });

        let mut rx = executor.execute_batch(vec![(item, action)]);

        let event = rx.recv().await.unwrap();
        assert!(matches!(event, ActionEvent::Started { .. }));

        let event = rx.recv().await.unwrap();
        match event {
            ActionEvent::Failed { error, .. } => {
                assert!(
                    error.contains("void-no-such-program") && error.contains("PATH"),
                    "Expected a named not-in-PATH error, got: {error}"
                );
            }
            other => panic!("Expected Failed event, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn batch_processes_multiple_items() {
        let tmp = tempfile::tempdir().unwrap();

        let file1 = tmp.path().join("file1.dat");
        std::fs::write(&file1, vec![0u8; 100]).unwrap();

        let file2 = tmp.path().join("file2.dat");
        std::fs::write(&file2, vec![0u8; 200]).unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));

        let pairs = vec![
            (
                test_item(file1.clone()),
                test_action(ActionMethod::RemoveFile {
                    path: file1.clone(),
                }),
            ),
            (
                test_item(file2.clone()),
                test_action(ActionMethod::RemoveFile {
                    path: file2.clone(),
                }),
            ),
        ];

        let mut rx = executor.execute_batch(pairs);

        let mut completed_count = 0;
        let mut total_bytes = 0u64;

        while let Some(event) = rx.recv().await {
            if let ActionEvent::Completed { bytes_freed, .. } = event {
                completed_count += 1;
                total_bytes += bytes_freed;
            }
        }

        assert_eq!(completed_count, 2);
        assert_eq!(total_bytes, 300);
        assert!(!file1.exists());
        assert!(!file2.exists());
    }

    #[tokio::test]
    async fn command_success_reports_estimate_since_nothing_is_measurable() {
        // `cargo clean` frees real space but returns zero bytes. The estimate
        // is the only figure available, and the batch must say so.
        let tmp = tempfile::tempdir().unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(tmp.path().to_path_buf());
        let action = CleanAction {
            estimated_savings_bytes: 3_800_000_000,
            ..test_action(ActionMethod::Command {
                program: "true".into(),
                args: vec![],
                working_dir: Some(tmp.path().to_path_buf()),
            })
        };

        let mut rx = executor.execute_batch(vec![(item, action)]);

        assert!(matches!(
            rx.recv().await.unwrap(),
            ActionEvent::Started { .. }
        ));
        match rx.recv().await.unwrap() {
            ActionEvent::Completed {
                bytes_freed,
                estimated_bytes,
                ..
            } => {
                assert_eq!(bytes_freed, 0, "a command cannot measure what it removed");
                assert_eq!(estimated_bytes, 3_800_000_000);
            }
            other => panic!("Expected Completed, got: {other:?}"),
        }
        match rx.recv().await.unwrap() {
            ActionEvent::BatchComplete {
                bytes_freed,
                estimated,
                ..
            } => {
                assert_eq!(bytes_freed, 3_800_000_000);
                assert!(estimated, "total must be flagged as an estimate");
            }
            other => panic!("Expected BatchComplete, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn measured_bytes_are_not_flagged_as_estimated() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f.dat");
        std::fs::write(&file, vec![0u8; 512]).unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(file.clone());
        let action = CleanAction {
            estimated_savings_bytes: 999_999,
            ..test_action(ActionMethod::RemoveFile { path: file })
        };

        let mut rx = executor.execute_batch(vec![(item, action)]);
        let _ = rx.recv().await;
        let _ = rx.recv().await;
        match rx.recv().await.unwrap() {
            ActionEvent::BatchComplete {
                bytes_freed,
                estimated,
                ..
            } => {
                assert_eq!(bytes_freed, 512, "measurement beats estimate");
                assert!(!estimated);
            }
            other => panic!("Expected BatchComplete, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancelling_stops_before_the_next_item() {
        let tmp = tempfile::tempdir().unwrap();
        let mut pairs = vec![];
        for i in 0..5 {
            let f = tmp.path().join(format!("f{i}.dat"));
            std::fs::write(&f, vec![0u8; 10]).unwrap();
            pairs.push((
                test_item(f.clone()),
                test_action(ActionMethod::RemoveFile { path: f }),
            ));
        }

        // Already cancelled: nothing should run at all.
        let cancel = Arc::new(AtomicBool::new(true));
        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let mut rx = executor.execute_batch_with_cancel(pairs, cancel);

        match rx.recv().await.unwrap() {
            ActionEvent::BatchComplete {
                total,
                succeeded,
                failed,
                cancelled,
                ..
            } => {
                assert_eq!(total, 5);
                assert_eq!(succeeded, 0);
                assert_eq!(failed, 0);
                assert!(cancelled, "batch must report that it was cancelled");
            }
            other => panic!("Expected BatchComplete, got: {other:?}"),
        }
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn failure_does_not_stop_the_rest_of_the_batch() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.dat");
        std::fs::write(&good, vec![0u8; 64]).unwrap();
        let missing = tmp.path().join("does-not-exist.dat");

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let pairs = vec![
            (
                test_item(missing.clone()),
                test_action(ActionMethod::RemoveFile { path: missing }),
            ),
            (
                test_item(good.clone()),
                test_action(ActionMethod::RemoveFile { path: good.clone() }),
            ),
        ];

        let mut rx = executor.execute_batch(pairs);
        let mut last = None;
        while let Some(event) = rx.recv().await {
            last = Some(event);
        }

        match last.unwrap() {
            ActionEvent::BatchComplete {
                total,
                succeeded,
                failed,
                ..
            } => {
                assert_eq!(total, 2);
                assert_eq!(succeeded, 1, "the good item must still run");
                assert_eq!(failed, 1);
            }
            other => panic!("Expected BatchComplete last, got: {other:?}"),
        }
        assert!(!good.exists(), "the second item should have been removed");
    }

    #[tokio::test]
    async fn blocks_a_command_targeting_a_protected_path() {
        // `rm -rf ~/.ssh` previously bypassed the safety checker entirely,
        // because only the working directory was ever inspected.
        let home = dirs::home_dir().unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let executor = ActionExecutor::new(SafetyChecker::new(vec![]));
        let item = test_item(tmp.path().to_path_buf());
        let action = test_action(ActionMethod::Command {
            program: "rm".into(),
            args: vec![
                "-rf".into(),
                home.join(".ssh").to_string_lossy().to_string(),
            ],
            working_dir: Some(tmp.path().to_path_buf()),
        });

        let mut rx = executor.execute_batch(vec![(item, action)]);
        assert!(matches!(
            rx.recv().await.unwrap(),
            ActionEvent::Started { .. }
        ));
        match rx.recv().await.unwrap() {
            ActionEvent::Failed { error, .. } => {
                assert!(
                    error.contains("protected path"),
                    "Expected a protected-path block, got: {error}"
                );
            }
            other => panic!("Expected Failed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn allows_a_command_targeting_a_file_inside_downloads() {
        // Cleaning an entry inside ~/Downloads is the System scanner's whole
        // purpose; only the directory itself is off limits.
        let home = dirs::home_dir().unwrap();
        let checker = SafetyChecker::new(vec![]);

        assert!(
            checker
                .is_command_allowed(
                    "rm",
                    &[
                        "-rf".into(),
                        home.join("Downloads/xcode_14.3.xip")
                            .to_string_lossy()
                            .to_string(),
                    ],
                    None
                )
                .is_ok(),
            "a file inside Downloads must be cleanable"
        );

        assert!(
            checker
                .is_command_allowed(
                    "rm",
                    &[
                        "-rf".into(),
                        home.join("Downloads").to_string_lossy().to_string(),
                    ],
                    None
                )
                .is_err(),
            "the Downloads directory itself must never be removed"
        );
    }

    #[tokio::test]
    async fn blocks_a_path_embedded_in_a_quoted_script_argument() {
        // The trash action passes its path inside an osascript string.
        let home = dirs::home_dir().unwrap();
        let checker = SafetyChecker::new(vec![]);

        let arg = format!(
            "tell application \"Finder\" to delete POSIX file \"{}\"",
            home.join(".gnupg").display()
        );
        assert!(
            checker
                .is_command_allowed("osascript", &["-e".into(), arg], None)
                .is_err(),
            "a protected path inside a script string must still be caught"
        );
    }

    #[tokio::test]
    async fn documents_desktop_and_pictures_are_blocked_all_the_way_down() {
        // Only Downloads has a scanner that cleans its contents. The other
        // protected containers must be refused for the whole subtree, not just
        // at the folder itself.
        let home = dirs::home_dir().unwrap();
        let checker = SafetyChecker::new(vec![]);

        for folder in ["Documents", "Desktop", "Pictures"] {
            let inside = home.join(folder).join("something.zip");
            assert!(
                checker
                    .is_command_allowed(
                        "rm",
                        &["-rf".into(), inside.to_string_lossy().to_string()],
                        None
                    )
                    .is_err(),
                "a command reaching into ~/{folder} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn a_relative_argument_is_resolved_against_the_working_directory() {
        // `rm -rf Documents` run from home is the same act as the absolute
        // form, and must be refused the same way.
        let home = dirs::home_dir().unwrap();
        let checker = SafetyChecker::new(vec![]);

        if home.join("Documents").exists() {
            assert!(
                checker
                    .is_command_allowed("rm", &["-rf".into(), "Documents".into()], Some(&home))
                    .is_err(),
                "a relative path into a protected folder must be refused"
            );
        }

        // A subcommand that happens to look like a word is not a path.
        assert!(
            checker
                .is_command_allowed("cargo", &["clean".into()], Some(&home))
                .is_ok(),
            "ordinary subcommands must not be blocked by the relative-path check"
        );
    }

    #[tokio::test]
    async fn ordinary_command_arguments_are_not_mistaken_for_paths() {
        let checker = SafetyChecker::new(vec![]);
        for (program, args) in [
            ("cargo", vec!["clean".to_string()]),
            ("go", vec!["clean".into(), "-modcache".into()]),
            (
                "npm",
                vec!["cache".into(), "clean".into(), "--force".into()],
            ),
            ("brew", vec!["cleanup".into()]),
            ("docker", vec!["system".into(), "prune".into(), "-f".into()]),
            (
                "xcrun",
                vec!["simctl".into(), "delete".into(), "unavailable".into()],
            ),
        ] {
            assert!(
                checker.is_command_allowed(program, &args, None).is_ok(),
                "{program} {args:?} should not be blocked"
            );
        }
    }
}
