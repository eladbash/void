use std::path::Path;

use serde::{Deserialize, Serialize};
use sysinfo::Disks;

/// Capacity of the volume holding a given path.
///
/// The results screen shows reclaimable space as a share of the disk, which is
/// a far better motivator than a raw byte count — but only if the disk is the
/// one the artifacts actually live on.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
}

/// Report usage for the volume containing `path`.
///
/// Picks the mount point with the longest match, so `/Volumes/Work/x` reports
/// the external volume rather than `/`. Returns `None` when no mount point
/// matches, which the UI renders as an absent meter rather than zeros.
pub fn usage_for_path(path: &Path) -> Option<DiskUsage> {
    let disks = Disks::new_with_refreshed_list();

    let best = disks
        .list()
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())?;

    let total = best.total_space();
    let available = best.available_space();

    Some(DiskUsage {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total.saturating_sub(available),
    })
}

/// Report usage for the volume holding the user's home directory.
pub fn usage_for_home() -> Option<DiskUsage> {
    let home = dirs::home_dir()?;
    usage_for_path(&home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_volume_reports_plausible_numbers() {
        let Some(usage) = usage_for_home() else {
            // A sandbox with no enumerable mounts is a legitimate outcome.
            return;
        };
        assert!(usage.total_bytes > 0, "total capacity should be positive");
        assert!(
            usage.available_bytes <= usage.total_bytes,
            "available ({}) must not exceed total ({})",
            usage.available_bytes,
            usage.total_bytes
        );
        assert_eq!(
            usage.used_bytes,
            usage.total_bytes - usage.available_bytes,
            "used must be the difference"
        );
    }

    #[test]
    fn unmatched_path_reports_nothing() {
        // A path under no mount point must not silently report a zeroed disk.
        assert!(usage_for_path(Path::new("relative/not/absolute")).is_none());
    }
}
