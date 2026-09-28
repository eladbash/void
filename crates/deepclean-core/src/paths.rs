//! The one place Void asks "where is the user's home directory?".
//!
//! Every scanner and the safety checker resolve home through [`home_dir`] so a
//! process can be pointed at a sandbox instead of the real home. The CLI's
//! `--home` flag and the development sandbox (`void dev seed`) rely on this:
//! a scan of a seeded fake home must never reach past it into the real one.
//!
//! The override is process-wide and meant to be set once at startup. Tests do
//! not use it — they construct scanners with an explicit home instead — so
//! parallel test threads never race on it.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

static HOME_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);

tokio::task_local! {
    /// A home scoped to one scanner's work, set by the registry when it roots
    /// scanners somewhere other than the process-wide home. Task-local, so it
    /// follows an `analyze` future across `.await` points and threads, and
    /// two scans with different homes (parallel tests) never see each other's.
    static HOME_SCOPE: PathBuf;
}

/// Run `f` with [`home_dir`] resolving to `home`.
pub fn sync_scope<R>(home: PathBuf, f: impl FnOnce() -> R) -> R {
    HOME_SCOPE.sync_scope(home, f)
}

/// Await `fut` with [`home_dir`] resolving to `home`.
pub async fn scope<F: std::future::Future>(home: PathBuf, fut: F) -> F::Output {
    HOME_SCOPE.scope(home, fut).await
}

/// Point every home-relative lookup at `home` for the rest of the process.
pub fn set_home_override(home: impl AsRef<Path>) {
    let mut guard = HOME_OVERRIDE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(home.as_ref().to_path_buf());
}

/// The sandbox home, if one was set.
pub fn home_override() -> Option<PathBuf> {
    HOME_OVERRIDE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The effective home directory: the override when set, otherwise the real one.
pub fn home_dir() -> Option<PathBuf> {
    HOME_SCOPE
        .try_with(Clone::clone)
        .ok()
        .or_else(home_override)
        .or_else(dirs::home_dir)
}

/// Whether the effective home is not the real one — a sandbox or a test.
/// Scanners that shell out to system tools (`go env`, `docker`) use this to
/// avoid reporting the real machine's state inside a sandbox.
pub fn is_sandboxed() -> bool {
    match (home_dir(), dirs::home_dir()) {
        (Some(effective), Some(real)) => {
            effective.canonicalize().unwrap_or(effective) != real.canonicalize().unwrap_or(real)
        }
        _ => false,
    }
}

/// The effective home directory, falling back to `/` when none can be found.
pub fn home_or_root() -> PathBuf {
    home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_scope_overrides_home_only_inside() {
        let fake = PathBuf::from("/tmp/void-fake-home");
        let inside = sync_scope(fake.clone(), home_dir);
        assert_eq!(inside, Some(fake));
        assert_ne!(home_dir(), Some(PathBuf::from("/tmp/void-fake-home")));
    }

    #[tokio::test]
    async fn async_scope_survives_awaits_and_is_sandboxed() {
        let fake = PathBuf::from("/tmp/void-fake-home-2");
        let (home, sandboxed) = scope(fake.clone(), async {
            tokio::task::yield_now().await;
            (home_dir(), is_sandboxed())
        })
        .await;
        assert_eq!(home, Some(fake));
        assert!(sandboxed);
    }
}
