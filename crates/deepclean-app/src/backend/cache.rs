//! The last scan, kept on disk so the next launch opens to it.
//!
//! Nothing here is trusted for safety: every clean re-checks its gates when
//! it runs, exactly as for a fresh scan. The cache only decides what is shown
//! before the background rescan replaces it. A cache from another app
//! version, another format or other scan roots is ignored, as is one that
//! doesn't parse, and items whose paths are gone are dropped on load.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use deepclean_core::model::CleanableItem;
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "scan-cache.json";

/// Bump when the file's shape changes.
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanCache {
    pub format: u32,
    /// The app (and so scanner) version that wrote it.
    pub version: String,
    pub roots: Vec<PathBuf>,
    pub scanned_at: DateTime<Utc>,
    pub scan_duration_ms: u64,
    pub items: Vec<CleanableItem>,
}

impl ScanCache {
    pub fn new(
        roots: Vec<PathBuf>,
        scanned_at: DateTime<Utc>,
        scan_duration_ms: u64,
        items: Vec<CleanableItem>,
    ) -> Self {
        Self {
            format: FORMAT,
            version: env!("CARGO_PKG_VERSION").to_string(),
            roots,
            scanned_at,
            scan_duration_ms,
            items,
        }
    }
}

/// The cache at `path`, if it is this version's, for these `roots`, and
/// readable — with vanished items dropped.
pub fn load(path: &Path, roots: &[PathBuf]) -> Option<ScanCache> {
    let raw = std::fs::read(path).ok()?;
    let mut cache: ScanCache = serde_json::from_slice(&raw).ok()?;
    if cache.format != FORMAT || cache.version != env!("CARGO_PKG_VERSION") || cache.roots != roots
    {
        return None;
    }
    cache.items.retain(|i| exists(&i.path));
    Some(cache)
}

/// Write atomically: a crash mid-write leaves the previous cache.
pub fn save(path: &Path, cache: &ScanCache) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec(cache).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// The path is still there (a dangling symlink counts: it is what was found).
pub fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

impl super::Backend {
    fn cache_path(&self) -> PathBuf {
        self.state
            .config_path()
            .parent()
            .map(|p| p.join(FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(FILE_NAME))
    }

    /// The last scan, loaded into the results the cleaner works from.
    pub fn load_scan_cache(&self) -> Option<ScanCache> {
        let roots = self.state.config().scan_roots.clone();
        let cache = load(&self.cache_path(), &roots)?;
        *self.state.results() = cache.items.clone();
        Some(cache)
    }

    /// Save the results on screen, off the UI thread.
    pub fn save_scan_cache(
        &self,
        items: Vec<CleanableItem>,
        scanned_at: DateTime<Utc>,
        scan_duration_ms: u64,
    ) {
        let path = self.cache_path();
        let roots = self.state.config().scan_roots.clone();
        self.rt.spawn_blocking(move || {
            let cache = ScanCache::new(roots, scanned_at, scan_duration_ms, items);
            if let Err(err) = save(&path, &cache) {
                eprintln!("could not save the scan cache: {err}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::*;

    fn setup() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>, Vec<CleanableItem>) {
        let tmp = tempfile::tempdir().unwrap();
        let live = tmp.path().join("code/app/node_modules");
        std::fs::create_dir_all(&live).unwrap();
        let a = cleanable(&live.display().to_string(), 100);
        let gone = cleanable(
            &tmp.path().join("code/old/target").display().to_string(),
            50,
        );
        let path = tmp.path().join("cfg").join(FILE_NAME);
        let roots = vec![tmp.path().to_path_buf()];
        (tmp, path, roots, vec![a, gone])
    }

    #[test]
    fn round_trips_and_drops_items_whose_paths_vanished() {
        let (_tmp, path, roots, items) = setup();
        let at = Utc::now();
        save(
            &path,
            &ScanCache::new(roots.clone(), at, 1234, items.clone()),
        )
        .unwrap();
        let loaded = load(&path, &roots).unwrap();
        assert_eq!(loaded.scanned_at, at);
        assert_eq!(loaded.scan_duration_ms, 1234);
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].id, items[0].id);
    }

    #[test]
    fn ignores_other_versions_formats_and_roots() {
        let (_tmp, path, roots, items) = setup();
        let mut cache = ScanCache::new(roots.clone(), Utc::now(), 0, items);
        cache.version = "0.0.1".into();
        save(&path, &cache).unwrap();
        assert!(load(&path, &roots).is_none());

        cache.version = env!("CARGO_PKG_VERSION").into();
        cache.format = FORMAT + 1;
        save(&path, &cache).unwrap();
        assert!(load(&path, &roots).is_none());

        cache.format = FORMAT;
        save(&path, &cache).unwrap();
        assert!(load(&path, &[PathBuf::from("/elsewhere")]).is_none());
        assert!(load(&path, &roots).is_some());
    }

    #[test]
    fn a_corrupt_or_missing_file_is_ignored() {
        let (_tmp, path, roots, _) = setup();
        assert!(load(&path, &roots).is_none());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ truncated").unwrap();
        assert!(load(&path, &roots).is_none());
    }
}
