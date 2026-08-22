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

fn compute_dir_size_sync(path: &Path) -> u64 {
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
        .map(|meta| meta.len())
        .sum()
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
}
