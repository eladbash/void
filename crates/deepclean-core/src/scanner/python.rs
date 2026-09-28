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
            // `pyvenv.cfg` is written by every venv creator (venv, virtualenv,
            // uv, poetry) and is far more reliable than guessing from the
            // parent's files — it also catches venvs in projects that keep
            // their manifest elsewhere.
            ".venv" | "venv" | "env" if path.join("pyvenv.cfg").is_file() => true,
            ".venv" | "venv" => path.parent().is_some_and(is_python_project_dir),
            // These names are only ever written by the tools they belong to.
            ".pytest_cache" | ".ruff_cache" | ".mypy_cache" => true,
            // `.tox`/`.nox`/`.hypothesis` are plausible names for other
            // things, so require the project to actually use them.
            ".tox" => path
                .parent()
                .is_some_and(|p| has_any(p, &["tox.ini", "pyproject.toml", "setup.cfg"])),
            ".nox" => path
                .parent()
                .is_some_and(|p| has_any(p, &["noxfile.py", "pyproject.toml"])),
            ".hypothesis" => path.parent().is_some_and(is_python_project_dir),
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

        match file_name.as_str() {
            "__pycache__" => self.analyze_pycache(path).await,
            ".venv" | "venv" | "env" => self.analyze_venv(path).await,
            name if TOOL_CACHES.contains(&name) => self.analyze_tool_cache(path).await,
            _ => match classify_global(path) {
                Some(GlobalCache::Uv) => self.analyze_uv_cache(path).await,
                Some(GlobalCache::Pip) => self.analyze_pip_cache(path).await,
                Some(GlobalCache::Conda) => self.analyze_conda_cache(path).await,
                None => Ok(None),
            },
        }
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        super::existing_home_dirs([
            ".cache/pip",             // pip, Linux default
            "Library/Caches/pip",     // pip, macOS
            ".cache/uv",              // uv, Linux (and macOS with XDG)
            "Library/Caches/uv",      // uv, macOS
            "AppData/Local/uv/cache", // uv, Windows
            ".conda/pkgs",            // Conda packages
            "miniconda3/pkgs",
            "anaconda3/pkgs",
            "miniforge3/pkgs",
            "mambaforge/pkgs",
        ])
    }
}

/// Project-local caches written by Python dev tools; all regenerate on the
/// next run.
const TOOL_CACHES: [&str; 6] = [
    ".pytest_cache",
    ".ruff_cache",
    ".mypy_cache",
    ".tox",
    ".nox",
    ".hypothesis",
];

/// Which global cache a `global_locations` path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlobalCache {
    Uv,
    Pip,
    Conda,
}

/// Classify by trailing path components rather than substrings: a home
/// directory called `/Users/pipeline` must not make every cache "pip".
fn classify_global(path: &Path) -> Option<GlobalCache> {
    if path.ends_with("uv") || path.ends_with("uv/cache") {
        Some(GlobalCache::Uv)
    } else if path.ends_with("pip") {
        Some(GlobalCache::Pip)
    } else if path.ends_with("pkgs") {
        Some(GlobalCache::Conda)
    } else {
        None
    }
}

fn action(
    label: impl Into<String>,
    description: impl Into<String>,
    method: ActionMethod,
    estimated_savings_bytes: u64,
) -> CleanAction {
    CleanAction {
        id: Uuid::new_v4(),
        label: label.into(),
        description: description.into(),
        method,
        risk: RiskLevel::Safe,
        estimated_savings_bytes,
    }
}

fn command(program: &str, args: &[&str]) -> ActionMethod {
    ActionMethod::Command {
        program: program.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        working_dir: None,
    }
}

impl PythonScanner {
    /// `.pytest_cache`, `.ruff_cache`, `.mypy_cache`, `.tox`, `.nox`,
    /// `.hypothesis` — rebuilt by the tool on its next run.
    async fn analyze_tool_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }
        let last_modified = staleness::most_recent_modification(path);
        let parent = path.parent();
        let dir_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        Ok(Some(CleanableItem {
            details: vec![Detail::new("Tool cache", dir_name.clone())],
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Python,
            kind: ArtifactKind::PythonToolCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale: last_modified.map(staleness::days_since),
            project_name: parent
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string()),
            project_root: parent.map(Path::to_path_buf),
            available_actions: vec![action(
                format!("Remove {dir_name}/"),
                format!("Delete {dir_name} (recreated on the tool's next run)"),
                ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                size_bytes,
            )],
        }))
    }

    /// uv's global cache: wheels, sources and interpreters shared by every
    /// uv-managed venv. AI-generated Python projects create a lot of these.
    async fn analyze_uv_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }
        let last_modified = staleness::most_recent_modification(path);

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Python,
            kind: ArtifactKind::UvCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale: last_modified.map(staleness::days_since),
            project_name: Some("uv cache".into()),
            project_root: None,
            available_actions: vec![
                // How much prune frees depends on which entries are still
                // referenced, which only uv knows — so no estimate.
                action(
                    "uv cache prune",
                    "Run `uv cache prune` to remove unused cache entries",
                    command("uv", &["cache", "prune"]),
                    0,
                ),
                action(
                    "uv cache clean",
                    "Run `uv cache clean` to clear the whole uv cache",
                    command("uv", &["cache", "clean"]),
                    size_bytes,
                ),
                action(
                    "Remove directory",
                    format!("Delete {}", path.display()),
                    ActionMethod::RemoveDir {
                        path: path.to_path_buf(),
                    },
                    size_bytes,
                ),
            ],
        }))
    }

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

        debug!(
            "Found __pycache__ at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
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

        debug!(
            "Found venv at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
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
                label: format!("Remove {venv_name}/"),
                description: "Delete the virtual environment (recreate with `python -m venv`)"
                    .into(),
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

        debug!(
            "Found pip cache at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
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

        debug!(
            "Found conda cache at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
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
            available_actions: vec![
                action(
                    "conda clean --all",
                    "Run `conda clean --all -y` to remove unused packages, tarballs and caches",
                    command("conda", &["clean", "--all", "-y"]),
                    // Packages still linked into environments are kept.
                    0,
                ),
                action(
                    "Remove conda packages cache",
                    "Delete cached conda packages (will re-download when needed)",
                    ActionMethod::RemoveDir {
                        path: path.to_path_buf(),
                    },
                    size_bytes,
                ),
            ],
        }))
    }
}

/// Whether `dir` contains any of `names`.
fn has_any(dir: &Path, names: &[&str]) -> bool {
    names.iter().any(|n| dir.join(n).exists())
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

    #[test]
    fn env_dir_is_a_venv_only_with_pyvenv_cfg() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().join("app/env");
        std::fs::create_dir_all(&env).unwrap();
        let scanner = PythonScanner;
        assert!(!scanner.is_candidate("env", &env));
        std::fs::write(env.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert!(scanner.is_candidate("env", &env));
    }

    #[test]
    fn dot_venv_with_pyvenv_cfg_needs_no_project_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join("scratch/.venv");
        std::fs::create_dir_all(&venv).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        assert!(PythonScanner.is_candidate(".venv", &venv));
    }

    #[test]
    fn unambiguous_tool_caches_are_candidates_anywhere() {
        let scanner = PythonScanner;
        for name in [".pytest_cache", ".ruff_cache", ".mypy_cache"] {
            assert!(scanner.is_candidate(name, &Path::new("/x").join(name)));
        }
    }

    #[test]
    fn tox_and_nox_require_their_config() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("p");
        std::fs::create_dir_all(project.join(".tox")).unwrap();
        std::fs::create_dir_all(project.join(".nox")).unwrap();
        let scanner = PythonScanner;
        assert!(!scanner.is_candidate(".tox", &project.join(".tox")));
        assert!(!scanner.is_candidate(".nox", &project.join(".nox")));
        assert!(!scanner.is_candidate(".hypothesis", &project.join(".hypothesis")));

        std::fs::write(project.join("tox.ini"), "[tox]").unwrap();
        std::fs::write(project.join("noxfile.py"), "").unwrap();
        assert!(scanner.is_candidate(".tox", &project.join(".tox")));
        assert!(scanner.is_candidate(".nox", &project.join(".nox")));
    }

    #[tokio::test]
    async fn analyze_tool_cache_is_safe_remove_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("proj/.ruff_cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("0.1"), vec![0u8; 300]).unwrap();

        let item = PythonScanner.analyze(&cache).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::PythonToolCache);
        assert_eq!(item.risk, RiskLevel::Safe);
        assert_eq!(item.project_name.as_deref(), Some("proj"));
        assert_eq!(item.available_actions.len(), 1);
        assert!(matches!(
            &item.available_actions[0].method,
            ActionMethod::RemoveDir { path } if path == &cache
        ));
        assert_eq!(item.available_actions[0].estimated_savings_bytes, 300);
    }

    #[tokio::test]
    async fn analyze_uv_cache_offers_prune_first() {
        let tmp = tempfile::tempdir().unwrap();
        let uv = tmp.path().join(".cache/uv");
        std::fs::create_dir_all(uv.join("wheels-v1")).unwrap();
        std::fs::write(uv.join("wheels-v1/x.whl"), vec![0u8; 500]).unwrap();

        let item = PythonScanner.analyze(&uv).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::UvCache);
        let actions = &item.available_actions;
        assert_eq!(actions.len(), 3);
        assert!(actions.iter().all(|a| a.risk == RiskLevel::Safe));
        let cmd = |i: usize| match &actions[i].method {
            ActionMethod::Command { program, args, .. } => format!("{program} {}", args.join(" ")),
            other => panic!("expected command, got {other:?}"),
        };
        assert_eq!(cmd(0), "uv cache prune");
        assert_eq!(cmd(1), "uv cache clean");
        assert!(matches!(&actions[2].method, ActionMethod::RemoveDir { path } if path == &uv));
    }

    #[tokio::test]
    async fn windows_uv_cache_path_is_classified_as_uv() {
        let tmp = tempfile::tempdir().unwrap();
        let uv = tmp.path().join("AppData/Local/uv/cache");
        std::fs::create_dir_all(&uv).unwrap();
        std::fs::write(uv.join("f"), "x").unwrap();
        let item = PythonScanner.analyze(&uv).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::UvCache);
    }

    #[tokio::test]
    async fn miniforge_pkgs_is_conda_with_clean_command() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path().join("miniforge3/pkgs");
        std::fs::create_dir_all(&pkgs).unwrap();
        std::fs::write(pkgs.join("numpy.conda"), vec![0u8; 100]).unwrap();

        let item = PythonScanner.analyze(&pkgs).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::CondaCache);
        match &item.available_actions[0].method {
            ActionMethod::Command { program, args, .. } => {
                assert_eq!(program, "conda");
                assert_eq!(args, &["clean", "--all", "-y"]);
            }
            other => panic!("expected conda clean first, got {other:?}"),
        }
        assert_eq!(item.available_actions[0].risk, RiskLevel::Safe);
    }

    #[test]
    fn global_classification_uses_components_not_substrings() {
        assert_eq!(
            classify_global(Path::new("/Users/pipeline/.cache/uv")),
            Some(GlobalCache::Uv)
        );
        assert_eq!(
            classify_global(Path::new("/Users/uv/Library/Caches/pip")),
            Some(GlobalCache::Pip)
        );
        assert_eq!(
            classify_global(Path::new("/Users/x/.conda/pkgs")),
            Some(GlobalCache::Conda)
        );
        assert_eq!(classify_global(Path::new("/Users/x/random")), None);
    }
}
