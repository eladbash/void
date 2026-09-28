//! Guard mode's runtime: the timer, notifications, and opt-in automatic
//! cleanup.
//!
//! The rules — what counts as low, which items a policy picks — live in
//! `deepclean_core::guard` so the CLI and this loop share one tested
//! implementation. This module only decides *when* to ask and what to do with
//! the answer.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

use deepclean_core::config::AppConfig;
use deepclean_core::guard::{self, DiskState, GuardStatus};
use deepclean_core::model::{CleanAction, CleanableItem, RiskLevel, ScanEvent};
use deepclean_core::scanner::{registry, ScanOrchestrator};

use crate::state::AppState;
use crate::tray;

/// Label recorded on history runs Guard mode started.
pub const GUARD_TRIGGER: &str = "guard";

/// While free space stays low, re-run automatic cleanup at most this often.
/// Every attempt is a full background scan; running one every tick on a disk
/// that policies cannot help would cost more than it saves.
const AUTO_CLEAN_COOLDOWN: chrono::Duration = chrono::Duration::hours(6);

/// Guard mode as the UI sees it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GuardView {
    pub enabled: bool,
    pub auto_clean: bool,
    pub status: Option<GuardStatus>,
    pub checked_at: Option<DateTime<Utc>>,
    pub last_auto_clean: Option<AutoCleanReport>,
    /// Why the last check did not clean automatically, when it wanted to —
    /// "CPU busy", "a scan is running". Absent when nothing was skipped.
    pub note: Option<String>,
}

/// What one automatic cleanup did.
#[derive(Debug, Clone, Serialize)]
pub struct AutoCleanReport {
    pub at: DateTime<Utc>,
    pub succeeded: usize,
    pub failed: usize,
    pub bytes_freed: u64,
}

/// Start the background loop. Checks once at launch, then every
/// `guard.check_interval_minutes`, re-reading the config each time so a
/// changed interval applies without a restart.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Let the window and tray finish coming up before the first check.
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            if let Err(err) = check(&app, false).await {
                // No log subscriber in the app; stderr is where `cargo tauri
                // dev` shows it, and the UI keeps showing the last status.
                eprintln!("guard: {err}");
            }
            let minutes = app
                .try_state::<AppState>()
                .map(|s| s.config().guard.check_interval_minutes)
                .unwrap_or(15)
                .max(1);
            tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
        }
    });
}

/// One guard check: measure, classify, notify on a worsening, and clean
/// automatically when the user opted in.
pub async fn check(app: &AppHandle, manual: bool) -> Result<GuardView, String> {
    let state: State<'_, AppState> = app
        .try_state::<AppState>()
        .ok_or_else(|| "App state unavailable".to_string())?;

    if !state.begin_guard() {
        // A check is already running; its result will arrive as an event.
        return Ok(current_view(&state));
    }
    let result = run_check(app, &state, manual).await;
    state.finish_guard();
    result
}

async fn run_check(
    app: &AppHandle,
    state: &State<'_, AppState>,
    manual: bool,
) -> Result<GuardView, String> {
    let config = state.config().clone();
    let root = first_root(&config).ok_or("No scan root to measure")?;

    let usage =
        tauri::async_runtime::spawn_blocking(move || deepclean_core::disk::usage_for_path(&root))
            .await
            .map_err(|err| format!("Disk check failed: {err}"))?
            .ok_or("Could not read free space for the first scan root")?;

    let status = guard::evaluate(&usage, &config.guard);
    let now = Utc::now();
    let (previous, last_auto) = {
        let mut view = state.guard_view();
        let previous = view.status.as_ref().map(|s| s.state);
        view.status = Some(status.clone());
        view.checked_at = Some(now);
        view.note = None;
        (previous, view.last_auto_clean.as_ref().map(|r| r.at))
    };
    publish(app, state);

    // Disabled means "do not bother me": the numbers still feed the tray and
    // the Agents screen, but nothing pops up and nothing is cleaned.
    if !config.guard.enabled {
        return Ok(current_view(state));
    }

    if worsened(previous, status.state) {
        notify(app, notification_title(status.state), &status_body(&status));
    }

    if config.guard.auto_clean && should_auto_clean(previous, status.state, last_auto, now, manual)
    {
        match auto_clean(app, state, &config).await {
            Ok(Some(report)) => {
                if report.succeeded > 0 {
                    notify(
                        app,
                        "Guard freed space",
                        &format!(
                            "Guard freed {} by running your cleanup policies ({} item{}).",
                            tray::format_bytes(report.bytes_freed),
                            report.succeeded,
                            if report.succeeded == 1 { "" } else { "s" }
                        ),
                    );
                }
                state.guard_view().last_auto_clean = Some(report);
            }
            Ok(None) => {
                state.guard_view().note =
                    Some("No enabled policy matched anything to clean.".into());
            }
            Err(note) => state.guard_view().note = Some(note),
        }
        publish(app, state);
    }

    Ok(current_view(state))
}

/// The view with the config-derived flags filled in.
pub fn current_view(state: &State<'_, AppState>) -> GuardView {
    let (enabled, auto_clean) = {
        let config = state.config();
        (config.guard.enabled, config.guard.auto_clean)
    };
    let mut view = state.guard_view().clone();
    view.enabled = enabled;
    view.auto_clean = auto_clean;
    view
}

fn publish(app: &AppHandle, state: &State<'_, AppState>) {
    let _ = app.emit("guard-status", current_view(state));
    tray::refresh(app);
}

fn first_root(config: &AppConfig) -> Option<std::path::PathBuf> {
    config
        .scan_roots
        .first()
        .cloned()
        .or_else(deepclean_core::paths::home_dir)
}

/// True when the disk moved to a worse state than last time. The first
/// check of a session counts as coming from `Ok`, so launching on an
/// already-full disk warns once rather than never.
pub fn worsened(previous: Option<DiskState>, now: DiskState) -> bool {
    now > previous.unwrap_or(DiskState::Ok)
}

/// Whether this check should try an automatic cleanup.
///
/// Only when space is actually low, and then on the transition, on an
/// explicit "Check now", or once the cooldown since the last attempt passed.
pub fn should_auto_clean(
    previous: Option<DiskState>,
    now_state: DiskState,
    last_auto_clean: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    manual: bool,
) -> bool {
    if now_state == DiskState::Ok {
        return false;
    }
    manual
        || worsened(previous, now_state)
        || last_auto_clean.is_none_or(|at| now - at >= AUTO_CLEAN_COOLDOWN)
}

fn notification_title(state: DiskState) -> &'static str {
    match state {
        DiskState::Critical => "Disk space critical",
        DiskState::Low => "Disk space low",
        DiskState::Ok => "Disk space",
    }
}

fn status_body(status: &GuardStatus) -> String {
    if !status.message.is_empty() {
        return status.message.clone();
    }
    format!(
        "{} free ({:.0}% of the disk).",
        tray::format_bytes(status.free_bytes),
        status.free_percent
    )
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    let _ = app.notification().builder().title(title).body(body).show();
}

/// Scan in the background and run the enabled policies' selections.
///
/// `Ok(None)` means nothing matched; `Err` carries why the attempt was
/// skipped, for the UI to show.
async fn auto_clean(
    app: &AppHandle,
    state: &State<'_, AppState>,
    config: &AppConfig,
) -> Result<Option<AutoCleanReport>, String> {
    if state.is_scanning() {
        return Err("Skipped automatic cleanup: a scan is running.".into());
    }

    // `cpu_threshold_percent` is the promise that background work backs off
    // while the machine is busy. Skipping a tick is cheap; the next one will
    // try again.
    let limit = f32::from(config.cpu_threshold_percent);
    match global_cpu_usage().await {
        Some(cpu) if cpu > limit => {
            return Err(format!(
                "Skipped automatic cleanup: CPU at {cpu:.0}% (limit {limit:.0}%)."
            ))
        }
        Some(_) => {}
        None => return Err("Skipped automatic cleanup: CPU usage unavailable.".into()),
    }

    let items = background_scan(config.clone()).await;
    let pairs = safe_pairs(&items, config);
    if pairs.is_empty() {
        return Ok(None);
    }

    let outcome = crate::commands::run_batch(
        app.clone(),
        pairs,
        Arc::new(AtomicBool::new(false)),
        Some(GUARD_TRIGGER.to_string()),
    )
    .await;

    let _ = app.emit("history-changed", ());
    Ok(Some(AutoCleanReport {
        at: Utc::now(),
        succeeded: outcome.succeeded,
        failed: outcome.failed,
        bytes_freed: outcome.bytes_freed,
    }))
}

/// The policies' selections, keeping only `Safe` actions.
///
/// `policy_selections` already guarantees that; checking again here costs
/// nothing and means an unattended delete above Safe needs two bugs, not one.
pub fn safe_pairs(
    items: &[CleanableItem],
    config: &AppConfig,
) -> Vec<(CleanableItem, CleanAction)> {
    guard::policy_selections(items, &config.guard.policies)
        .into_iter()
        .filter(|sel| sel.action.risk == RiskLevel::Safe)
        .map(|sel| (sel.item, sel.action))
        .collect()
}

async fn background_scan(config: AppConfig) -> Vec<CleanableItem> {
    let scanners = registry::build(&config);
    let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
    let mut items = Vec::new();
    while let Some(event) = rx.recv().await {
        if let ScanEvent::ItemFound { item } = event {
            items.push(item);
        }
    }
    match deepclean_core::paths::home_override() {
        Some(home) => deepclean_core::sandbox::confine(items, &home),
        None => items,
    }
}

async fn global_cpu_usage() -> Option<f32> {
    tauri::async_runtime::spawn_blocking(|| {
        // CPU usage is a difference between two samples; the first refresh
        // only establishes the baseline.
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_usage();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(500)));
        sys.refresh_cpu_usage();
        sys.global_cpu_usage()
    })
    .await
    .ok()
}

/// Guard mode's latest verdict, without measuring again.
#[tauri::command]
pub async fn get_guard_status(state: State<'_, AppState>) -> Result<GuardView, String> {
    Ok(current_view(&state))
}

/// Measure now ("Check now" on the Agents screen), running automatic cleanup
/// if it is enabled and space is low.
#[tauri::command]
pub async fn run_guard_now(app: AppHandle) -> Result<GuardView, String> {
    check(&app, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_worse_state_is_a_worsening() {
        assert!(!worsened(None, DiskState::Ok));
        assert!(
            worsened(None, DiskState::Low),
            "launching on a full disk warns once"
        );
        assert!(worsened(Some(DiskState::Ok), DiskState::Critical));
        assert!(worsened(Some(DiskState::Low), DiskState::Critical));
        assert!(
            !worsened(Some(DiskState::Low), DiskState::Low),
            "no repeat nagging"
        );
        assert!(
            !worsened(Some(DiskState::Critical), DiskState::Low),
            "recovering is not news"
        );
    }

    #[test]
    fn auto_clean_never_runs_when_space_is_fine() {
        let now = Utc::now();
        assert!(!should_auto_clean(None, DiskState::Ok, None, now, true));
        assert!(!should_auto_clean(
            Some(DiskState::Low),
            DiskState::Ok,
            None,
            now,
            false
        ));
    }

    #[test]
    fn auto_clean_runs_on_transition_and_manual_checks() {
        let now = Utc::now();
        let just_now = Some(now - chrono::Duration::minutes(5));
        assert!(should_auto_clean(
            Some(DiskState::Ok),
            DiskState::Low,
            just_now,
            now,
            false
        ));
        assert!(should_auto_clean(
            Some(DiskState::Low),
            DiskState::Low,
            just_now,
            now,
            true
        ));
    }

    #[test]
    fn auto_clean_backs_off_while_space_stays_low() {
        let now = Utc::now();
        let recent = Some(now - chrono::Duration::minutes(30));
        let long_ago = Some(now - chrono::Duration::hours(7));
        assert!(!should_auto_clean(
            Some(DiskState::Low),
            DiskState::Low,
            recent,
            now,
            false
        ));
        assert!(should_auto_clean(
            Some(DiskState::Low),
            DiskState::Low,
            long_ago,
            now,
            false
        ));
        assert!(should_auto_clean(
            Some(DiskState::Low),
            DiskState::Low,
            None,
            now,
            false
        ));
    }

    #[test]
    fn notification_copy_falls_back_when_core_has_no_message() {
        let status = GuardStatus {
            state: DiskState::Low,
            free_bytes: 8_100_000_000,
            total_bytes: 100_000_000_000,
            free_percent: 8.1,
            message: String::new(),
        };
        assert_eq!(status_body(&status), "8.1 GB free (8% of the disk).");
        assert_eq!(
            notification_title(DiskState::Critical),
            "Disk space critical"
        );
    }
}
