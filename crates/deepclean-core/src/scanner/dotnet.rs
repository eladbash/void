use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct DotNetScanner;

#[async_trait]
impl EcosystemScanner for DotNetScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::DotNet
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        match file_name {
            "bin" | "obj" => {
                let Some(parent) = path.parent() else {
                    return false;
                };
                is_dotnet_project_dir(parent)
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
            "bin" | "obj" => self.analyze_bin_obj(path).await,
            _ => {
                if path_str.contains(".nuget/packages") || path_str.ends_with(".nuget/packages") {
                    self.analyze_nuget_cache(path).await
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];
        if let Some(home) = dirs::home_dir() {
            let nuget = home.join(".nuget/packages");
            if nuget.is_dir() {
                locs.push(nuget);
            }
        }
        locs
    }
}

impl DotNetScanner {
    async fn analyze_bin_obj(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
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

        let dir_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        debug!(
            "Found .NET {} at {} ({})",
            dir_name,
            path.display(),
            ByteSize(size_bytes)
        );

        let mut actions = vec![CleanAction {
            id: Uuid::new_v4(),
            label: format!("Remove {dir_name}/"),
            description: format!("Delete the {dir_name} directory"),
            method: ActionMethod::RemoveDir {
                path: path.to_path_buf(),
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        }];

        if let Some(parent) = parent {
            actions.push(CleanAction {
                id: Uuid::new_v4(),
                label: "dotnet clean".into(),
                description: "Run `dotnet clean` in the project directory".into(),
                method: ActionMethod::Command {
                    program: "dotnet".into(),
                    args: vec!["clean".into()],
                    working_dir: Some(parent.to_path_buf()),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            });
        }

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::DotNet,
            kind: ArtifactKind::DotNetBinObj,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name,
            project_root: parent.map(|p| p.to_path_buf()),
            available_actions: actions,
        }))
    }

    async fn analyze_nuget_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        debug!(
            "Found NuGet cache at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::DotNet,
            kind: ArtifactKind::NuGetCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("NuGet packages".into()),
            project_root: None,
            available_actions: vec![
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "dotnet nuget locals all --clear".into(),
                    description: "Run `dotnet nuget locals all --clear` to clear NuGet caches"
                        .into(),
                    method: ActionMethod::Command {
                        program: "dotnet".into(),
                        args: vec![
                            "nuget".into(),
                            "locals".into(),
                            "all".into(),
                            "--clear".into(),
                        ],
                        working_dir: None,
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                },
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove NuGet packages".into(),
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
}

/// Check whether a directory contains .NET project markers.
fn is_dotnet_project_dir(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".csproj")
                || name_str.ends_with(".fsproj")
                || name_str.ends_with(".sln")
            {
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
    fn is_candidate_bin_with_csproj() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("MyApp");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        std::fs::write(project.join("MyApp.csproj"), "<Project />").unwrap();

        let scanner = DotNetScanner;
        assert!(scanner.is_candidate("bin", &project.join("bin")));
    }

    #[test]
    fn is_candidate_obj_with_fsproj() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("MyApp");
        std::fs::create_dir_all(project.join("obj")).unwrap();
        std::fs::write(project.join("MyApp.fsproj"), "<Project />").unwrap();

        let scanner = DotNetScanner;
        assert!(scanner.is_candidate("obj", &project.join("obj")));
    }

    #[test]
    fn is_candidate_bin_with_sln() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("MySolution");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        std::fs::write(
            project.join("MySolution.sln"),
            "Microsoft Visual Studio Solution",
        )
        .unwrap();

        let scanner = DotNetScanner;
        assert!(scanner.is_candidate("bin", &project.join("bin")));
    }

    #[test]
    fn is_candidate_bin_without_dotnet() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("generic");
        std::fs::create_dir_all(project.join("bin")).unwrap();

        let scanner = DotNetScanner;
        assert!(!scanner.is_candidate("bin", &project.join("bin")));
    }

    #[test]
    fn is_candidate_rejects_unrelated() {
        let scanner = DotNetScanner;
        assert!(!scanner.is_candidate("target", Path::new("/some/target")));
        assert!(!scanner.is_candidate("node_modules", Path::new("/some/node_modules")));
    }

    #[tokio::test]
    async fn analyze_bin_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("MyApp");
        std::fs::create_dir_all(project.join("bin/Debug/net8.0")).unwrap();
        std::fs::write(project.join("MyApp.csproj"), "<Project />").unwrap();
        std::fs::write(project.join("bin/Debug/net8.0/MyApp.dll"), "fake dll").unwrap();

        let scanner = DotNetScanner;
        let result = scanner.analyze(&project.join("bin")).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::DotNet);
        assert_eq!(item.kind, ArtifactKind::DotNetBinObj);
        assert!(item.size_bytes > 0);
        assert!(item.available_actions.len() >= 2); // RemoveDir + dotnet clean
    }

    #[tokio::test]
    async fn analyze_empty_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("bin");
        std::fs::create_dir_all(&empty).unwrap();

        let scanner = DotNetScanner;
        let result = scanner.analyze(&empty).await.unwrap();
        assert!(result.is_none());
    }
}
