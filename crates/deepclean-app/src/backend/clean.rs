use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use deepclean_core::history::{CleanRun, RunItem, RunOutcome};
use deepclean_core::model::{ActionEvent, ActionMethod, CleanAction, CleanableItem, Ecosystem};
use uuid::Uuid;

use super::{Backend, UiEvent};

/// What a finished batch did, for callers that wait on it (Guard mode).
#[derive(Debug, Clone, Copy, Default)]
pub struct BatchOutcome {
    pub succeeded: usize,
    pub failed: usize,
    pub bytes_freed: u64,
}

impl Backend {
    /// Run the user's selections. `(item id, action id)` pairs are resolved
    /// against Void's own results, so nothing the UI sends can name an
    /// arbitrary path or action.
    pub fn execute_clean(&self, selections: &[(Uuid, Uuid)]) -> Result<(), String> {
        let pairs: Vec<(CleanableItem, CleanAction)> = {
            let results = self.state.results();
            selections
                .iter()
                .filter_map(|(item_id, action_id)| {
                    let item = results.iter().find(|i| i.id == *item_id)?;
                    let action = item.available_actions.iter().find(|a| a.id == *action_id)?;
                    Some((item.clone(), action.clone()))
                })
                .collect()
        };
        if pairs.is_empty() {
            return Err("No valid selections".into());
        }
        // A fresh batch clears any cancellation left over from the last one.
        self.state.reset_cancel();
        let cancel = self.state.cancel_flag();
        let this = self.clone();
        self.rt.spawn(async move {
            this.run_batch(pairs, cancel, None).await;
        });
        Ok(())
    }

    /// Stop the running batch before its next item. Never interrupts an
    /// action in flight — a half-finished `rm -rf` would leave the user worse
    /// off than letting it complete.
    pub fn cancel_clean(&self) {
        self.state.request_cancel();
    }

    /// Run a batch through the executor, record it in history, and keep the
    /// authoritative result list in step.
    ///
    /// `trigger` is `None` for a clean the user started. Anything else
    /// (`"guard"`) is unattended: its progress is not streamed as action
    /// events — the window would mistake them for the user's own batch — and
    /// it does not touch the user's cancel flag.
    pub async fn run_batch(
        &self,
        pairs: Vec<(CleanableItem, CleanAction)>,
        cancel: Arc<AtomicBool>,
        trigger: Option<String>,
    ) -> BatchOutcome {
        let interactive = trigger.is_none();
        let pending: HashMap<Uuid, PendingRecord> = pairs
            .iter()
            .map(|(item, action)| {
                (
                    item.id,
                    PendingRecord {
                        path: item.path.clone(),
                        ecosystem: item.ecosystem,
                        action_label: action.label.clone(),
                        method_summary: describe_method(&action.method),
                        estimated_bytes: action.estimated_savings_bytes,
                    },
                )
            })
            .collect();

        let executor = self.state.create_executor();
        let mut rx = executor.execute_batch_with_cancel(pairs, cancel);
        let started_at = chrono::Utc::now();
        let start = std::time::Instant::now();
        let mut recorded: Vec<RunItem> = Vec::new();
        let mut outcome = BatchOutcome::default();

        while let Some(event) = rx.recv().await {
            match &event {
                ActionEvent::Completed {
                    item_id,
                    bytes_freed,
                    estimated_bytes,
                    ..
                } => {
                    let path = pending.get(item_id).map(|p| p.path.clone());
                    // Drop it from the authoritative list too, or a second
                    // clean could re-select something already gone. An
                    // unattended run cleaned items from its own scan, so
                    // they are matched on path as well as id.
                    self.state.results().retain(|i| {
                        i.id != *item_id && (interactive || Some(&i.path) != path.as_ref())
                    });
                    if !interactive {
                        if let Some(path) = path {
                            self.emit(UiEvent::GuardCleaned(path));
                        }
                    }
                    if let Some(p) = pending.get(item_id) {
                        recorded.push(p.to_run_item(
                            *bytes_freed,
                            *estimated_bytes,
                            RunOutcome::Succeeded,
                        ));
                    }
                }
                ActionEvent::Failed { item_id, error, .. } => {
                    if let Some(p) = pending.get(item_id) {
                        recorded.push(p.to_run_item(
                            0,
                            0,
                            RunOutcome::Failed {
                                error: error.clone(),
                            },
                        ));
                    }
                }
                ActionEvent::BatchComplete {
                    succeeded,
                    failed,
                    bytes_freed,
                    ..
                } => {
                    outcome = BatchOutcome {
                        succeeded: *succeeded,
                        failed: *failed,
                        bytes_freed: *bytes_freed,
                    };
                    if !recorded.is_empty() {
                        self.state.history().push(CleanRun {
                            id: Uuid::new_v4(),
                            started_at,
                            duration_ms: start.elapsed().as_millis() as u64,
                            items: std::mem::take(&mut recorded),
                            trigger: trigger.clone(),
                        });
                        if let Err(err) = self.state.persist_history() {
                            self.emit(UiEvent::Warning(err));
                        }
                    }
                    if interactive {
                        self.state.reset_cancel();
                    }
                    self.emit(UiEvent::TrayRefresh);
                }
                _ => {}
            }
            if interactive {
                self.emit(UiEvent::Action(event));
            }
        }
        outcome
    }
}

/// Details of a queued item, held so a history record can be written when
/// its terminal event arrives.
struct PendingRecord {
    path: std::path::PathBuf,
    ecosystem: Ecosystem,
    action_label: String,
    method_summary: String,
    estimated_bytes: u64,
}

impl PendingRecord {
    fn to_run_item(&self, bytes_freed: u64, estimated: u64, result: RunOutcome) -> RunItem {
        RunItem {
            path: self.path.clone(),
            ecosystem: self.ecosystem,
            action_label: self.action_label.clone(),
            method_summary: self.method_summary.clone(),
            bytes_freed,
            // The event carries the estimate for completions; failures get the
            // action's own figure, which is what was at stake.
            estimated_bytes: if estimated > 0 {
                estimated
            } else {
                self.estimated_bytes
            },
            result,
        }
    }
}

/// Human-readable summary of what an action method does, for history.
///
/// For commands this is the literal command line — the only honest
/// disclosure available, since Void does not interpret what a command removes.
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
        ActionMethod::RemoveDir { .. } => "Delete directory recursively".into(),
        ActionMethod::RemoveFile { .. } => "Delete file".into(),
        ActionMethod::DockerPrune { prune_type } => format!("docker {prune_type} prune -f"),
        ActionMethod::MoveToTrash { .. } => "Move to Trash".into(),
        ActionMethod::RemoveDirs { paths } => format!("Delete {} directories", paths.len()),
        ActionMethod::RemoveFiles { paths } => format!("Delete {} files", paths.len()),
        ActionMethod::GitWorktreeRemove {
            worktree,
            delete_branch,
            ..
        } => match delete_branch {
            Some(branch) => format!(
                "git worktree remove {} && git branch -d {branch}",
                worktree.display()
            ),
            None => format!("git worktree remove {}", worktree.display()),
        },
        ActionMethod::GitWorktreePrune { .. } => "git worktree prune".into(),
        ActionMethod::RemoveOllamaOrphans { blobs, .. } => format!(
            "Delete {} unreferenced Ollama blobs (manifests re-checked first)",
            blobs.len()
        ),
        ActionMethod::GitDeleteBranches { branches, .. } => {
            format!("git branch -d {}", branches.join(" "))
        }
        ActionMethod::DedupFiles { groups } => format!(
            "Replace {} duplicate files with copy-on-write clones",
            groups.iter().map(|g| g.duplicates.len()).sum::<usize>()
        ),
    }
}

/// Where Reveal should point for a result: the item itself, or — once it
/// has been cleaned — the folder it lived in.
///
/// Takes an item id rather than a path: the path comes from Void's own
/// results, so nothing the UI sends can aim the file manager elsewhere.
pub fn reveal_target(
    state: &crate::state::AppState,
    item_id: Uuid,
) -> Result<std::path::PathBuf, String> {
    let path = state
        .results()
        .iter()
        .find(|i| i.id == item_id)
        .map(|i| i.path.clone())
        .ok_or_else(|| "That item is no longer in the scan results.".to_string())?;
    if path.exists() {
        return Ok(path);
    }
    path.parent()
        .filter(|p| p.exists())
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} no longer exists.", path.display()))
}
