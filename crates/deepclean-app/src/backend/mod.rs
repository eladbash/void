//! Scans, cleans and Guard mode, without any UI in them.
//!
//! These were the Tauri commands. They now run on a tokio runtime the app
//! owns and report progress as [`UiEvent`]s on a channel the window drains.
//! A send to a closed channel (the window is gone) is simply dropped.

pub mod clean;
pub mod guard;
pub mod integrations;
pub mod scan;

use std::path::PathBuf;
use std::sync::Arc;

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};

use deepclean_core::disk::DiskUsage;
use deepclean_core::history::{CleanRun, History};
use deepclean_core::model::{ActionEvent, ScanEvent};

use crate::state::AppState;
pub use guard::GuardView;

/// Something the window should know about.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Scan(ScanEvent),
    /// The scan's event stream ended, however it ended.
    ScanFinished,
    Action(ActionEvent),
    GuardStatus(GuardView),
    /// Guard's automatic cleanup removed this path.
    GuardCleaned(PathBuf),
    HistoryChanged,
    Warning(String),
    Notify {
        title: String,
        body: String,
    },
    /// Numbers behind the tray's status line changed.
    TrayRefresh,
}

/// The worker runtime lives for the whole process. Background tasks hold
/// `Backend` clones; if one of them dropped the last owner of the runtime on
/// a worker thread, tokio would panic.
fn runtime() -> tokio::runtime::Handle {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("void-worker")
                .build()
                .expect("tokio runtime")
        })
        .handle()
        .clone()
}

/// Handle to the background work. Cheap to clone.
#[derive(Clone)]
pub struct Backend {
    rt: tokio::runtime::Handle,
    state: Arc<AppState>,
    tx: UnboundedSender<UiEvent>,
}

impl Backend {
    pub fn new(state: Arc<AppState>) -> (Self, UnboundedReceiver<UiEvent>) {
        let (tx, rx) = unbounded();
        (
            Self {
                rt: runtime(),
                state,
                tx,
            },
            rx,
        )
    }

    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.rt
    }

    pub(crate) fn emit(&self, event: UiEvent) {
        let _ = self.tx.unbounded_send(event);
    }

    /// Run a future on the worker runtime and hand its result back as a
    /// future any executor (GPUI's included) can await.
    pub fn run<F>(&self, fut: F) -> impl std::future::Future<Output = Option<F::Output>> + 'static
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = futures::channel::oneshot::channel();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        async move { rx.await.ok() }
    }

    /// Capacity of the volume holding the first scan root.
    pub fn disk_usage(&self) -> Option<DiskUsage> {
        let root = self
            .state
            .config()
            .scan_roots
            .first()
            .cloned()
            .or_else(deepclean_core::paths::home_dir)?;
        deepclean_core::disk::usage_for_path(&root)
    }

    pub fn history_view(&self) -> HistoryView {
        let history = self.state.history();
        HistoryView {
            runs: history.runs.clone(),
            total_bytes: history.total_bytes(),
            bytes_last_30_days: history.bytes_since(30),
            run_count: history.runs.len(),
        }
    }

    pub fn clear_history(&self) -> Result<(), String> {
        *self.state.history() = History::default();
        self.state.persist_history()
    }
}

/// Aggregated view of past clean runs, shaped for the History screen.
#[derive(Debug, Clone, Default)]
pub struct HistoryView {
    pub runs: Vec<CleanRun>,
    pub total_bytes: u64,
    pub bytes_last_30_days: u64,
    pub run_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window can close mid-scan. The scan must still finish, release
    /// its latch and accept the next one, with nobody listening.
    #[test]
    fn scan_survives_dropped_receiver() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("proj/node_modules/x")).unwrap();
        std::fs::write(root.join("proj/package.json"), "{}").unwrap();
        let state = Arc::new(AppState::load_from(tmp.path().join("cfg")));
        state.config().scan_roots = vec![root];
        let (backend, events) = Backend::new(state.clone());
        drop(events);

        backend.start_scan().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while state.is_scanning() {
            assert!(std::time::Instant::now() < deadline, "scan never finished");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        backend.start_scan().expect("the latch was released");
    }
}
