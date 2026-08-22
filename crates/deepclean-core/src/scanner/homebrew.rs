use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct HomebrewScanner;

#[async_trait]
impl EcosystemScanner for HomebrewScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Homebrew
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        // Homebrew scanner is entirely global-based.
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

        debug!(
            "Found Homebrew cache at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Homebrew,
            kind: ArtifactKind::HomebrewCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Homebrew".into()),
            project_root: None,
            available_actions: vec![
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "brew cleanup".into(),
                    description: "Run `brew cleanup --prune=all` to remove old bottle downloads"
                        .into(),
                    method: ActionMethod::Command {
                        program: "brew".into(),
                        args: vec!["cleanup".into(), "--prune=all".into()],
                        working_dir: None,
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                },
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove cache directory".into(),
                    description: format!("Delete {}", path.display()),
                    method: ActionMethod::RemoveDir {
                        path: path.to_path_buf(),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                },
            ],
        }))
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        super::existing_home_dirs(["Library/Caches/Homebrew"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_always_false() {
        let scanner = HomebrewScanner;
        assert!(!scanner.is_candidate("Homebrew", Path::new("/usr/local/Homebrew")));
        assert!(!scanner.is_candidate("anything", Path::new("/anything")));
    }

    #[tokio::test]
    async fn analyze_homebrew_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("Homebrew");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("some-bottle.tar.gz"), "fake bottle data").unwrap();

        let scanner = HomebrewScanner;
        let result = scanner.analyze(&cache).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Homebrew);
        assert_eq!(item.kind, ArtifactKind::HomebrewCache);
        assert!(item.size_bytes > 0);
        assert!(item.available_actions.len() >= 2);
    }

    #[tokio::test]
    async fn analyze_empty_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty-cache");
        std::fs::create_dir_all(&empty).unwrap();

        let scanner = HomebrewScanner;
        let result = scanner.analyze(&empty).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn analyze_nonexistent_returns_none() {
        let scanner = HomebrewScanner;
        let result = scanner
            .analyze(Path::new("/nonexistent/path"))
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
