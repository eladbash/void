use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct NodeScanner;

/// Detected package manager for a Node.js project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
}

impl PackageManager {
    /// Detect the package manager from lock files in the project root.
    fn detect(project_root: &Path) -> PackageManager {
        if project_root.join("pnpm-lock.yaml").exists() {
            PackageManager::Pnpm
        } else if project_root.join("yarn.lock").exists() {
            PackageManager::Yarn
        } else {
            // Default to npm (package-lock.json or no lock file)
            PackageManager::Npm
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
        }
    }
}

#[async_trait]
impl EcosystemScanner for NodeScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Node
    }

    fn is_candidate(&self, file_name: &str, _path: &Path) -> bool {
        file_name == "node_modules"
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // For global cache locations, handle them differently
        if file_name != "node_modules" {
            return self.analyze_global_cache(path).await;
        }

        // Confirm sibling package.json exists
        let Some(parent) = path.parent() else {
            return Ok(None);
        };

        if !parent.join("package.json").exists() {
            return Ok(None);
        }

        let project_name = parent
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());

        let pkg_manager = PackageManager::detect(parent);
        debug!(
            "Node project '{}' uses {} ({})",
            project_name,
            pkg_manager.name(),
            path.display()
        );

        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        let mut actions = vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove node_modules/".into(),
            description: "Delete the entire node_modules directory".into(),
            method: ActionMethod::RemoveDir {
                path: path.to_path_buf(),
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        }];

        // Add package-manager-specific cache clean commands
        match pkg_manager {
            PackageManager::Npm => {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "npm cache clean".into(),
                    description: "Run `npm cache clean --force` to clear the npm cache".into(),
                    method: ActionMethod::Command {
                        program: "npm".into(),
                        args: vec!["cache".into(), "clean".into(), "--force".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: 0, // unknown until scanned
                });
            }
            PackageManager::Yarn => {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "yarn cache clean".into(),
                    description: "Run `yarn cache clean` to clear the Yarn cache".into(),
                    method: ActionMethod::Command {
                        program: "yarn".into(),
                        args: vec!["cache".into(), "clean".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: 0,
                });
            }
            PackageManager::Pnpm => {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "pnpm store prune".into(),
                    description: "Run `pnpm store prune` to remove unreferenced packages".into(),
                    method: ActionMethod::Command {
                        program: "pnpm".into(),
                        args: vec!["store".into(), "prune".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: 0,
                });
            }
        }

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Node,
            kind: ArtifactKind::NodeModules,
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
            // npm cache
            let npm_cache = home.join(".npm/_cacache");
            if npm_cache.is_dir() {
                locs.push(npm_cache);
            }

            // Yarn cache (v1 default location)
            let yarn_cache = home.join(".cache/yarn");
            if yarn_cache.is_dir() {
                locs.push(yarn_cache);
            }
            // macOS Yarn cache location
            let yarn_cache_mac = home.join("Library/Caches/Yarn");
            if yarn_cache_mac.is_dir() {
                locs.push(yarn_cache_mac);
            }

            // pnpm store
            let pnpm_store = home.join(".local/share/pnpm/store");
            if pnpm_store.is_dir() {
                locs.push(pnpm_store);
            }
            // macOS pnpm store location
            let pnpm_store_mac = home.join("Library/pnpm/store");
            if pnpm_store_mac.is_dir() {
                locs.push(pnpm_store_mac);
            }
        }
        locs
    }
}

impl NodeScanner {
    /// Analyze a global cache directory (npm cache, yarn cache, pnpm store).
    async fn analyze_global_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        let path_str = path.to_string_lossy();

        let (kind, label, description, action) = if path_str.contains(".npm") {
            (
                ArtifactKind::NpmCache,
                "npm cache clean",
                "Run `npm cache clean --force` to clear npm cache",
                ActionMethod::Command {
                    program: "npm".into(),
                    args: vec!["cache".into(), "clean".into(), "--force".into()],
                    working_dir: None,
                },
            )
        } else if path_str.contains("yarn") || path_str.contains("Yarn") {
            (
                ArtifactKind::YarnCache,
                "yarn cache clean",
                "Run `yarn cache clean` to clear Yarn cache",
                ActionMethod::Command {
                    program: "yarn".into(),
                    args: vec!["cache".into(), "clean".into()],
                    working_dir: None,
                },
            )
        } else {
            (
                ArtifactKind::PnpmStore,
                "pnpm store prune",
                "Run `pnpm store prune` to prune the pnpm store",
                ActionMethod::Command {
                    program: "pnpm".into(),
                    args: vec!["store".into(), "prune".into()],
                    working_dir: None,
                },
            )
        };

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Node,
            kind,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: None,
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: label.into(),
                description: description.into(),
                method: action,
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_node_modules() {
        let scanner = NodeScanner;
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp.path().join("myapp/node_modules");
        assert!(scanner.is_candidate("node_modules", &nm));
    }

    #[test]
    fn is_candidate_rejects_other_dirs() {
        let scanner = NodeScanner;
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("myapp/target");
        assert!(!scanner.is_candidate("target", &target));
        assert!(!scanner.is_candidate("src", &tmp.path().join("src")));
    }

    #[test]
    fn detect_npm_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        assert_eq!(PackageManager::detect(tmp.path()), PackageManager::Npm);
    }

    #[test]
    fn detect_yarn_from_lock() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("yarn.lock"), "").unwrap();
        assert_eq!(PackageManager::detect(tmp.path()), PackageManager::Yarn);
    }

    #[test]
    fn detect_pnpm_from_lock() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(PackageManager::detect(tmp.path()), PackageManager::Pnpm);
    }

    #[tokio::test]
    async fn analyze_node_modules_with_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("node_modules/.package-lock.json")).unwrap();
        std::fs::write(project.join("package.json"), r#"{"name": "myapp"}"#).unwrap();
        std::fs::write(
            project.join("node_modules/some-package"),
            "fake package content",
        )
        .unwrap();

        let scanner = NodeScanner;
        let result = scanner
            .analyze(&project.join("node_modules"))
            .await
            .unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Node);
        assert_eq!(item.kind, ArtifactKind::NodeModules);
        assert!(item.size_bytes > 0);
        assert_eq!(item.project_name.as_deref(), Some("myapp"));
        assert!(!item.available_actions.is_empty());
    }

    #[tokio::test]
    async fn analyze_node_modules_without_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        std::fs::write(
            project.join("node_modules/some-package"),
            "fake package content",
        )
        .unwrap();

        let scanner = NodeScanner;
        let result = scanner
            .analyze(&project.join("node_modules"))
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
