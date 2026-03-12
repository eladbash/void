use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct SystemScanner;

#[async_trait]
impl EcosystemScanner for SystemScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::System
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        false
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let path_str = path.to_string_lossy();

        // Trash directories
        if path_str.contains("Trash") || path_str.contains("$Recycle.Bin") {
            if path.is_dir() {
                return analyze_trash(path).await;
            }
            return Ok(None);
        }

        // System logs
        if path_str.ends_with("Library/Logs") || path_str.contains("Library/Logs") {
            if path.is_dir() {
                return analyze_logs(path).await;
            }
            return Ok(None);
        }

        // Individual download entry (file or folder inside Downloads)
        if is_inside_downloads(path) {
            return analyze_download_entry(path).await;
        }

        Ok(None)
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];

        // Enumerate individual entries inside Downloads
        if let Some(dl) = dirs::download_dir() {
            if dl.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&dl) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        // Skip hidden files
                        let name = entry.file_name();
                        if name.to_string_lossy().starts_with('.') {
                            continue;
                        }
                        locs.push(path);
                    }
                }
            }
        }

        // System logs
        if let Some(home) = dirs::home_dir() {
            let logs = home.join("Library/Logs");
            if logs.is_dir() {
                locs.push(logs);
            }
        }

        // Trash
        if let Some(home) = dirs::home_dir() {
            let mac_trash = home.join(".Trash");
            if mac_trash.is_dir() {
                locs.push(mac_trash);
            }

            #[cfg(windows)]
            {
                let recycle = PathBuf::from("C:\\$Recycle.Bin");
                if recycle.is_dir() {
                    locs.push(recycle);
                }
            }
        }

        locs
    }
}

fn is_inside_downloads(path: &Path) -> bool {
    if let Some(dl) = dirs::download_dir() {
        if let Some(parent) = path.parent() {
            return parent == dl;
        }
    }
    false
}

/// Analyze an individual file or folder inside Downloads.
async fn analyze_download_entry(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let size_bytes = if path.is_dir() {
        staleness::compute_dir_size(path).await
    } else if path.is_file() {
        tokio::fs::metadata(path)
            .await
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        return Ok(None);
    };

    if size_bytes == 0 {
        return Ok(None);
    }

    let last_modified: Option<DateTime<Utc>> = tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from);
    let days_stale = last_modified.map(staleness::days_since);

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());

    let mut actions = vec![];

    // Move to Trash (safe, reversible)
    if cfg!(target_os = "macos") {
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Move to Trash".into(),
            description: format!("Move \"{}\" to the Trash", file_name),
            method: ActionMethod::Command {
                program: "osascript".into(),
                args: vec![
                    "-e".into(),
                    format!(
                        "tell application \"Finder\" to delete POSIX file \"{}\"",
                        path.to_string_lossy()
                    ),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        });
    } else {
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Move to Recycle Bin".into(),
            description: format!("Move \"{}\" to the Recycle Bin", file_name),
            method: ActionMethod::Command {
                program: "powershell".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    format!(
                        "$shell = New-Object -ComObject Shell.Application; $shell.Namespace(10).MoveHere('{}')",
                        path.to_string_lossy()
                    ),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        });
    }

    // Permanent delete
    if path.is_dir() {
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Delete permanently".into(),
            description: format!("Permanently delete \"{}\"", file_name),
            method: ActionMethod::Command {
                program: if cfg!(target_os = "macos") { "rm" } else { "powershell" }.into(),
                args: if cfg!(target_os = "macos") {
                    vec!["-rf".into(), path.to_string_lossy().to_string()]
                } else {
                    vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        format!("Remove-Item '{}' -Recurse -Force", path.to_string_lossy()),
                    ]
                },
                working_dir: None,
            },
            risk: RiskLevel::Danger,
            estimated_savings_bytes: size_bytes,
        });
    } else {
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "Delete permanently".into(),
            description: format!("Permanently delete \"{}\"", file_name),
            method: ActionMethod::Command {
                program: if cfg!(target_os = "macos") { "rm" } else { "powershell" }.into(),
                args: if cfg!(target_os = "macos") {
                    vec![path.to_string_lossy().to_string()]
                } else {
                    vec![
                        "-NoProfile".into(),
                        "-Command".into(),
                        format!("Remove-Item '{}' -Force", path.to_string_lossy()),
                    ]
                },
                working_dir: None,
            },
            risk: RiskLevel::Danger,
            estimated_savings_bytes: size_bytes,
        });
    }

    Ok(Some(CleanableItem {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::DownloadsDir,
        risk: RiskLevel::Safe,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified,
        days_stale,
        project_name: Some(file_name),
        project_root: dirs::download_dir(),
        available_actions: actions,
    }))
}

async fn analyze_trash(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let item_count = count_entries(path);
    if item_count == 0 {
        return Ok(None);
    }

    let mut size_bytes = staleness::compute_dir_size(path).await;

    // Fallback: on macOS, ignore walker may fail on .Trash due to permissions.
    // Use `du` as a fallback to get the size.
    if size_bytes == 0 {
        size_bytes = du_fallback_size(path).await;
    }

    let actions = if cfg!(target_os = "macos") {
        vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Empty Trash".into(),
            description: "Permanently delete all items in the Trash".into(),
            method: ActionMethod::Command {
                program: "osascript".into(),
                args: vec![
                    "-e".into(),
                    "tell application \"Finder\" to empty the trash".into(),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Caution,
            estimated_savings_bytes: size_bytes,
        }]
    } else {
        vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Empty Recycle Bin".into(),
            description: "Permanently delete all items in the Recycle Bin".into(),
            method: ActionMethod::Command {
                program: "powershell".into(),
                args: vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    "Clear-RecycleBin -Force".into(),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Caution,
            estimated_savings_bytes: size_bytes,
        }]
    };

    Ok(Some(CleanableItem {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::TrashBin,
        risk: RiskLevel::Caution,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified: staleness::most_recent_modification(path),
        days_stale: None,
        project_name: Some(format!("Trash ({} items)", item_count)),
        project_root: None,
        available_actions: actions,
    }))
}

/// Fallback size computation using `du -sk` (macOS/Linux) or powershell (Windows).
async fn du_fallback_size(path: &Path) -> u64 {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = if cfg!(target_os = "windows") {
            std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "(Get-ChildItem -Recurse -Force '{}' -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum",
                        path.to_string_lossy()
                    ),
                ])
                .output()
        } else {
            std::process::Command::new("du")
                .args(["-sk", &path.to_string_lossy()])
                .output()
        };

        match output {
            Ok(out) if out.status.success() => {
                let s = String::from_utf8_lossy(&out.stdout);
                // `du -sk` outputs "<size_in_kb>\t<path>"
                // powershell outputs just the number
                s.split_whitespace()
                    .next()
                    .and_then(|n| n.trim().parse::<u64>().ok())
                    .map(|kb| if cfg!(target_os = "windows") { kb } else { kb * 1024 })
                    .unwrap_or(0)
            }
            _ => 0,
        }
    })
    .await
    .unwrap_or(0)
}

/// Analyze ~/Library/Logs — offer to remove old log files.
async fn analyze_logs(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let size_bytes = staleness::compute_dir_size(path).await;
    if size_bytes == 0 {
        return Ok(None);
    }

    let last_modified = staleness::most_recent_modification(path);
    let days_stale = last_modified.map(staleness::days_since);

    Ok(Some(CleanableItem {
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::System,
        kind: ArtifactKind::SystemLogs,
        risk: RiskLevel::Safe,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified,
        days_stale,
        project_name: Some("System Logs".into()),
        project_root: None,
        available_actions: vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove logs older than 7 days".into(),
            description: "Delete log files older than 7 days from ~/Library/Logs".into(),
            method: ActionMethod::Command {
                program: "find".into(),
                args: vec![
                    path.to_string_lossy().to_string(),
                    "-type".into(),
                    "f".into(),
                    "-mtime".into(),
                    "+7".into(),
                    "-delete".into(),
                ],
                working_dir: None,
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        }],
    }))
}

fn count_entries(path: &Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| entries.filter(|e| e.is_ok()).count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_always_false() {
        let scanner = SystemScanner;
        assert!(!scanner.is_candidate("Downloads", Path::new("/Users/test/Downloads")));
        assert!(!scanner.is_candidate(".Trash", Path::new("/Users/test/.Trash")));
    }

    #[tokio::test]
    async fn analyze_individual_download_file() {
        let tmp = tempfile::tempdir().unwrap();
        let downloads = tmp.path().join("Downloads");
        std::fs::create_dir(&downloads).unwrap();
        let file = downloads.join("big_archive.zip");
        std::fs::write(&file, vec![0u8; 4096]).unwrap();

        // analyze_download_entry works on individual files
        let result = analyze_download_entry(&file).await.unwrap();
        assert!(result.is_some());
        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::System);
        assert_eq!(item.kind, ArtifactKind::DownloadsDir);
        assert_eq!(item.size_bytes, 4096);
        assert_eq!(item.project_name.as_deref(), Some("big_archive.zip"));
        assert!(item.available_actions.len() >= 2); // trash + delete
    }

    #[tokio::test]
    async fn analyze_individual_download_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let downloads = tmp.path().join("Downloads");
        std::fs::create_dir(&downloads).unwrap();
        let subdir = downloads.join("extracted_app");
        std::fs::create_dir(&subdir).unwrap();
        std::fs::write(subdir.join("file.bin"), vec![0u8; 2048]).unwrap();

        let result = analyze_download_entry(&subdir).await.unwrap();
        assert!(result.is_some());
        let item = result.unwrap();
        assert_eq!(item.size_bytes, 2048);
        assert_eq!(item.project_name.as_deref(), Some("extracted_app"));
    }

    #[tokio::test]
    async fn analyze_trash_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join("Trash");
        std::fs::create_dir(&trash).unwrap();
        std::fs::write(trash.join("deleted_file"), vec![0u8; 512]).unwrap();

        let result = analyze_trash(&trash).await.unwrap();
        assert!(result.is_some());
        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::System);
        assert_eq!(item.kind, ArtifactKind::TrashBin);
        assert_eq!(item.size_bytes, 512);
    }

    #[tokio::test]
    async fn analyze_empty_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("empty.txt");
        std::fs::write(&file, b"").unwrap();

        let result = analyze_download_entry(&file).await.unwrap();
        assert!(result.is_none());
    }
}
