use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;

pub struct DockerScanner;

/// Result of checking a Docker resource category.
struct DockerResourceInfo {
    _kind: ArtifactKind,
    label: &'static str,
    count: usize,
    prune_type: &'static str,
    extra_args: Vec<&'static str>,
    risk: RiskLevel,
}

#[async_trait]
impl EcosystemScanner for DockerScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Docker
    }

    fn is_candidate(&self, _file_name: &str, _path: &Path) -> bool {
        // Docker scanner is entirely global; it does not participate in filesystem walking.
        false
    }

    async fn analyze(&self, _path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        // Check if Docker is available
        let docker_available = tokio::task::spawn_blocking(|| {
            Command::new("docker")
                .arg("info")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);

        if !docker_available {
            debug!("Docker is not available, skipping Docker scanner");
            return Ok(None);
        }

        let resources = tokio::task::spawn_blocking(collect_docker_resources)
            .await
            .unwrap_or_default();

        if resources.is_empty() {
            return Ok(None);
        }

        let mut actions = Vec::new();

        for resource in &resources {
            if resource.count > 0 {
                let mut args: Vec<String> = vec![
                    resource.prune_type.to_string(),
                    "prune".to_string(),
                    "-f".to_string(),
                ];
                args.extend(resource.extra_args.iter().map(|a| a.to_string()));
                let extra_desc = if resource.extra_args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", resource.extra_args.join(" "))
                };

                actions.push(CleanAction {
                    id: Uuid::new_v4(),
                    label: format!("Prune {} ({} found)", resource.label, resource.count),
                    description: format!(
                        "Run `docker {} prune{}` to clean up {}",
                        resource.prune_type, extra_desc, resource.label
                    ),
                    method: ActionMethod::Command {
                        program: "docker".into(),
                        args,
                        working_dir: None,
                    },
                    risk: resource.risk,
                    estimated_savings_bytes: 0,
                });
            }
        }

        // Always offer a full system prune
        actions.push(CleanAction {
            id: Uuid::new_v4(),
            label: "docker system prune".into(),
            description: "Run `docker system prune` to remove all unused data".into(),
            method: ActionMethod::DockerPrune {
                prune_type: "system".into(),
            },
            risk: RiskLevel::Caution,
            estimated_savings_bytes: 0,
        });

        // Get disk usage estimate if possible
        let disk_usage = tokio::task::spawn_blocking(get_docker_disk_usage)
            .await
            .unwrap_or(0);

        Ok(Some(CleanableItem {
            id: Uuid::new_v4(),
            path: PathBuf::from("/var/run/docker.sock"),
            ecosystem: Ecosystem::Docker,
            kind: ArtifactKind::BuildCache,
            risk: RiskLevel::Caution,
            size_bytes: disk_usage,
            size_display: if disk_usage > 0 {
                ByteSize(disk_usage).to_string()
            } else {
                "unknown".into()
            },
            last_modified: None,
            days_stale: None,
            project_name: Some("Docker".into()),
            project_root: None,
            available_actions: actions,
        }))
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        // We use a sentinel path to trigger analyze() from the orchestrator's
        // global-locations phase. The path itself is not walked on disk.
        vec![PathBuf::from("/var/run/docker.sock")]
    }
}

/// Query Docker for cleanable resource counts.
fn collect_docker_resources() -> Vec<DockerResourceInfo> {
    let mut resources = Vec::new();

    // Dangling images
    if let Some(count) = docker_count("images", &["-f", "dangling=true", "-q"]) {
        if count > 0 {
            resources.push(DockerResourceInfo {
                _kind: ArtifactKind::DanglingImages,
                label: "dangling images",
                count,
                prune_type: "image",
                extra_args: vec![],
                risk: RiskLevel::Safe,
            });
        }
    }

    // Stopped containers
    if let Some(count) = docker_count("ps", &["-a", "-f", "status=exited", "-q"]) {
        if count > 0 {
            resources.push(DockerResourceInfo {
                _kind: ArtifactKind::StoppedContainers,
                label: "stopped containers",
                count,
                prune_type: "container",
                extra_args: vec![],
                risk: RiskLevel::Safe,
            });
        }
    }

    // Unused images (installed but not referenced by any container)
    if let Some(count) = count_unused_images() {
        if count > 0 {
            resources.push(DockerResourceInfo {
                _kind: ArtifactKind::UnusedImages,
                label: "unused images",
                count,
                prune_type: "image",
                extra_args: vec!["-a"],
                risk: RiskLevel::Caution,
            });
        }
    }

    // Dangling volumes
    if let Some(count) = docker_count("volume", &["ls", "-f", "dangling=true", "-q"]) {
        if count > 0 {
            resources.push(DockerResourceInfo {
                _kind: ArtifactKind::UnusedVolumes,
                label: "unused volumes",
                count,
                prune_type: "volume",
                extra_args: vec![],
                risk: RiskLevel::Caution,
            });
        }
    }

    resources
}

/// Run a docker command and count the output lines.
fn docker_count(subcommand: &str, args: &[&str]) -> Option<usize> {
    let output = Command::new("docker")
        .arg(subcommand)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        warn!(
            "docker {} failed: {}",
            subcommand,
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let count = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    Some(count)
}

/// Count images not used by any container (running or stopped).
fn count_unused_images() -> Option<usize> {
    // Get all image IDs
    let all_output = Command::new("docker")
        .args(["images", "-q"])
        .output()
        .ok()?;
    if !all_output.status.success() {
        return None;
    }
    let all_ids: std::collections::HashSet<String> = String::from_utf8_lossy(&all_output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();

    if all_ids.is_empty() {
        return Some(0);
    }

    // Get image IDs referenced by any container (running or stopped)
    let used_output = Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Image}}"])
        .output()
        .ok()?;
    if !used_output.status.success() {
        return None;
    }

    // Resolve image names to IDs by inspecting each used image
    let used_ids: std::collections::HashSet<String> = String::from_utf8_lossy(&used_output.stdout)
        .lines()
        .map(str::trim)
        .filter(|image_ref| !image_ref.is_empty())
        .filter_map(|image_ref| {
            let output = Command::new("docker")
                .args(["inspect", "--format", "{{.Id}}", image_ref])
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let full_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // The short ID from `docker images -q` is the first 12 characters
            // after "sha256:" — characters, not bytes. Lossy decoding of
            // unexpected output emits multi-byte U+FFFD, and a byte slice can
            // land mid-character and panic.
            Some(
                full_id
                    .strip_prefix("sha256:")
                    .unwrap_or(&full_id)
                    .chars()
                    .take(12)
                    .collect(),
            )
        })
        .collect();

    let unused = all_ids
        .iter()
        .filter(|id| !used_ids.contains(id.as_str()))
        .count();
    Some(unused)
}

/// Try to get Docker's total disk usage via `docker system df`.
fn get_docker_disk_usage() -> u64 {
    let output = Command::new("docker")
        .args(["system", "df", "--format", "{{.Size}}"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            // Docker outputs human-readable sizes; we just return 0 for now
            // since parsing all the different format variations is fragile.
            // The actual size will be determined during cleanup.
            0
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_always_false() {
        let scanner = DockerScanner;
        assert!(!scanner.is_candidate("docker", Path::new("/var/lib/docker")));
        assert!(!scanner.is_candidate("Dockerfile", Path::new("/app/Dockerfile")));
        assert!(!scanner.is_candidate("node_modules", Path::new("/app/node_modules")));
        assert!(!scanner.is_candidate("anything", Path::new("/anything")));
    }

    #[test]
    fn global_locations_returns_docker_sock() {
        let scanner = DockerScanner;
        let locs = scanner.global_locations();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0], PathBuf::from("/var/run/docker.sock"));
    }
}
