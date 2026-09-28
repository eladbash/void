use std::path::Path;

use chrono::{DateTime, Utc};

/// Find the most recent modification time in a directory (shallow scan).
pub fn most_recent_modification(path: &Path) -> Option<DateTime<Utc>> {
    let newest = std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .max();

    newest.map(DateTime::<Utc>::from)
}

/// Compute the number of days since a given datetime.
pub fn days_since(dt: DateTime<Utc>) -> u64 {
    let now = Utc::now();
    let duration = now.signed_duration_since(dt);
    duration.num_days().max(0) as u64
}

/// Recursively compute the total size of a directory.
pub async fn compute_dir_size(path: &Path) -> u64 {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || compute_dir_size_sync(&path))
        .await
        .unwrap_or(0)
}

/// Sum the bytes a directory really occupies.
///
/// Two corrections over a plain `len()` sum, both of which matter for AI-era
/// bloat:
///
/// - **Hardlinks are counted once.** pnpm stores, `uv` caches and
///   deduplicated model files hardlink the same inode from many places;
///   summing every link reported space that deleting one of them never frees.
/// - **Sparse files count their allocated blocks.** `Docker.raw` claims 64 GB
///   but may hold 9; a model download in progress is preallocated. The size
///   shown is `min(apparent, allocated)`, so tiny files (whose one allocated
///   block exceeds their length) keep their exact byte count.
pub fn compute_dir_size_sync(path: &Path) -> u64 {
    let mut seen_inodes = std::collections::HashSet::new();
    ignore::WalkBuilder::new(path)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| first_link(meta, &mut seen_inodes))
        .map(|meta| on_disk_len(&meta))
        .sum()
}

/// Bytes a single file occupies, per the rules of [`compute_dir_size_sync`].
pub fn on_disk_len(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.len().min(meta.blocks().saturating_mul(512))
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// True the first time an inode is seen; false for further links to it.
fn first_link(meta: &std::fs::Metadata, seen: &mut std::collections::HashSet<(u64, u64)>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() > 1 {
            return seen.insert((meta.dev(), meta.ino()));
        }
        true
    }
    #[cfg(not(unix))]
    {
        let _ = (meta, seen);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_since_now_is_zero() {
        assert_eq!(days_since(Utc::now()), 0);
    }

    #[test]
    fn most_recent_on_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty dir has no entries
        assert!(most_recent_modification(tmp.path()).is_none());
    }

    #[tokio::test]
    async fn compute_size_of_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "world!").unwrap();
        let size = compute_dir_size(tmp.path()).await;
        assert_eq!(size, 11); // 5 + 6
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hardlinked_file_is_counted_once() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("weights.bin"), vec![1u8; 8192]).unwrap();
        std::fs::hard_link(tmp.path().join("weights.bin"), tmp.path().join("link.bin")).unwrap();
        assert_eq!(compute_dir_size(tmp.path()).await, 8192);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sparse_file_counts_allocated_blocks_not_apparent_length() {
        let tmp = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(tmp.path().join("Docker.raw")).unwrap();
        // 1 GiB apparent, nothing written.
        file.set_len(1 << 30).unwrap();
        drop(file);
        let size = compute_dir_size(tmp.path()).await;
        assert!(size < 1 << 20, "sparse file reported as {size} bytes");
    }
}
