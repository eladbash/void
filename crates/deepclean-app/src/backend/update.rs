//! The release check, off the UI thread.

use deepclean_core::update::{Check, Feed, Package};

use super::Backend;

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
}
