//! The release check, off the UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use deepclean_core::update::install::{self, InstallError};
use deepclean_core::update::{Check, Feed, Installed, Package, Update};

use super::{Backend, UiEvent};

/// The running app's version, which the check compares against.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

impl Backend {
    /// Ask the release feed whether a newer Void exists. A sandboxed run
    /// without a local `VOID_UPDATE_FEED` can't check, and says so.
    pub fn check_for_updates(&self) -> impl std::future::Future<Output = Result<Check, String>> {
        let done = self.run(async {
            tokio::task::spawn_blocking(|| {
                let feed = Feed::from_env().ok_or_else(|| {
                    "update checks are off in a sandbox; set VOID_UPDATE_FEED to a local file"
                        .to_string()
                })?;
                feed.check(CURRENT_VERSION, Package::desktop())
                    .map_err(|e| e.to_string())
            })
            .await
        });
        async move {
            match done.await {
                Some(Ok(result)) => result,
                _ => Err("the check stopped unexpectedly".into()),
            }
        }
    }

    /// Download, verify and install `update`, reporting progress as
    /// [`UiEvent::UpdateProgress`]. Setting `cancel` stops the download.
    pub fn install_update(
        &self,
        update: Update,
        cancel: Arc<AtomicBool>,
    ) -> impl std::future::Future<Output = Result<Installed, String>> {
        let dir = self
            .state
            .config_path()
            .parent()
            .map(|p| p.join("updates"))
            .unwrap_or_else(|| std::env::temp_dir().join("void-updates"));
        let backend = self.clone();
        let done = self.run(async move {
            tokio::task::spawn_blocking(move || {
                let package = Package::desktop();
                let key = install::trusted_key().map_err(|e| e.to_string())?;
                let target = install::install_target(package).map_err(|e| e.to_string())?;
                let mut last = Instant::now() - Duration::from_secs(1);
                let file = install::download_verified(
                    &update,
                    &dir,
                    CURRENT_VERSION,
                    &key,
                    &|| cancel.load(Ordering::Relaxed),
                    &mut |done, total| {
                        // A few updates a second is plenty for a progress bar.
                        if last.elapsed() >= Duration::from_millis(100) || done == total {
                            last = Instant::now();
                            backend.emit(UiEvent::UpdateProgress { done, total });
                        }
                    },
                )
                .map_err(|e| match e {
                    InstallError::Cancelled => "cancelled".to_string(),
                    e => e.to_string(),
                })?;
                backend.emit(UiEvent::UpdateInstalling);
                let installed =
                    install::install(package, &file, &target, &update.version.to_string())
                        .map_err(|e| e.to_string());
                if !matches!(installed, Ok(Installed::Manual { .. })) {
                    let _ = std::fs::remove_file(&file);
                }
                installed
            })
            .await
        });
        async move {
            match done.await {
                Some(Ok(result)) => result,
                _ => Err("the update stopped unexpectedly".into()),
            }
        }
    }
}
