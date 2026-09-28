use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct GoScanner;

#[async_trait]
impl EcosystemScanner for GoScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Go
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        // Go scanner is entirely global-based, using `go env` to find cache locations.
        // There are no project-local directories to detect during filesystem walking.
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

        let path_str = path.to_string_lossy();

        // Determine which kind of Go cache this is
        let (kind, label, description, clean_args, risk) = if path_str.contains("mod") {
            (
                ArtifactKind::GoModCache,
                "go clean -modcache",
                "Run `go clean -modcache` to remove the module download cache",
                vec!["clean".to_string(), "-modcache".to_string()],
                RiskLevel::Caution,
            )
        } else if path_str.contains("fuzz") || path_str.contains("test") {
            (
                ArtifactKind::GoTestCache,
                "go clean -testcache",
                "Run `go clean -testcache` to remove cached test results",
                vec!["clean".to_string(), "-testcache".to_string()],
                RiskLevel::Safe,
            )
        } else {
            (
                ArtifactKind::GoBuildCache,
                "go clean -cache",
                "Run `go clean -cache` to remove the build cache",
                vec!["clean".to_string(), "-cache".to_string()],
                RiskLevel::Safe,
            )
        };

        debug!(
            "Found Go {} at {} ({})",
            label,
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Go,
            kind,
            risk,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Go".into()),
            project_root: None,
            available_actions: vec![
                CleanAction {
                    id: Uuid::new_v4(),
                    label: label.into(),
                    description: description.into(),
                    method: ActionMethod::Command {
                        program: "go".into(),
                        args: clean_args,
                        working_dir: None,
                    },
                    risk,
                    estimated_savings_bytes: size_bytes,
                },
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove directory".into(),
                    description: format!("Delete {}", path.display()),
                    method: ActionMethod::RemoveDir {
                        path: path.to_path_buf(),
                    },
                    risk,
                    estimated_savings_bytes: size_bytes,
                },
            ],
        }))
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];

        // Use `go env` to discover cache locations
        if let Some(mod_cache) = go_env("GOMODCACHE") {
            let p = PathBuf::from(&mod_cache);
            if p.is_dir() {
                locs.push(p);
            }
        }

        if let Some(go_cache) = go_env("GOCACHE") {
            let p = PathBuf::from(&go_cache);
            if p.is_dir() {
                locs.push(p);
            }
        }

        // If `go env` is not available, try well-known default locations
        if locs.is_empty() {
            if let Some(home) = crate::paths::home_dir() {
                let mod_cache = home.join("go/pkg/mod");
                if mod_cache.is_dir() {
                    locs.push(mod_cache);
                }

                // Build cache default locations
                #[cfg(target_os = "macos")]
                {
                    let cache = home.join("Library/Caches/go-build");
                    if cache.is_dir() {
                        locs.push(cache);
                    }
                }

                #[cfg(target_os = "linux")]
                {
                    let cache = home.join(".cache/go-build");
                    if cache.is_dir() {
                        locs.push(cache);
                    }
                }
            }
        }

        locs
    }
}

/// Query a `go env` variable.
fn go_env(var: &str) -> Option<String> {
    let output = Command::new("go").args(["env", var]).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_always_false() {
        let scanner = GoScanner;
        assert!(!scanner.is_candidate("go", Path::new("/usr/local/go")));
        assert!(!scanner.is_candidate("go.mod", Path::new("/app/go.mod")));
        assert!(!scanner.is_candidate("pkg", Path::new("/home/user/go/pkg")));
        assert!(!scanner.is_candidate("anything", Path::new("/anything")));
    }

    #[tokio::test]
    async fn analyze_go_build_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("go-build");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("some-cache-file"), "cached build artifact").unwrap();

        let scanner = GoScanner;
        let result = scanner.analyze(&cache).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Go);
        assert_eq!(item.kind, ArtifactKind::GoBuildCache);
        assert!(item.size_bytes > 0);
        assert!(item.available_actions.len() >= 2);
    }

    #[tokio::test]
    async fn analyze_go_mod_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let mod_cache = tmp.path().join("mod");
        std::fs::create_dir_all(mod_cache.join("github.com/user/repo")).unwrap();
        std::fs::write(
            mod_cache.join("github.com/user/repo/go.mod"),
            "module github.com/user/repo",
        )
        .unwrap();

        let scanner = GoScanner;
        let result = scanner.analyze(&mod_cache).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Go);
        assert_eq!(item.kind, ArtifactKind::GoModCache);
        assert!(item.size_bytes > 0);
    }

    #[tokio::test]
    async fn analyze_empty_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty-cache");
        std::fs::create_dir_all(&empty).unwrap();

        let scanner = GoScanner;
        let result = scanner.analyze(&empty).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn analyze_nonexistent_returns_none() {
        let scanner = GoScanner;
        let result = scanner
            .analyze(Path::new("/nonexistent/path"))
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
