use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct DockerScanner;

/// Sentinel global location that triggers the Docker CLI analysis. The path
/// itself is never walked.
const DOCKER_SOCK_SENTINEL: &str = "/var/run/docker.sock";

/// Container runtimes' VM disk images, relative to home. Files for the
/// single-image runtimes, a directory for podman (one image per machine).
const VM_DISKS: [&str; 5] = [
    "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
    ".orbstack/data/data.img",
    ".orbstack/data/orbstack.img",
    ".colima/_lima/colima/diffdisk",
    ".local/share/containers/podman/machine",
];

/// Result of checking a Docker resource category.
struct DockerResourceInfo {
    _kind: ArtifactKind,
    label: &'static str,
    count: usize,
    prune_type: &'static str,
    extra_args: Vec<&'static str>,
    risk: RiskLevel,
    /// Which `docker system df` row's Reclaimable honestly estimates this
    /// prune. `None` when no row does (dangling images are a subset of the
    /// Images row, whose reclaimable figure would overstate them).
    estimate_kind: Option<DfKind>,
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

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if path != Path::new(DOCKER_SOCK_SENTINEL) {
            return Ok(analyze_vm_disk(path).await);
        }

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

        let df = tokio::task::spawn_blocking(docker_system_df)
            .await
            .unwrap_or_default();
        let reclaimable = |kind: DfKind| {
            df.iter()
                .filter(|r| r.kind == kind)
                .map(|r| r.reclaimable)
                .sum::<u64>()
        };

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
                    estimated_savings_bytes: resource.estimate_kind.map(reclaimable).unwrap_or(0),
                });
            }
        }

        // Build cache is usually the biggest line in `docker system df` for
        // anyone iterating on Dockerfiles — and agents rebuild images a lot.
        let build_cache = reclaimable(DfKind::BuildCache);
        if build_cache > 0 {
            actions.push(CleanAction {
                id: Uuid::new_v4(),
                label: "Prune build cache".into(),
                description: "Run `docker builder prune -f` to remove dangling build cache".into(),
                method: ActionMethod::DockerPrune {
                    prune_type: "builder".into(),
                },
                risk: RiskLevel::Safe,
                // Upper bound: `builder prune` without `-a` keeps cache still
                // referenced by tagged images.
                estimated_savings_bytes: build_cache,
            });
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
            // `system prune` always removes stopped containers; what else it
            // frees (dangling images, dangling build cache) is not broken
            // out by `df`, so only the certain part is claimed.
            estimated_savings_bytes: reclaimable(DfKind::Containers),
        });

        if resources.is_empty() && build_cache == 0 {
            return Ok(None);
        }

        // `docker system df` rows overlap: with the containerd image store,
        // build cache and images share layers, so their reclaimable figures
        // can sum past the size of the disk that holds them. Everything
        // Docker Desktop / OrbStack stores lives inside one VM image, so its
        // allocated size is a hard ceiling on what pruning can free.
        let summed: u64 = df.iter().map(|r| r.reclaimable).sum();
        let ceiling = tokio::task::spawn_blocking(docker_vm_allocated)
            .await
            .ok()
            .flatten();
        let disk_usage = cap(summed, ceiling);
        for action in &mut actions {
            action.estimated_savings_bytes = cap(action.estimated_savings_bytes, ceiling);
        }
        let mut details: Vec<Detail> = df
            .iter()
            .map(|r| {
                Detail::new(
                    r.label.clone(),
                    format!(
                        "{} total, {} reclaimable",
                        ByteSize(r.size),
                        ByteSize(r.reclaimable)
                    ),
                )
            })
            .collect();
        if let Some(limit) = ceiling.filter(|limit| summed > *limit) {
            details.push(Detail::new(
                "Why the total is lower",
                format!(
                    "Docker reports {} reclaimable, but its categories overlap (images and build cache share layers). Everything lives inside a {} disk image, so no prune can free more than that.",
                    ByteSize(summed),
                    ByteSize(limit)
                ),
            ));
        }

        Ok(Some(CleanableItem {
            details,
            agent: None,
            id: Uuid::new_v4(),
            path: PathBuf::from(DOCKER_SOCK_SENTINEL),
            ecosystem: Ecosystem::Docker,
            kind: ArtifactKind::DockerData,
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
        let mut locs = vec![PathBuf::from(DOCKER_SOCK_SENTINEL)];
        if let Some(home) = crate::paths::home_dir() {
            locs.extend(
                VM_DISKS
                    .iter()
                    .map(|rel| home.join(rel))
                    .filter(|p| p.exists()),
            );
        }
        locs
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
                estimate_kind: None,
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
                estimate_kind: Some(DfKind::Containers),
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
                estimate_kind: Some(DfKind::Images),
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
                estimate_kind: Some(DfKind::Volumes),
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

/// Resource category of a `docker system df` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DfKind {
    Images,
    Containers,
    Volumes,
    BuildCache,
    Other,
}

/// One row of `docker system df --format '{{json .}}'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfRow {
    pub kind: DfKind,
    /// Docker's own label ("Images", "Local Volumes", "Build Cache").
    pub label: String,
    pub size: u64,
    pub reclaimable: u64,
}

/// Parse `docker system df --format '{{json .}}'`: one JSON object per line,
/// e.g. `{"Type":"Images","Size":"2.3GB","Reclaimable":"1.1GB (45%)",...}`.
/// Lines that are not such objects are skipped, not fatal — older Docker
/// versions and plugins print warnings on stdout.
pub fn parse_system_df(output: &str) -> Vec<DfRow> {
    output
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let label = value.get("Type")?.as_str()?.to_string();
            let field = |name: &str| {
                value
                    .get(name)
                    .and_then(|v| v.as_str())
                    .and_then(parse_docker_size)
                    .unwrap_or(0)
            };
            let kind = match label.as_str() {
                "Images" => DfKind::Images,
                "Containers" => DfKind::Containers,
                "Local Volumes" => DfKind::Volumes,
                "Build Cache" => DfKind::BuildCache,
                _ => DfKind::Other,
            };
            Some(DfRow {
                kind,
                size: field("Size"),
                reclaimable: field("Reclaimable"),
                label,
            })
        })
        .collect()
}

/// Parse a Docker human-readable size: `2.3GB`, `512.5kB`, `0B`, `1.2GiB`,
/// and the `"1.1GB (45%)"` form of the Reclaimable column (the percentage is
/// ignored). Docker prints decimal units (1 kB = 1000 B); binary suffixes
/// are accepted too.
pub fn parse_docker_size(text: &str) -> Option<u64> {
    let text = text.split_whitespace().next()?;
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let multiplier: f64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * multiplier).round() as u64)
}

/// Run `docker system df` and parse it; empty when Docker is unavailable.
fn docker_system_df() -> Vec<DfRow> {
    let output = Command::new("docker")
        .args(["system", "df", "--format", "{{json .}}"])
        .output();
    match output {
        Ok(out) if out.status.success() => parse_system_df(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// `value`, never more than `ceiling` when one is known.
fn cap(value: u64, ceiling: Option<u64>) -> u64 {
    ceiling.map_or(value, |limit| value.min(limit))
}

/// Allocated bytes of the Docker Desktop or OrbStack VM image, when present.
/// Colima and podman are separate engines the `docker` CLI may not talk to,
/// so they do not bound its figures.
fn docker_vm_allocated() -> Option<u64> {
    let home = crate::paths::home_dir()?;
    VM_DISKS
        .iter()
        .map(|rel| home.join(rel))
        .filter(|p| {
            matches!(
                vm_engine(p),
                Some(VmEngine::DockerDesktop | VmEngine::OrbStack)
            )
        })
        .filter_map(|p| std::fs::symlink_metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| staleness::on_disk_len(&m))
        .max()
}

/// Which runtime a VM disk path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmEngine {
    DockerDesktop,
    OrbStack,
    Colima,
    Podman,
}

fn vm_engine(path: &Path) -> Option<VmEngine> {
    let s = path.to_string_lossy();
    if s.contains("com.docker.docker") {
        Some(VmEngine::DockerDesktop)
    } else if s.contains(".orbstack") {
        Some(VmEngine::OrbStack)
    } else if s.contains(".colima") {
        Some(VmEngine::Colima)
    } else if s.contains("podman") {
        Some(VmEngine::Podman)
    } else {
        None
    }
}

/// A container runtime's VM disk image, shown so its size is visible.
///
/// Never offered for deletion: the image *is* every container, image and
/// volume the runtime holds. The disk only shrinks when data inside it is
/// pruned (and the runtime trims the image), so the one action offered is a
/// prune through the runtime's own CLI.
async fn analyze_vm_disk(path: &Path) -> Option<CleanableItem> {
    let engine = vm_engine(path)?;
    let (allocated, apparent) = if path.is_dir() {
        let allocated = staleness::compute_dir_size(path).await;
        (allocated, allocated)
    } else {
        let meta = tokio::fs::symlink_metadata(path).await.ok()?;
        if !meta.is_file() {
            return None;
        }
        (staleness::on_disk_len(&meta), meta.len())
    };
    if allocated == 0 {
        return None;
    }
    let last_modified = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(chrono::DateTime::<chrono::Utc>::from);

    // Informational only — no actions. The image *contains* the images,
    // volumes and build cache the Docker item already lists; offering a prune
    // here too would count the same bytes twice in "reclaimable" (a 220 GB
    // Docker.raw on top of the 160 GB of build cache inside it).
    let (name, advice) = match engine {
        VmEngine::DockerDesktop => (
            "Docker Desktop disk image",
            "Holds the Docker data listed under the Docker item — clean that first. Then Docker Desktop → Troubleshoot → Clean / Purge data shrinks the image. Do not delete Docker.raw by hand.",
        ),
        VmEngine::OrbStack => (
            "OrbStack disk image",
            "Holds the Docker data listed under the Docker item. OrbStack shrinks this image automatically once data inside it is deleted.",
        ),
        VmEngine::Colima => (
            "Colima disk image",
            "Prune unused data inside the VM (`docker system prune`), or recreate the VM with `colima delete` if you no longer need anything in it.",
        ),
        VmEngine::Podman => (
            "Podman machine images",
            "Prune unused data with `podman system prune`; remove whole machines with `podman machine rm`.",
        ),
    };
    let actions: Vec<CleanAction> = Vec::new();

    let mut details = vec![Detail::new(
        "Allocated on disk",
        ByteSize(allocated).to_string(),
    )];
    if apparent != allocated {
        details.push(Detail::new(
            "Maximum size",
            format!("{} (sparse)", ByteSize(apparent)),
        ));
    }
    details.push(Detail::new("How to reclaim", advice));

    Some(CleanableItem {
        details,
        agent: None,
        id: Uuid::new_v4(),
        path: path.to_path_buf(),
        ecosystem: Ecosystem::Docker,
        kind: ArtifactKind::ContainerVmDisk,
        risk: RiskLevel::Caution,
        size_bytes: allocated,
        size_display: ByteSize(allocated).to_string(),
        last_modified,
        days_stale: last_modified.map(staleness::days_since),
        project_name: Some(name.into()),
        project_root: None,
        available_actions: actions,
    })
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
    fn global_locations_starts_with_docker_sock() {
        let scanner = DockerScanner;
        let locs = scanner.global_locations();
        assert_eq!(locs[0], PathBuf::from("/var/run/docker.sock"));
        // Anything else is a VM disk image.
        assert!(locs[1..].iter().all(|p| vm_engine(p).is_some()));
    }

    #[test]
    fn parse_docker_sizes() {
        assert_eq!(parse_docker_size("0B"), Some(0));
        assert_eq!(parse_docker_size("2.3GB"), Some(2_300_000_000));
        assert_eq!(parse_docker_size("512.5kB"), Some(512_500));
        assert_eq!(parse_docker_size("10MB"), Some(10_000_000));
        assert_eq!(parse_docker_size("1GiB"), Some(1 << 30));
        assert_eq!(parse_docker_size("1.1GB (45%)"), Some(1_100_000_000));
        assert_eq!(parse_docker_size("1.2TB"), Some(1_200_000_000_000));
        assert_eq!(parse_docker_size(""), None);
        assert_eq!(parse_docker_size("lots"), None);
        assert_eq!(parse_docker_size("3XB"), None);
    }

    #[test]
    fn parse_system_df_json_lines() {
        let out = r#"{"Active":"3","Reclaimable":"1.1GB (45%)","Size":"2.3GB","TotalCount":"7","Type":"Images"}
{"Active":"1","Reclaimable":"12.5MB (80%)","Size":"15.6MB","TotalCount":"4","Type":"Containers"}
{"Active":"0","Reclaimable":"0B","Size":"0B","TotalCount":"0","Type":"Local Volumes"}
WARNING: something odd
{"Active":"0","Reclaimable":"4.2GB","Size":"4.2GB","TotalCount":"51","Type":"Build Cache"}
"#;
        let rows = parse_system_df(out);
        assert_eq!(
            rows,
            vec![
                DfRow {
                    kind: DfKind::Images,
                    label: "Images".into(),
                    size: 2_300_000_000,
                    reclaimable: 1_100_000_000,
                },
                DfRow {
                    kind: DfKind::Containers,
                    label: "Containers".into(),
                    size: 15_600_000,
                    reclaimable: 12_500_000,
                },
                DfRow {
                    kind: DfKind::Volumes,
                    label: "Local Volumes".into(),
                    size: 0,
                    reclaimable: 0,
                },
                DfRow {
                    kind: DfKind::BuildCache,
                    label: "Build Cache".into(),
                    size: 4_200_000_000,
                    reclaimable: 4_200_000_000,
                },
            ]
        );
    }

    #[test]
    fn parse_system_df_empty_or_garbage() {
        assert!(parse_system_df("").is_empty());
        assert!(parse_system_df("not json\n{\"no\":\"type\"}").is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn docker_desktop_raw_is_informational_and_sparse_aware() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join("Library/Containers/com.docker.docker/Data/vms/0/data");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("Docker.raw");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&raw).unwrap();
            f.write_all(&[1u8; 8192]).unwrap();
            f.set_len(1 << 30).unwrap();
        }

        let item = DockerScanner.analyze(&raw).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::ContainerVmDisk);
        assert_eq!(item.risk, RiskLevel::Caution);
        assert!(
            item.size_bytes < 1 << 20,
            "sparse image counted as {}",
            item.size_bytes
        );
        // Never offered for deletion.
        assert!(item.available_actions.iter().all(|a| !matches!(
            a.method,
            ActionMethod::RemoveFile { .. }
                | ActionMethod::RemoveDir { .. }
                | ActionMethod::RemoveFiles { .. }
                | ActionMethod::RemoveDirs { .. }
                | ActionMethod::MoveToTrash { .. }
        )));
        // Informational: it contains the Docker item's data, so any action
        // here would double count it in the reclaimable total.
        assert!(item.available_actions.is_empty());
        assert!(item
            .details
            .iter()
            .any(|d| d.value.contains("Troubleshoot")));
    }

    #[test]
    fn overlapping_df_rows_are_capped_at_the_vm_image() {
        // Real numbers from a Docker Desktop machine: 163.5 GB images +
        // 28.6 GB volumes + 162.9 GB build cache "reclaimable" inside a
        // 220 GB Docker.raw.
        let summed = 163_500_000_000 + 28_550_000_000 + 162_900_000_000;
        let image = 220_000_000_000;
        assert_eq!(cap(summed, Some(image)), image);
        assert_eq!(cap(1_000, Some(image)), 1_000);
        assert_eq!(cap(summed, None), summed);
    }

    #[tokio::test]
    async fn colima_disk_has_no_actions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".colima/_lima/colima");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("diffdisk"), vec![1u8; 4096]).unwrap();
        let item = DockerScanner
            .analyze(&dir.join("diffdisk"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, ArtifactKind::ContainerVmDisk);
        assert!(item.available_actions.is_empty());
    }

    #[tokio::test]
    async fn unrelated_path_is_not_a_vm_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("random.img");
        std::fs::write(&f, "x").unwrap();
        assert!(DockerScanner.analyze(&f).await.unwrap().is_none());
    }
}
