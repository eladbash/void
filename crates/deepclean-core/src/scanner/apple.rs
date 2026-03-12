use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct AppleScanner;

#[async_trait]
impl EcosystemScanner for AppleScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Apple
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        match file_name {
            "DerivedData" => true,
            "Build" => {
                // Only if the parent path suggests an Xcode project
                let Some(parent) = path.parent() else {
                    return false;
                };
                is_xcode_project_dir(parent)
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

        match file_name.as_str() {
            "DerivedData" => self.analyze_derived_data(path).await,
            "Build" => self.analyze_build_dir(path).await,
            "Archives" => self.analyze_archives(path).await,
            _ => {
                // Could be a DeviceSupport directory or other global location
                let path_str = path.to_string_lossy();
                if path_str.contains("CoreSimulator/Devices") {
                    self.analyze_simulator_devices(path).await
                } else if path_str.contains("CoreSimulator/Caches") {
                    self.analyze_simulator_caches(path).await
                } else if path_str.contains("CocoaPods") {
                    self.analyze_cocoapods_cache(path).await
                } else if path_str.contains("org.swift.swiftpm") {
                    self.analyze_spm_cache(path).await
                } else if path_str.contains("DeviceSupport") {
                    self.analyze_device_support(path).await
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let mut locs = vec![];
        if let Some(home) = dirs::home_dir() {
            // Xcode DerivedData
            let derived_data = home.join("Library/Developer/Xcode/DerivedData");
            if derived_data.is_dir() {
                locs.push(derived_data);
            }

            // Xcode Archives
            let archives = home.join("Library/Developer/Xcode/Archives");
            if archives.is_dir() {
                locs.push(archives);
            }

            // iOS DeviceSupport
            let device_support = home.join("Library/Developer/Xcode/iOS DeviceSupport");
            if device_support.is_dir() {
                locs.push(device_support);
            }

            // watchOS DeviceSupport
            let watch_support = home.join("Library/Developer/Xcode/watchOS DeviceSupport");
            if watch_support.is_dir() {
                locs.push(watch_support);
            }

            // tvOS DeviceSupport
            let tvos_support = home.join("Library/Developer/Xcode/tvOS DeviceSupport");
            if tvos_support.is_dir() {
                locs.push(tvos_support);
            }

            // iOS Simulator Devices
            let sim_devices = home.join("Library/Developer/CoreSimulator/Devices");
            if sim_devices.is_dir() {
                locs.push(sim_devices);
            }

            // iOS Simulator Caches
            let sim_caches = home.join("Library/Developer/CoreSimulator/Caches");
            if sim_caches.is_dir() {
                locs.push(sim_caches);
            }

            // CocoaPods cache
            let cocoapods = home.join("Library/Caches/CocoaPods");
            if cocoapods.is_dir() {
                locs.push(cocoapods);
            }

            // Swift Package Manager global cache
            let spm_cache = home.join("Library/org.swift.swiftpm");
            if spm_cache.is_dir() {
                locs.push(spm_cache);
            }
        }
        locs
    }
}

impl AppleScanner {
    async fn analyze_derived_data(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
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
            "Found DerivedData at {} ({} bytes)",
            path.display(),
            size_bytes
        );

        let mut actions = vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove DerivedData/".into(),
            description: "Delete the entire DerivedData directory".into(),
            method: ActionMethod::RemoveDir {
                path: path.to_path_buf(),
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        }];

        // If this is the global DerivedData, offer per-project cleanup via subdirs
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    let subdir = entry.path();
                    let subdir_name = entry.file_name().to_string_lossy().to_string();
                    // Skip non-project entries like ModuleCache
                    if subdir_name == "ModuleCache" || subdir_name.starts_with('.') {
                        continue;
                    }
                    actions.push(CleanAction {
                        id: Uuid::new_v4(),
                        label: format!("Remove DerivedData/{subdir_name}"),
                        description: format!("Delete DerivedData for project '{subdir_name}'"),
                        method: ActionMethod::RemoveDir { path: subdir },
                        risk: RiskLevel::Safe,
                        estimated_savings_bytes: 0, // individual sizes not computed here
                    });
                }
            }
        }

        // If we have a parent that looks like a project, offer xcodebuild clean
        if let Some(parent) = parent {
            if is_xcode_project_dir(parent) {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "xcodebuild clean".into(),
                    description: "Run `xcodebuild clean` in the project directory".into(),
                    method: ActionMethod::Command {
                        program: "xcodebuild".into(),
                        args: vec!["clean".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                });
            }
        }

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::DerivedData,
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

    async fn analyze_build_dir(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
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

        let mut actions = vec![CleanAction {
            id: Uuid::new_v4(),
            label: "Remove Build/".into(),
            description: "Delete the Build directory".into(),
            method: ActionMethod::RemoveDir {
                path: path.to_path_buf(),
            },
            risk: RiskLevel::Safe,
            estimated_savings_bytes: size_bytes,
        }];

        if let Some(parent) = parent {
            if is_xcode_project_dir(parent) {
                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: "xcodebuild clean".into(),
                    description: "Run `xcodebuild clean` in the project directory".into(),
                    method: ActionMethod::Command {
                        program: "xcodebuild".into(),
                        args: vec!["clean".into()],
                        working_dir: Some(parent.to_path_buf()),
                    },
                    risk: RiskLevel::Safe,
                    estimated_savings_bytes: size_bytes,
                });
            }
        }

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::DerivedData,
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

    async fn analyze_archives(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::Archives,
            risk: RiskLevel::Caution,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: None,
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove Archives/".into(),
                description: "Delete all Xcode archives (old builds for App Store submission)"
                    .into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Caution,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_simulator_devices(
        &self,
        path: &Path,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::SimulatorDevices,
            risk: RiskLevel::Caution,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("iOS Simulators".into()),
            project_root: None,
            available_actions: vec![
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Delete unavailable simulators".into(),
                    description: "Run `xcrun simctl delete unavailable` to remove simulators for old runtimes".into(),
                    method: ActionMethod::Command {
                        program: "xcrun".into(),
                        args: vec!["simctl".into(), "delete".into(), "unavailable".into()],
                        working_dir: None,
                    },
                    risk: RiskLevel::Caution,
                    estimated_savings_bytes: size_bytes,
                },
                CleanAction {
                    id: Uuid::new_v4(),
                    label: "Remove all simulator devices".into(),
                    description: "Delete all simulator device data (will be recreated as needed)".into(),
                    method: ActionMethod::RemoveDir {
                        path: path.to_path_buf(),
                    },
                    risk: RiskLevel::Danger,
                    estimated_savings_bytes: size_bytes,
                },
            ],
        }))
    }

    async fn analyze_simulator_caches(
        &self,
        path: &Path,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::SimulatorCaches,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Simulator Caches".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove simulator caches".into(),
                description: "Delete CoreSimulator caches".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_cocoapods_cache(
        &self,
        path: &Path,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::CocoaPodsCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("CocoaPods".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "pod cache clean --all".into(),
                description: "Run `pod cache clean --all` to clear CocoaPods cache".into(),
                method: ActionMethod::Command {
                    program: "pod".into(),
                    args: vec!["cache".into(), "clean".into(), "--all".into()],
                    working_dir: None,
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_spm_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::SwiftPackageCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some("Swift Package Manager".into()),
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove SPM cache".into(),
                description: "Delete Swift Package Manager global cache".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Safe,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }

    async fn analyze_device_support(
        &self,
        path: &Path,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Apple,
            kind: ArtifactKind::DeviceSupport,
            risk: RiskLevel::Caution,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: None,
            project_root: None,
            available_actions: vec![CleanAction {
                id: Uuid::new_v4(),
                label: "Remove DeviceSupport files".into(),
                description:
                    "Delete device support files (will re-download when device is connected)".into(),
                method: ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                risk: RiskLevel::Caution,
                estimated_savings_bytes: size_bytes,
            }],
        }))
    }
}

/// Check whether a directory contains an Xcode project or workspace.
fn is_xcode_project_dir(dir: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".xcodeproj")
                || name_str.ends_with(".xcworkspace")
                || name_str == "Package.swift"
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
    fn is_candidate_derived_data() {
        let scanner = AppleScanner;
        let path = Path::new("/Users/dev/Library/Developer/Xcode/DerivedData");
        assert!(scanner.is_candidate("DerivedData", path));
    }

    #[test]
    fn is_candidate_build_with_xcode_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("MyApp");
        std::fs::create_dir_all(project.join("MyApp.xcodeproj")).unwrap();
        std::fs::create_dir_all(project.join("Build")).unwrap();

        let scanner = AppleScanner;
        assert!(scanner.is_candidate("Build", &project.join("Build")));
    }

    #[test]
    fn is_candidate_build_without_xcode_project() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("generic");
        std::fs::create_dir_all(project.join("Build")).unwrap();

        let scanner = AppleScanner;
        assert!(!scanner.is_candidate("Build", &project.join("Build")));
    }

    #[test]
    fn is_candidate_rejects_unrelated() {
        let scanner = AppleScanner;
        assert!(!scanner.is_candidate("target", Path::new("/some/target")));
        assert!(!scanner.is_candidate("node_modules", Path::new("/some/node_modules")));
    }

    #[test]
    fn detects_xcworkspace() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("MyApp.xcworkspace")).unwrap();
        assert!(is_xcode_project_dir(tmp.path()));
    }

    #[test]
    fn detects_package_swift() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Package.swift"),
            "// swift-tools-version:5.5",
        )
        .unwrap();
        assert!(is_xcode_project_dir(tmp.path()));
    }

    #[test]
    fn no_xcode_markers() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.c"), "int main() {}").unwrap();
        assert!(!is_xcode_project_dir(tmp.path()));
    }

    #[tokio::test]
    async fn analyze_derived_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = tmp.path().join("DerivedData");
        std::fs::create_dir_all(dd.join("MyApp-abc123")).unwrap();
        std::fs::write(dd.join("MyApp-abc123/some_artifact"), "build output").unwrap();

        let scanner = AppleScanner;
        let result = scanner.analyze(&dd).await.unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Apple);
        assert_eq!(item.kind, ArtifactKind::DerivedData);
        assert!(item.size_bytes > 0);
        assert!(!item.available_actions.is_empty());
    }
}
