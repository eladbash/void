use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct JetBrainsScanner;

#[async_trait]
impl EcosystemScanner for JetBrainsScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::JetBrains
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        // JetBrains scanner is entirely global-based.
        false
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        // Extract IDE name from directory name (e.g., "IntelliJIdea2024.1" -> "IntelliJ IDEA 2024.1")
        let dir_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let ide_name = prettify_ide_name(&dir_name);

        debug!(
            "Found JetBrains cache for {} at {} ({})",
            ide_name,
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::JetBrains,
            kind: ArtifactKind::JetBrainsCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some(ide_name),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: format!("Remove {dir_name} cache"),
                description: "Delete IDE cache (rebuilt on next IDE launch)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];

        if let Some(home) = dirs::home_dir() {
            // macOS
            let mac_caches = home.join("Library/Caches/JetBrains");
            if mac_caches.is_dir() {
                collect_ide_dirs(&mac_caches, &mut locs);
            }

            // Linux
            let linux_caches = home.join(".cache/JetBrains");
            if linux_caches.is_dir() {
                collect_ide_dirs(&linux_caches, &mut locs);
            }
        }

        // Windows: %LOCALAPPDATA%\JetBrains
        #[cfg(windows)]
        {
            if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                let win_caches = PathBuf::from(local_app_data).join("JetBrains");
                if win_caches.is_dir() {
                    collect_ide_dirs(&win_caches, &mut locs);
                }
            }
        }

        locs
    }
}

/// Enumerate subdirectories (one per IDE version) inside a JetBrains cache root.
fn collect_ide_dirs(parent: &Path, locs: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                locs.push(entry.path());
            }
        }
    }
}

/// Convert a JetBrains directory name into a human-readable IDE name.
/// E.g., "IntelliJIdea2024.1" -> "IntelliJ IDEA 2024.1"
fn prettify_ide_name(dir_name: &str) -> String {
    let known = [
        ("IntelliJIdea", "IntelliJ IDEA"),
        ("PyCharm", "PyCharm"),
        ("WebStorm", "WebStorm"),
        ("CLion", "CLion"),
        ("GoLand", "GoLand"),
        ("RustRover", "RustRover"),
        ("Rider", "Rider"),
        ("DataGrip", "DataGrip"),
        ("PhpStorm", "PhpStorm"),
        ("RubyMine", "RubyMine"),
        ("AndroidStudio", "Android Studio"),
    ];

    for (prefix, display) in &known {
        if let Some(version) = dir_name.strip_prefix(prefix) {
            if version.is_empty() {
                return display.to_string();
            }
            return format!("{display} {version}");
        }
    }

    dir_name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_always_false() {
        let scanner = JetBrainsScanner;
        assert!(!scanner.is_candidate("JetBrains", Path::new("/Library/Caches/JetBrains")));
        assert!(!scanner.is_candidate("anything", Path::new("/anything")));
    }

    #[test]
    fn prettify_known_ides() {
        assert_eq!(
            prettify_ide_name("IntelliJIdea2024.1"),
            "IntelliJ IDEA 2024.1"
        );
        assert_eq!(prettify_ide_name("PyCharm2024.1"), "PyCharm 2024.1");
        assert_eq!(prettify_ide_name("WebStorm2023.3"), "WebStorm 2023.3");
        assert_eq!(prettify_ide_name("CLion2024.2"), "CLion 2024.2");
        assert_eq!(prettify_ide_name("GoLand2024.1"), "GoLand 2024.1");
        assert_eq!(prettify_ide_name("RustRover2024.1"), "RustRover 2024.1");
        assert_eq!(
            prettify_ide_name("AndroidStudio2024.1"),
            "Android Studio 2024.1"
        );
    }

    #[test]
    fn prettify_unknown_dir() {
        assert_eq!(prettify_ide_name("SomeNewIDE2025.1"), "SomeNewIDE2025.1");
    }

    #[tokio::test]
    async fn analyze_jetbrains_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let ide_cache = tmp.path().join("IntelliJIdea2024.1");
        std::fs::create_dir_all(ide_cache.join("caches")).unwrap();
        std::fs::write(ide_cache.join("caches/some_index"), "cached data").unwrap();

        let scanner = JetBrainsScanner;
        let result = scanner.analyze(&ide_cache).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::JetBrains);
        assert_eq!(item.kind, ArtifactKind::JetBrainsCache);
        assert!(item.size_bytes > 0);
        assert_eq!(item.project_name.as_deref(), Some("IntelliJ IDEA 2024.1"));
    }

    #[tokio::test]
    async fn analyze_empty_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("PyCharm2024.1");
        std::fs::create_dir_all(&empty).unwrap();

        let scanner = JetBrainsScanner;
        let result = scanner.analyze(&empty).await.unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn collect_ide_dirs_finds_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("IntelliJIdea2024.1")).unwrap();
        std::fs::create_dir_all(tmp.path().join("PyCharm2024.1")).unwrap();
        std::fs::write(tmp.path().join("some_file"), "not a dir").unwrap();

        let mut locs = vec![];
        collect_ide_dirs(tmp.path(), &mut locs);
        assert_eq!(locs.len(), 2);
    }
}
