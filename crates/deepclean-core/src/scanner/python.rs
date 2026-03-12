use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct PythonScanner;

#[async_trait]
impl EcosystemScanner for PythonScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Python
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        match file_name {
            "__pycache__" => true,
            ".venv" | "venv" => {
                let Some(parent) = path.parent() else {
                    return false;
                };
                is_python_project_dir(parent)
            }
            _ => false,
        }
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let path_str = path.to_string_lossy();

        match file_name.as_str() {
            "__pycache__" => self.analyze_pycache(path).await,
            ".venv" | "venv" => self.analyze_venv(path).await,
            _ => {
                if path_str.contains("pip") {
                    self.analyze_pip_cache(path).await
                } else if path_str.contains("conda") {
                    self.analyze_conda_cache(path).await
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];
        if let Some(home) = dirs::home_dir() {
            // pip cache (Linux default)
            let pip_cache = home.join(".cache/pip");
            if pip_cache.is_dir() {
                locs.push(pip_cache);
            }

            // pip cache (macOS alternative)
            let pip_cache_mac = home.join("Library/Caches/pip");
            if pip_cache_mac.is_dir() {
                locs.push(pip_cache_mac);
            }

            // Conda package cache
            let conda_pkgs = home.join(".conda/pkgs");
            if conda_pkgs.is_dir() {
                locs.push(conda_pkgs);
            }
        }
        locs
    }
}

impl PythonScanner {
    async fn analyze_pycache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        let parent = path.parent();
        let project_name = parent
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string());

        debug!("Found __pycache__ at {} ({})", path.display(), ByteSize(size_bytes));

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Python,
            kind: ArtifactKind::PycacheDir,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name,
            project_root: parent.map(|p| p.to_path_buf()),
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove __pycache__/".into(),
                description: "Delete the __pycache__ directory (recreated automatically)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_venv(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        let parent = path.parent();
        let project_name = parent
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string());

        let venv_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "venv".into());

        debug!("Found venv at {} ({})", path.display(), ByteSize(size_bytes));

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Python,
            kind: ArtifactKind::VenvDir,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name,
            project_root: parent.map(|p| p.to_path_buf()),
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: format!("Remove {}/", venv_name),
                description: "Delete the virtual environment (recreate with `python -m venv`)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_pip_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        debug!("Found pip cache at {} ({})", path.display(), ByteSize(size_bytes));

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Python,
            kind: ArtifactKind::PipCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("pip cache".into()),
            project_root: None,
            available_actions: vec![
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "pip cache purge".into(),
                    description: "Run `pip cache purge` to clear the pip download cache".into(),
                    method: ActionMethod::Command {
                        program: "pip".into(),
                        args: vec!["cache".into(), "purge".into()],
                        working_dir: None,
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                },
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove directory".into(),
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

    async fn analyze_conda_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        debug!("Found conda cache at {} ({})", path.display(), ByteSize(size_bytes));

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Python,
            kind: ArtifactKind::CondaCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Conda packages".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove conda packages cache".into(),
                description: "Delete cached conda packages (will re-download when needed)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }
}

/// Check whether a directory contains Python project markers.
fn is_python_project_dir(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if matches!(
                name_str.as_ref(),
                "requirements.txt" | "setup.py" | "pyproject.toml" | "Pipfile"
            ) {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_pycache() {
        let scanner = PythonScanner;
        assert!(scanner.is_candidate("__pycache__", Path::new("/app/src/__pycache__")));
    }

    #[test]
    fn is_candidate_venv_with_requirements() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myproject");
        std::fs::create_dir_all(project.join(".venv")).unwrap();
        std::fs::write(project.join("requirements.txt"), "flask\n").unwrap();

        let scanner = PythonScanner;
        assert!(scanner.is_candidate(".venv", &project.join(".venv")));
    }

    #[test]
    fn is_candidate_venv_with_pyproject_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myproject");
        std::fs::create_dir_all(project.join("venv")).unwrap();
        std::fs::write(project.join("pyproject.toml"), "[tool.poetry]").unwrap();

        let scanner = PythonScanner;
        assert!(scanner.is_candidate("venv", &project.join("venv")));
    }

    #[test]
    fn is_candidate_venv_without_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("notpython");
        std::fs::create_dir_all(project.join("venv")).unwrap();

        let scanner = PythonScanner;
        assert!(!scanner.is_candidate("venv", &project.join("venv")));
    }

    #[test]
    fn is_candidate_rejects_unrelated() {
        let scanner = PythonScanner;
        assert!(!scanner.is_candidate("target", Path::new("/some/target")));
        assert!(!scanner.is_candidate("node_modules", Path::new("/some/node_modules")));
    }

    #[tokio::test]
    async fn analyze_pycache_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let pycache = tmp.path().join("src/__pycache__");
        std::fs::create_dir_all(&pycache).unwrap();
        std::fs::write(pycache.join("module.cpython-311.pyc"), "fake bytecode").unwrap();

        let scanner = PythonScanner;
        let result = scanner.analyze(&pycache).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Python);
        assert_eq!(item.kind, ArtifactKind::PycacheDir);
        assert!(item.size_bytes > 0);
    }

    #[tokio::test]
    async fn analyze_venv_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join(".venv");
        std::fs::create_dir_all(venv.join("lib/python3.11/site-packages")).unwrap();
        std::fs::write(venv.join("lib/python3.11/site-packages/pkg.py"), "# pkg").unwrap();

        let scanner = PythonScanner;
        let result = scanner.analyze(&venv).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Python);
        assert_eq!(item.kind, ArtifactKind::VenvDir);
        assert!(item.size_bytes > 0);
    }

    #[tokio::test]
    async fn analyze_empty_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("__pycache__");
        std::fs::create_dir_all(&empty).unwrap();

        let scanner = PythonScanner;
        let result = scanner.analyze(&empty).await.unwrap();
        assert!(result.is_none());
    }
}
