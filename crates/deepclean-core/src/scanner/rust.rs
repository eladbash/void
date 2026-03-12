use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct RustScanner;

#[async_trait]
impl EcosystemScanner for RustScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Rust
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        if file_name != "target" {
            return false;
        }
        // Must be a directory with a sibling Cargo.toml
        let Some(parent) = path.parent() else {
            return false;
        };
        parent.join("Cargo.toml").exists()
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        let parent = path.parent().unwrap();
        let project_name = parent
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());

        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        let mut actions = vec![
            CleanAction {
                id: Uuid::new_v4(),
                label: "cargo clean".into(),
                description: "Run `cargo clean` to remove build artifacts".into(),
                method: ActionMethod::Command {
                    program: "cargo".into(),
                    args: vec!["clean".into()],
                    working_dir: Some(parent.to_path_buf()),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            },
            CleanAction {
                id: Uuid::new_v4(),
                label: "Remove target/".into(),
                description: "Delete the entire target directory".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            },
        ];

        // Add release-only clean if release dir exists
        let release_dir = path.join("release");
        if release_dir.is_dir() {
            let release_size = staleness::compute_dir_size(&release_dir).await;
            if release_size > 0 {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove release builds only".into(),
                    description: "Delete target/release/ but keep debug builds".into(),
                    method: ActionMethod::RemoveDir { path: release_dir },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: release_size,
                });
            }
        }

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Rust,
            kind: ArtifactKind::TargetDir,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some(project_name),
            project_root: Some(parent.to_path_buf()),
            available_actions: actions,
        }))
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];
        if let Some(home) = dirs::home_dir() {
            let registry = home.join(".cargo/registry");
            if registry.is_dir() {
                locs.push(registry);
            }
            let git = home.join(".cargo/git");
            if git.is_dir() {
                locs.push(git);
            }
        }
        locs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_with_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myproject");
        std::fs::create_dir_all(project.join("target")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let scanner = RustScanner;
        assert!(scanner.is_candidate("target", &project.join("target")));
    }

    #[test]
    fn is_candidate_without_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("notrust");
        std::fs::create_dir_all(project.join("target")).unwrap();

        let scanner = RustScanner;
        assert!(!scanner.is_candidate("target", &project.join("target")));
    }

    #[tokio::test]
    async fn analyze_finds_target_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myproject");
        std::fs::create_dir_all(project.join("target/debug")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        std::fs::write(project.join("target/debug/binary"), "fake binary content").unwrap();

        let scanner = RustScanner;
        let result = scanner.analyze(&project.join("target")).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Rust);
        assert_eq!(item.kind, ArtifactKind::TargetDir);
        assert!(item.size_bytes > 0);
        assert_eq!(item.project_name.as_deref(), Some("myproject"));
        assert!(item.available_actions.len() >= 2);
    }
}
