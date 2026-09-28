use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct JavaScanner;

#[async_trait]
impl EcosystemScanner for JavaScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Java
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        match file_name {
            "build" => is_gradle_project_dir(parent),
            ".gradle" => is_gradle_project_dir(parent),
            // Rust also builds into `target/`; a Cargo.toml beside it means
            // the Rust scanner owns it, even if a pom.xml is there too.
            "target" => is_maven_project_dir(parent),
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
            "build" | ".gradle" => self.analyze_gradle_build(path).await,
            "target" => match path.parent() {
                Some(parent) if is_maven_project_dir(parent) => {
                    self.analyze_maven_target(path, parent).await
                }
                _ => Ok(None),
            },
            _ => {
                if path.ends_with(".gradle/wrapper/dists") {
                    self.analyze_gradle_wrapper_dists(path).await
                } else if path_str.contains(".gradle/caches")
                    || path_str.ends_with(".gradle/caches")
                {
                    self.analyze_gradle_cache(path).await
                } else if path_str.contains(".m2/repository")
                    || path_str.ends_with(".m2/repository")
                {
                    self.analyze_maven_repo(path).await
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        super::existing_home_dirs([
            ".gradle/caches",        // Gradle dependency cache
            ".gradle/wrapper/dists", // Gradle distributions fetched by gradlew
            ".m2/repository",        // Maven local repository
        ])
    }
}

impl JavaScanner {
    /// A Maven module's `target/`: compiled classes, test reports and
    /// packaged jars, all rebuilt by `mvn package`.
    async fn analyze_maven_target(
        &self,
        path: &Path,
        parent: &Path,
    ) -> Result<Option<CleanableItem>, ScanError> {
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
            ecosystem: Ecosystem::Java,
            kind: ArtifactKind::MavenTarget,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale: last_modified.map(staleness::days_since),
            project_name: parent.file_name().map(|n| n.to_string_lossy().to_string()),
            project_root: Some(parent.to_path_buf()),
            available_actions: vec![
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "mvn clean".into(),
                    description: "Run `mvn clean` in the project directory".into(),
                    method: ActionMethod::Command {
                        program: "mvn".into(),
                        args: vec!["clean".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                },
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove target/".into(),
                    description: "Delete the Maven target directory".into(),
                    method: ActionMethod::RemoveDir {
                        path: path.to_path_buf(),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                },
            ],
        }))
    }

    /// `~/.gradle/wrapper/dists`: one full Gradle distribution per version
    /// any project's wrapper ever asked for. Caution like the other Gradle
    /// caches: removing it forces a download on the next offline build.
    async fn analyze_gradle_wrapper_dists(
        &self,
        path: &Path,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }
        let last_modified = staleness::most_recent_modification(path);
        let mut versions: Vec<String> = std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        versions.sort();
        let details = if versions.is_empty() {
            Vec::new()
        } else {
            vec![Detail::new("Distributions", versions.join(", "))]
        };

        Ok(Some(CleanableItem {
            details,
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Java,
            kind: ArtifactKind::GradleCache,
            risk: RiskLevel::Caution,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale: last_modified.map(staleness::days_since),
            project_name: Some("Gradle wrapper distributions".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove Gradle distributions".into(),
                description:
                    "Delete ~/.gradle/wrapper/dists (each wrapper re-downloads its Gradle version)"
                        .into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Caution,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_gradle_build(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
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
            "Found Gradle {} at {} ({})",
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

        // Offer gradle clean for build/ directories
        if dir_name == "build" {
            if let Some(parent) = parent {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "gradle clean".into(),
                    description: "Run `gradle clean` in the project directory".into(),
                    method: ActionMethod::Command {
                        program: "gradle".into(),
                        args: vec!["clean".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                });
            }
        }

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Java,
            kind: ArtifactKind::GradleBuildDir,
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

    async fn analyze_gradle_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        debug!(
            "Found Gradle caches at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Java,
            kind: ArtifactKind::GradleCache,
            risk: RiskLevel::Caution,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Gradle caches".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove Gradle caches".into(),
                description: "Delete ~/.gradle/caches (dependencies will re-download)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Caution,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_maven_repo(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        debug!(
            "Found Maven repository at {} ({})",
            path.display(),
            ByteSize(size_bytes)
        );

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Java,
            kind: ArtifactKind::MavenRepository,
            risk: RiskLevel::Caution,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Maven repository".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove Maven repository".into(),
                description: "Delete ~/.m2/repository (dependencies will re-download)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Caution,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }
}

/// A Maven module that is not also a Cargo crate.
fn is_maven_project_dir(dir: &Path) -> bool {
    dir.join("pom.xml").is_file() && !dir.join("Cargo.toml").exists()
}

/// Check whether a directory contains Gradle project markers.
fn is_gradle_project_dir(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if matches!(
                name_str.as_ref(),
                "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts"
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
    fn is_candidate_build_with_gradle() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("build")).unwrap();
        std::fs::write(project.join("build.gradle"), "plugins { id 'java' }").unwrap();

        let scanner = JavaScanner;
        assert!(scanner.is_candidate("build", &project.join("build")));
    }

    #[test]
    fn is_candidate_build_with_gradle_kts() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("build")).unwrap();
        std::fs::write(project.join("build.gradle.kts"), "plugins { java }").unwrap();

        let scanner = JavaScanner;
        assert!(scanner.is_candidate("build", &project.join("build")));
    }

    #[test]
    fn is_candidate_build_without_gradle() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("generic");
        std::fs::create_dir_all(project.join("build")).unwrap();

        let scanner = JavaScanner;
        assert!(!scanner.is_candidate("build", &project.join("build")));
    }

    #[test]
    fn is_candidate_dot_gradle_with_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join(".gradle")).unwrap();
        std::fs::write(project.join("build.gradle"), "plugins { id 'java' }").unwrap();

        let scanner = JavaScanner;
        assert!(scanner.is_candidate(".gradle", &project.join(".gradle")));
    }

    #[test]
    fn is_candidate_rejects_unrelated() {
        let scanner = JavaScanner;
        assert!(!scanner.is_candidate("target", Path::new("/some/target")));
        assert!(!scanner.is_candidate("node_modules", Path::new("/some/node_modules")));
    }

    #[test]
    fn maven_target_needs_pom_and_no_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("svc");
        std::fs::create_dir_all(project.join("target")).unwrap();
        let scanner = JavaScanner;
        assert!(!scanner.is_candidate("target", &project.join("target")));
        std::fs::write(project.join("pom.xml"), "<project/>").unwrap();
        assert!(scanner.is_candidate("target", &project.join("target")));
        std::fs::write(project.join("Cargo.toml"), "[package]").unwrap();
        assert!(!scanner.is_candidate("target", &project.join("target")));
    }

    #[tokio::test]
    async fn analyze_maven_target_offers_mvn_clean_and_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("svc");
        std::fs::create_dir_all(project.join("target/classes")).unwrap();
        std::fs::write(project.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(project.join("target/classes/A.class"), vec![0u8; 40]).unwrap();

        let item = JavaScanner
            .analyze(&project.join("target"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, ArtifactKind::MavenTarget);
        assert_eq!(item.risk, RiskLevel::Safe);
        assert_eq!(item.project_name.as_deref(), Some("svc"));
        match &item.available_actions[0].method {
            ActionMethod::Command {
                program,
                args,
                working_dir,
            } => {
                assert_eq!(program, "mvn");
                assert_eq!(args, &["clean"]);
                assert_eq!(working_dir.as_deref(), Some(project.as_path()));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            &item.available_actions[1].method,
            ActionMethod::RemoveDir { path } if path == &project.join("target")
        ));
    }

    #[tokio::test]
    async fn gradle_wrapper_dists_is_a_caution_gradle_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let dists = tmp.path().join(".gradle/wrapper/dists");
        std::fs::create_dir_all(dists.join("gradle-8.5-bin")).unwrap();
        std::fs::write(dists.join("gradle-8.5-bin/g.zip"), vec![0u8; 20]).unwrap();
        let item = JavaScanner.analyze(&dists).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::GradleCache);
        assert_eq!(item.risk, RiskLevel::Caution);
        assert_eq!(item.details[0].value, "gradle-8.5-bin");
    }

    #[tokio::test]
    async fn analyze_gradle_build_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("build/classes")).unwrap();
        std::fs::write(project.join("build.gradle"), "plugins { id 'java' }").unwrap();
        std::fs::write(project.join("build/classes/Main.class"), "fake class").unwrap();

        let scanner = JavaScanner;
        let result = scanner.analyze(&project.join("build")).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Java);
        assert_eq!(item.kind, ArtifactKind::GradleBuildDir);
        assert!(item.size_bytes > 0);
        assert!(item.available_actions.len() >= 2); // RemoveDir + gradle clean
    }

    #[tokio::test]
    async fn analyze_empty_dir_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("build");
        std::fs::create_dir_all(&empty).unwrap();

        let scanner = JavaScanner;
        let result = scanner.analyze(&empty).await.unwrap();
        assert!(result.is_none());
    }
}
