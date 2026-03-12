use std::sync::Mutex;

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::model::{CleanableItem, ScanSummary};
use deepclean_core::safety::SafetyChecker;

/// Application state managed by Tauri.
pub struct AppState {
    pub scan_results: Mutex<Vec<CleanableItem>>,
    pub scan_summary: Mutex<ScanSummary>,
    pub is_scanning: Mutex<bool>,
    pub config: Mutex<AppConfig>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            scan_results: Mutex::new(Vec::new()),
            scan_summary: Mutex::new(ScanSummary::default()),
            is_scanning: Mutex::new(false),
            config: Mutex::new(AppConfig::default()),
        }
    }

    pub fn create_executor(&self) -> ActionExecutor {
        let config = self.config.lock().unwrap();
        let safety = SafetyChecker::new(config.blocked_paths.clone());
        ActionExecutor::new(safety)
    }
}
