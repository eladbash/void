use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use deepclean_core::disk::DiskUsage;
use deepclean_core::history::{CleanRun, History, RunItem, RunOutcome};
use deepclean_core::model::*;
use deepclean_core::scanner::{registry, ScanOrchestrator};

use crate::state::AppState;

#[tauri::command]
pub async fn start_scan(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    if !state.begin_scan() {
        return Err("Scan already in progress".into());
    }

    state.results().clear();

    let config = state.config().clone();

    let scanners = registry::build(&config);

    let orchestrator = ScanOrchestrator::new(scanners, config);
    let mut rx = orchestrator.start_scan();

    // Stream events to frontend
    let app_clone = app.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            // A sandboxed app (VOID_HOME) must never show — or offer to run —
            // anything that reaches past the fake home.
            let event = match (event, deepclean_core::paths::home_override()) {
                (ScanEvent::ItemFound { item }, Some(home)) => {
                    match deepclean_core::sandbox::confine(vec![item], &home).pop() {
                        Some(item) => ScanEvent::ItemFound { item },
                        None => continue,
                    }
                }
                (event, _) => event,
            };
            match &event {
                ScanEvent::ItemFound { item } => {
                    // Store result
                    if let Some(handle) = app_clone.try_state::<AppState>() {
                        handle.results().push(item.clone());
                    }
                }
                ScanEvent::ScanComplete { summary } => {
                    if let Some(handle) = app_clone.try_state::<AppState>() {
                        *handle.summary() = summary.clone();
                    }
                    crate::tray::refresh(&app_clone);
                }
                _ => {}
            }
            let _ = app_clone.emit("scan-event", &event);
        }

        // Released here rather than in the ScanComplete arm: if the orchestrator
        // task dies or the channel closes without a final event, that arm never
        // runs and the latch would stay set for the life of the process,
        // refusing every later scan.
        if let Some(handle) = app_clone.try_state::<AppState>() {
            handle.finish_scan();
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn get_scan_results(
    state: State<'_, AppState>,
    ecosystem: Option<Ecosystem>,
    min_risk: Option<RiskLevel>,
) -> Result<Vec<CleanableItem>, String> {
    let results = state.results();
    let filtered: Vec<CleanableItem> = results
        .iter()
        .filter(|item| {
            if let Some(eco) = &ecosystem {
                if &item.ecosystem != eco {
                    return false;
                }
            }
            if let Some(risk) = &min_risk {
                if &item.risk < risk {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    Ok(filtered)
}

#[derive(Deserialize)]
pub struct CleanRequest {
    pub selections: Vec<CleanSelection>,
}

#[derive(Deserialize)]
pub struct CleanSelection {
    pub item_id: uuid::Uuid,
    pub action_id: uuid::Uuid,
}

#[tauri::command]
pub async fn execute_clean(
    app: AppHandle,
    state: State<'_, AppState>,
    request: CleanRequest,
) -> Result<(), String> {
    let pairs: Vec<(CleanableItem, CleanAction)> = {
        let results = state.results();
        request
            .selections
            .iter()
            .filter_map(|sel| {
                let item = results.iter().find(|i| i.id == sel.item_id)?;
                let action = item
                    .available_actions
                    .iter()
                    .find(|a| a.id == sel.action_id)?;
                Some((item.clone(), action.clone()))
            })
            .collect()
    };

    if pairs.is_empty() {
        return Err("No valid selections".into());
    }

    // A fresh batch clears any cancellation left over from the previous one.
    state.reset_cancel();
    let cancel = state.cancel_flag();

    tokio::spawn(run_batch(app, pairs, cancel, None));
    Ok(())
}

/// What a finished batch did, for callers that wait on it (Guard mode).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BatchOutcome {
    pub succeeded: usize,
    pub failed: usize,
    pub bytes_freed: u64,
}

/// Run a batch through the executor, record it in history, and keep the
/// authoritative result list in step.
///
/// `trigger` is `None` for a clean the user started. Anything else (`"guard"`)
/// is unattended: its progress is not streamed as `action-event`s — the
/// frontend would mistake them for the user's own batch — and it does not
/// touch the user's cancel flag.
pub(crate) async fn run_batch(
    app: AppHandle,
    pairs: Vec<(CleanableItem, CleanAction)>,
    cancel: Arc<AtomicBool>,
    trigger: Option<String>,
) -> BatchOutcome {
    let interactive = trigger.is_none();

    // Everything needed to write the history record, captured before the
    // batch consumes the pairs.
    let pending: HashMap<uuid::Uuid, PendingRecord> = pairs
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

    let executor = match app.try_state::<AppState>() {
        Some(handle) => handle.create_executor(),
        None => return BatchOutcome::default(),
    };
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
                // Drop it from the authoritative list too, or a second clean
                // could re-select something already gone. An unattended run
                // cleaned items from its own scan, so they are matched on
                // path as well as id.
                if let Some(handle) = app.try_state::<AppState>() {
                    handle.results().retain(|i| {
                        i.id != *item_id && (interactive || Some(&i.path) != path.as_ref())
                    });
                }
                if !interactive {
                    if let Some(path) = &path {
                        let _ = app.emit("guard-cleaned", path);
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
                    if let Some(handle) = app.try_state::<AppState>() {
                        let run = CleanRun {
                            id: uuid::Uuid::new_v4(),
                            started_at,
                            duration_ms: start.elapsed().as_millis() as u64,
                            items: std::mem::take(&mut recorded),
                            trigger: trigger.clone(),
                        };
                        handle.history().push(run);
                        if let Err(err) = handle.persist_history() {
                            let _ = app.emit("app-warning", &err);
                        }
                    }
                }
                if interactive {
                    if let Some(handle) = app.try_state::<AppState>() {
                        handle.reset_cancel();
                    }
                }
                crate::tray::refresh(&app);
            }
            _ => {}
        }
        if interactive {
            let _ = app.emit("action-event", &event);
        }
    }

    outcome
}

/// Details of a queued item, held so a history record can be written when its
/// terminal event arrives.
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

/// Human-readable summary of what an action method actually does.
///
/// For commands this is the literal command line — the only honest disclosure
/// available, since Void does not interpret what a command removes.
pub(crate) fn describe_method(method: &ActionMethod) -> String {
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

/// Stop the running clean batch before its next item.
///
/// Cancellation never interrupts an action already in flight — a half-finished
/// `rm -rf` would leave the user worse off than letting it complete.
#[tauri::command]
pub async fn cancel_clean(state: State<'_, AppState>) -> Result<(), String> {
    state.request_cancel();
    Ok(())
}

/// Capacity of the volume holding the first scan root.
#[tauri::command]
pub async fn get_disk_usage(state: State<'_, AppState>) -> Result<Option<DiskUsage>, String> {
    let root = state
        .config()
        .scan_roots
        .first()
        .cloned()
        .or_else(dirs::home_dir);

    Ok(match root {
        Some(path) => deepclean_core::disk::usage_for_path(&path),
        None => None,
    })
}

/// Aggregated view of past clean runs, shaped for the History screen.
#[derive(Serialize)]
pub struct HistoryView {
    pub runs: Vec<CleanRun>,
    pub total_bytes: u64,
    pub bytes_last_30_days: u64,
    pub run_count: usize,
}

#[tauri::command]
pub async fn get_history(state: State<'_, AppState>) -> Result<HistoryView, String> {
    let history = state.history();
    Ok(HistoryView {
        runs: history.runs.clone(),
        total_bytes: history.total_bytes(),
        bytes_last_30_days: history.bytes_since(30),
        run_count: history.runs.len(),
    })
}

#[tauri::command]
pub async fn clear_history(state: State<'_, AppState>) -> Result<(), String> {
    *state.history() = History::default();
    state.persist_history()
}

/// Problems encountered while loading persisted state at startup.
///
/// Drained on read: they describe the launch that just happened, and repeating
/// them on every poll would be noise.
#[tauri::command]
pub async fn take_startup_warnings(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    Ok(state.take_startup_warnings())
}

#[tauri::command]
pub async fn get_summary(state: State<'_, AppState>) -> Result<ScanSummary, String> {
    let summary = state.summary().clone();
    Ok(summary)
}

#[tauri::command]
pub async fn get_config(
    state: State<'_, AppState>,
) -> Result<deepclean_core::config::AppConfig, String> {
    let config = state.config().clone();
    Ok(config)
}

#[tauri::command]
pub async fn update_config(
    app: AppHandle,
    state: State<'_, AppState>,
    config: deepclean_core::config::AppConfig,
) -> Result<(), String> {
    // The config file is hand-editable and its path is deliberately exposed,
    // so the UI's slider bounds are not a guarantee. Zero concurrency would
    // wedge every future scan on an empty semaphore.
    let config = config.sanitized();
    let previous = std::mem::replace(&mut *state.config(), config.clone());

    // Settings that act on the OS take effect as soon as they are saved, not
    // at the next launch.
    if previous.ui.show_menu_bar_icon != config.ui.show_menu_bar_icon {
        crate::tray::set_visible(&app, config.ui.show_menu_bar_icon);
    }
    if previous.ui.launch_at_login != config.ui.launch_at_login {
        if let Err(err) =
            crate::integrations::apply_launch_at_login(&app, config.ui.launch_at_login)
        {
            // Keep the file honest: it must not claim a login item that the
            // OS refused to register.
            state.config().ui.launch_at_login = previous.ui.launch_at_login;
            let _ = state.persist_config();
            return Err(err);
        }
    }
    crate::tray::refresh(&app);

    // Persist immediately. Settings that silently reset on relaunch are worse
    // than no settings screen at all.
    state.persist_config()
}

/// Show an item in the system file manager.
///
/// Takes the item's id rather than a path: the path is resolved from Void's own
/// scan results, so nothing the frontend sends can point the file manager at an
/// arbitrary location.
///
/// This *reveals* rather than opens. `shell.open` on a file hands it to its
/// default application — clicking Reveal on a downloaded `.xip` would launch
/// the installer instead of showing it.
#[tauri::command]
pub async fn reveal_item(state: State<'_, AppState>, item_id: uuid::Uuid) -> Result<(), String> {
    let path = state
        .results()
        .iter()
        .find(|i| i.id == item_id)
        .map(|i| i.path.clone())
        .ok_or_else(|| "That item is no longer in the scan results.".to_string())?;

    // A cleaned item is gone; showing where it used to live still helps.
    let target = if path.exists() {
        path.clone()
    } else {
        path.parent()
            .filter(|p| p.exists())
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{} no longer exists.", path.display()))?
    };

    reveal_path(&target).map_err(|err| format!("Could not open the file manager: {err}"))
}

/// Platform-specific reveal. Arguments are passed as argv, never through a
/// shell, so a path with spaces or quotes needs no escaping.
fn reveal_path(target: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // -R selects the item in Finder rather than opening it.
        std::process::Command::new("open")
            .arg("-R")
            .arg(target)
            .spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        // explorer.exe exits non-zero even on success, so the status is not
        // checked; `/select,` must be one argument joined to the path.
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(target);
        std::process::Command::new("explorer").arg(arg).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No portable "select the file" on Linux; open the containing folder.
        let dir = if target.is_dir() {
            target
        } else {
            target.parent().unwrap_or(target)
        };
        std::process::Command::new("xdg-open").arg(dir).spawn()?;
    }
    Ok(())
}

/// Where settings are stored, so the About screen can show it and the user can
/// find the file.
#[tauri::command]
pub async fn get_config_path(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.config_path().display().to_string())
}
