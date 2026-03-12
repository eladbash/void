use std::path::Path;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::error::ActionError;
use crate::model::{ActionEvent, ActionMethod, CleanAction, CleanableItem};
use crate::safety::SafetyChecker;
use crate::staleness;

/// Executes clean actions with safety checks.
pub struct ActionExecutor {
    safety: SafetyChecker,
}

impl ActionExecutor {
    pub fn new(safety: SafetyChecker) -> Self {
        Self { safety }
    }

    /// Execute a batch of (item, action) pairs, returning a channel of events.
    ///
    /// Spawns a background tokio task that processes each pair sequentially,
    /// checking safety before execution and sending progress events through the channel.
    pub fn execute_batch(
        self,
        items: Vec<(CleanableItem, CleanAction)>,
    ) -> mpsc::Receiver<ActionEvent> {
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            for (item, action) in items {
                let item_id = item.id;
                let action_id = action.id;

                // Send Started event
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
                    let _ = tx
                        .send(ActionEvent::Failed {
                            item_id,
                            action_id,
                            error: err.to_string(),
                        })
                        .await;
                    continue;
                }

                // Execute the action
                match self.execute_action(&action.method).await {
                    Ok(bytes_freed) => {
                        info!(
                            item_id = %item_id,
                            action_id = %action_id,
                            bytes_freed,
                            "Action completed successfully"
                        );
                        let _ = tx
                            .send(ActionEvent::Completed {
                                item_id,
                                action_id,
                                bytes_freed,
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
            ActionMethod::Command { working_dir, .. } => {
                if let Some(dir) = working_dir {
                    if !self.safety.is_path_allowed(dir) {
                        return Err(ActionError::PathBlocked(dir.display().to_string()));
                    }
                }
            }
            ActionMethod::DockerPrune { .. } => {
                // Docker prune doesn't operate on filesystem paths directly
            }
        }
        Ok(())
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
                let output = cmd.output().await?;
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
                    "Expected safety block error, got: {}",
                    error
                );
            }
            other => panic!("Expected Failed event, got: {:?}", other),
        }

        // Channel should be closed
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
            other => panic!("Expected Completed event, got: {:?}", other),
        }

        // Directory should be gone
        assert!(!target.exists());

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
            other => panic!("Expected Completed event, got: {:?}", other),
        }

        // File should be gone
        assert!(!file.exists());

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
}
