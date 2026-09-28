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

        // `global_locations` hands us `~/.cargo/registry` and `~/.cargo/git`.
        // Those are dependency caches, not a project's `target/`: treating
        // them as one labelled them "Remove target/" and offered `cargo
        // clean` inside `~/.cargo`, which does nothing useful.
        match cargo_home_kind(path) {
            Some(CargoHomeDir::Registry) => return analyze_registry(path).await,
            Some(CargoHomeDir::Git) => return analyze_git_checkouts(path).await,
            None => {}
        }

        // `parent()` is None only at a filesystem root, which cannot be a
        // build artifact — but this walks arbitrary user paths, so it returns
        // rather than panicking.
        let Some(parent) = path.parent() else {
            return Ok(None);
        };
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
            details: Vec::new(),
            agent: None,
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
        super::existing_home_dirs([".cargo/registry", ".cargo/git"])
    }
}

/// Which part of `CARGO_HOME` a global location is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CargoHomeDir {
    Registry,
    Git,
}

/// Classify `~/.cargo/registry` and `~/.cargo/git`; anything else is a
/// project `target/`.
fn cargo_home_kind(path: &Path) -> Option<CargoHomeDir> {
    let parent_is_cargo_home = path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == ".cargo");
    if !parent_is_cargo_home {
        return None;
    }
    match path.file_name()?.to_str()? {
        "registry" => Some(CargoHomeDir::Registry),
        "git" => Some(CargoHomeDir::Git),
        _ => None,
    }
}

fn action(
    label: impl Into<String>,
    description: impl Into<String>,
    method: ActionMethod,
    risk: RiskLevel,
    estimated_savings_bytes: u64,
) -> CleanAction {
    CleanAction {
        id: Uuid::new_v4(),
        label: label.into(),
        description: description.into(),
        method,
        risk,
        estimated_savings_bytes,
    }
}

/// Existing subdirectories of `root` from `names`, with their sizes.
async fn sized_subdirs(root: &Path, names: &[&str]) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    for name in names {
        let dir = root.join(name);
        if dir.is_dir() {
            let size = staleness::compute_dir_size(&dir).await;
            out.push((dir, size));
        }
    }
    out
}

/// `~/.cargo/registry`: downloaded `.crate` archives (`cache/`) and their
/// extracted sources (`src/`) are re-fetched on demand by the next build.
///
/// `index/` is kept: it is small relative to the rest, and rebuilding it is
/// the slow part of the first build after a clean.
async fn analyze_registry(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let removable = sized_subdirs(path, &["cache", "src"]).await;
    let size_bytes: u64 = removable.iter().map(|(_, s)| s).sum();
    if size_bytes == 0 {
        return Ok(None);
    }

    let mut details: Vec<Detail> = removable
        .iter()
        .map(|(dir, size)| {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            Detail::new(format!("registry/{name}"), ByteSize(*size).to_string())
        })
        .collect();
    let index = path.join("index");
    if index.is_dir() {
        let index_size = staleness::compute_dir_size(&index).await;
        details.push(Detail::new(
            "registry/index (kept)",
            ByteSize(index_size).to_string(),
        ));
    }

    let last_modified = staleness::most_recent_modification(path);
    let paths: Vec<PathBuf> = removable.into_iter().map(|(p, _)| p).collect();

    Ok(Some(CleanableItem {
        details,
        agent: None,
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::Rust,
        kind: ArtifactKind::CargoRegistry,
        risk: RiskLevel::Safe,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified,
        days_stale: last_modified.map(staleness::days_since),
        project_name: Some("Cargo registry".into()),
        project_root: None,
        available_actions: vec![action(
            "Remove downloaded crates",
            "Delete registry/cache and registry/src; Cargo re-downloads crates on the next build. The registry index is kept.",
            ActionMethod::RemoveDirs { paths },
            RiskLevel::Safe,
            size_bytes,
        )],
    }))
}

/// `~/.cargo/git`: `checkouts/` are working copies of git dependencies,
/// recreated from `db/` without a network. `db/` holds the bare clones
/// themselves, so removing it means re-fetching — hence Caution.
async fn analyze_git_checkouts(path: &Path) -> Result<Option<CleanableItem>, ScanError> {
    let parts = sized_subdirs(path, &["checkouts", "db"]).await;
    let size_bytes: u64 = parts.iter().map(|(_, s)| s).sum();
    if size_bytes == 0 {
        return Ok(None);
    }

    let mut actions = Vec::new();
    let mut details = Vec::new();
    for (dir, size) in &parts {
        let is_checkouts = dir.file_name().is_some_and(|n| n == "checkouts");
        if is_checkouts {
            details.push(Detail::new("git/checkouts", ByteSize(*size).to_string()));
            if *size > 0 {
                actions.push(action(
                    "Remove git checkouts",
                    "Delete git/checkouts; Cargo recreates them from its local clones.",
                    ActionMethod::RemoveDir { path: dir.clone() },
                    RiskLevel::Safe,
                    *size,
                ));
            }
        } else {
            details.push(Detail::new("git/db", ByteSize(*size).to_string()));
            if *size > 0 {
                actions.push(action(
                    "Remove git dependency clones",
                    "Delete git/db; git dependencies are re-fetched over the network on the next build.",
                    ActionMethod::RemoveDir { path: dir.clone() },
                    RiskLevel::Caution,
                    *size,
                ));
            }
        }
    }
    // Lowest risk first.
    actions.sort_by_key(|a| a.risk);

    let last_modified = staleness::most_recent_modification(path);
    Ok(Some(CleanableItem {
        details,
        agent: None,
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::Rust,
        kind: ArtifactKind::CargoGitCheckouts,
        risk: RiskLevel::Safe,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified,
        days_stale: last_modified.map(staleness::days_since),
        project_name: Some("Cargo git dependencies".into()),
        project_root: None,
        available_actions: actions,
    }))
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

    #[tokio::test]
    async fn cargo_registry_is_not_a_target_dir_and_keeps_the_index() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = tmp.path().join(".cargo/registry");
        for sub in ["cache/idx", "src/idx", "index/idx"] {
            std::fs::create_dir_all(registry.join(sub)).unwrap();
            std::fs::write(registry.join(sub).join("f"), vec![0u8; 100]).unwrap();
        }

        let item = RustScanner.analyze(&registry).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::CargoRegistry);
        assert_eq!(item.size_bytes, 200);
        assert_eq!(item.available_actions.len(), 1);
        let action = &item.available_actions[0];
        assert_eq!(action.risk, RiskLevel::Safe);
        assert_eq!(action.estimated_savings_bytes, 200);
        let ActionMethod::RemoveDirs { paths } = &action.method else {
            panic!("expected RemoveDirs, got {:?}", action.method);
        };
        assert_eq!(paths, &vec![registry.join("cache"), registry.join("src")]);
        assert!(!paths.iter().any(|p| p.starts_with(registry.join("index"))));
        assert!(!item
            .available_actions
            .iter()
            .any(|a| matches!(&a.method, ActionMethod::Command { .. })));
    }

    #[tokio::test]
    async fn cargo_git_offers_checkouts_safe_and_db_caution() {
        let tmp = tempfile::tempdir().unwrap();
        let git = tmp.path().join(".cargo/git");
        for sub in ["checkouts/dep-abc", "db/dep-abc"] {
            std::fs::create_dir_all(git.join(sub)).unwrap();
            std::fs::write(git.join(sub).join("f"), vec![0u8; 64]).unwrap();
        }

        let item = RustScanner.analyze(&git).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::CargoGitCheckouts);
        let shape: Vec<(RiskLevel, PathBuf)> = item
            .available_actions
            .iter()
            .map(|a| match &a.method {
                ActionMethod::RemoveDir { path } => (a.risk, path.clone()),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (RiskLevel::Safe, git.join("checkouts")),
                (RiskLevel::Caution, git.join("db")),
            ]
        );
    }

    #[tokio::test]
    async fn a_project_named_registry_is_still_a_target() {
        // Only `.cargo/registry` is special; a `target/` stays a target.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("registry");
        std::fs::create_dir_all(project.join("target")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(project.join("target/x"), "bin").unwrap();
        let item = RustScanner
            .analyze(&project.join("target"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, ArtifactKind::TargetDir);
    }
}
