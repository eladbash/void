use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::history::History;
use deepclean_core::model::{CleanableItem, ScanSummary};
use deepclean_core::safety::SafetyChecker;

/// Application state managed by Tauri.
///
/// Fields are private and reached through the accessors below. That is not
/// ceremony: it is what keeps the lock-poisoning policy in one place instead of
/// spread across every command as `.lock().unwrap()`.
pub struct AppState {
    scan_results: Mutex<Vec<CleanableItem>>,
    scan_summary: Mutex<ScanSummary>,
    is_scanning: Mutex<bool>,
    config: Mutex<AppConfig>,
    history: Mutex<History>,

    /// Where config and history are written. Resolved once at startup from the
    /// Tauri path resolver so the rest of the app never has to know the
    /// platform conventions.
    config_path: PathBuf,
    history_path: PathBuf,

    /// Set to stop the running clean batch before its next item.
    cancel_clean: Arc<AtomicBool>,

    /// Problems encountered while loading state at startup, surfaced to the UI
    /// rather than silently swallowed.
    startup_warnings: Mutex<Vec<String>>,
}

/// Take a lock, recovering rather than panicking if it was poisoned.
///
/// Poisoning means some other thread panicked while holding the lock. None of
/// the state behind these mutexes is a multi-step invariant — each is a plain
/// `Vec`, `bool` or config struct that is fully written under a single lock —
/// so the data is still structurally sound. Propagating the panic instead would
/// turn one failure into a permanently unusable window, where every subsequent
/// scan, clean and settings write panics in turn. Recovering keeps the app
/// answering.
fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl AppState {
    /// Load persisted state from `config_dir`, falling back to defaults.
    pub fn load_from(config_dir: PathBuf) -> Self {
        let config_path = config_dir.join(AppConfig::FILE_NAME);
        let history_path = config_dir.join(History::FILE_NAME);

        let (config, config_err) = AppConfig::load(&config_path);
        let (history, history_err) = History::load(&history_path);

        let warnings = [
            config_err.map(|err| format!("Settings reset to defaults — {err}")),
            history_err.map(|err| format!("Clean history could not be read — {err}")),
        ]
        .into_iter()
        .flatten()
        .collect();

        Self {
            scan_results: Mutex::new(Vec::new()),
            scan_summary: Mutex::new(ScanSummary::default()),
            is_scanning: Mutex::new(false),
            config: Mutex::new(config),
            history: Mutex::new(history),
            config_path,
            history_path,
            cancel_clean: Arc::new(AtomicBool::new(false)),
            startup_warnings: Mutex::new(warnings),
        }
    }

    pub fn results(&self) -> MutexGuard<'_, Vec<CleanableItem>> {
        guard(&self.scan_results)
    }

    pub fn summary(&self) -> MutexGuard<'_, ScanSummary> {
        guard(&self.scan_summary)
    }

    pub fn config(&self) -> MutexGuard<'_, AppConfig> {
        guard(&self.config)
    }

    pub fn history(&self) -> MutexGuard<'_, History> {
        guard(&self.history)
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Claim the scanning slot. Returns `false` when a scan is already running.
    ///
    /// Test-and-set behind one lock, so two rapid triggers — the button and the
    /// tray, say — cannot both start a scan.
    pub fn begin_scan(&self) -> bool {
        let mut scanning = guard(&self.is_scanning);
        if *scanning {
            return false;
        }
        *scanning = true;
        true
    }

    pub fn finish_scan(&self) {
        *guard(&self.is_scanning) = false;
    }

    /// Cancellation flag for the running clean batch.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_clean)
    }

    pub fn request_cancel(&self) {
        self.cancel_clean.store(true, Ordering::Relaxed);
    }

    pub fn reset_cancel(&self) {
        self.cancel_clean.store(false, Ordering::Relaxed);
    }

    /// Startup problems, drained on read: they describe the launch that just
    /// happened, and repeating them on every poll would be noise.
    pub fn take_startup_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *guard(&self.startup_warnings))
    }

    pub fn create_executor(&self) -> ActionExecutor {
        let blocked = self.config().blocked_paths.clone();
        ActionExecutor::new(SafetyChecker::new(blocked))
    }

    /// Write the current config to disk.
    pub fn persist_config(&self) -> Result<(), String> {
        let config = self.config().clone();
        config
            .save(&self.config_path)
            .map_err(|err| format!("Could not save settings: {err}"))
    }

    /// Write the current history to disk.
    pub fn persist_history(&self) -> Result<(), String> {
        let history = self.history().clone();
        history
            .save(&self.history_path)
            .map_err(|err| format!("Could not save clean history: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_defaults_in_an_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::load_from(tmp.path().to_path_buf());

        assert!(state.take_startup_warnings().is_empty());
        assert_eq!(state.config().staleness_threshold_days, 30);
        assert!(state.history().runs.is_empty());
    }

    #[test]
    fn persists_and_reloads_config() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();

        let state = AppState::load_from(dir.clone());
        state.config().staleness_threshold_days = 120;
        state.persist_config().unwrap();

        let reloaded = AppState::load_from(dir);
        assert_eq!(reloaded.config().staleness_threshold_days, 120);
    }

    #[test]
    fn corrupt_config_warns_instead_of_failing_to_start() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(AppConfig::FILE_NAME), "{ broken").unwrap();

        let state = AppState::load_from(tmp.path().to_path_buf());

        let warnings = state.take_startup_warnings();
        assert_eq!(warnings.len(), 1, "expected one warning: {warnings:?}");
        assert!(warnings[0].contains("Settings reset to defaults"));
        assert_eq!(state.config().staleness_threshold_days, 30);
    }

    #[test]
    fn startup_warnings_are_reported_once() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(AppConfig::FILE_NAME), "nonsense").unwrap();

        let state = AppState::load_from(tmp.path().to_path_buf());
        assert_eq!(state.take_startup_warnings().len(), 1);
        assert!(
            state.take_startup_warnings().is_empty(),
            "warnings must drain so they are not repeated on every poll"
        );
    }

    #[test]
    fn only_one_scan_can_claim_the_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let state = AppState::load_from(tmp.path().to_path_buf());

        assert!(state.begin_scan(), "first caller should win");
        assert!(!state.begin_scan(), "second caller must be refused");

        state.finish_scan();
        assert!(
            state.begin_scan(),
            "slot should be reusable after finishing"
        );
    }

    #[test]
    fn a_poisoned_lock_does_not_take_the_app_down() {
        // One panicking thread must not make every later command panic too.
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::load_from(tmp.path().to_path_buf()));

        let poisoner = Arc::clone(&state);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.config();
            panic!("poison the config lock");
        })
        .join();

        // The lock is now poisoned; the accessor must still hand back the data.
        assert_eq!(state.config().staleness_threshold_days, 30);
        state.config().staleness_threshold_days = 45;
        assert_eq!(state.config().staleness_threshold_days, 45);
    }
}
