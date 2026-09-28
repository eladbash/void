//! Local model stores (Ollama, Hugging Face, LM Studio, torch hub, ComfyUI) and duplicate model files.
//!
//! Every model is its own item so the user can pick which to drop, and every
//! size is what deleting *that* item actually frees: Ollama layers shared
//! with another model, and Hugging Face blobs a live revision still uses, are
//! never counted as reclaimable.
//!
//! # Where the duplicate-files item comes from
//!
//! The orchestrator analyzes one root at a time, but duplicates span stores.
//! So the scanner nominates one *anchor* — the first existing store root in a
//! fixed order ([`ModelScanner::global_locations`] order, then any ComfyUI
//! directory the walk found) — and analyzing the anchor additionally runs the
//! cross-store duplicate search over every known root. The walk phase always
//! finishes before analysis starts, so by then every walked ComfyUI directory
//! has been recorded by [`EcosystemScanner::is_candidate`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytesize::ByteSize;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

/// Files smaller than this are not worth hashing for duplicates: model
/// weights are hundreds of MB to tens of GB, and small files are configs and
/// tokenizers where sharing storage saves nothing meaningful.
pub const DEFAULT_MIN_DUP_SIZE: u64 = 64 * 1024 * 1024;

/// Unreferenced files younger than this may belong to a download in
/// progress (an `ollama pull`, a Hugging Face `.incomplete`) and are left
/// alone.
const IN_PROGRESS_GRACE: Duration = Duration::from_secs(24 * 3600);

/// Ollama layer media types that hold weights (worth deduplicating).
const OLLAMA_WEIGHT_TYPES: [&str; 3] = [
    "application/vnd.ollama.image.model",
    "application/vnd.ollama.image.projector",
    "application/vnd.ollama.image.adapter",
];

/// Which kind of store a root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Store {
    Ollama,
    HuggingFace,
    LmStudio,
    TorchHub,
    ComfyUi,
}

pub struct ModelScanner {
    home: PathBuf,
    config: AppConfig,
    /// Every store root this scanner knows, existing or not, in anchor order.
    roots: Vec<(Store, PathBuf)>,
    /// File names of `roots`, so `is_candidate` can reject almost every
    /// walked entry without comparing paths.
    root_names: HashSet<OsString>,
    min_dup_size: u64,
    /// ComfyUI `models` directories the walk handed to this scanner.
    walked_comfy: Mutex<BTreeSet<PathBuf>>,
}

impl ModelScanner {
    /// A scanner rooted at `home` — the real home in the app, a temp dir in
    /// tests and the sandbox.
    ///
    /// Environment overrides (`OLLAMA_MODELS`, `HF_HUB_CACHE`, `HF_HOME`,
    /// `TORCH_HOME`) and system-wide stores are honored only when `home` is
    /// the real home: in a sandbox or a test they would point at the user's
    /// real models.
    pub fn new(home: PathBuf, config: &AppConfig) -> Self {
        let honor_env = dirs::home_dir()
            .map(|real| canonical(&real) == canonical(&home))
            .unwrap_or(false);
        let env = |key: &str| -> Option<OsString> {
            if honor_env {
                std::env::var_os(key).filter(|v| !v.is_empty())
            } else {
                None
            }
        };
        let roots = store_roots(&home, &env, honor_env);
        let root_names = roots
            .iter()
            .filter_map(|(_, p)| p.file_name().map(|n| n.to_os_string()))
            .collect();
        Self {
            home,
            config: config.clone(),
            roots,
            root_names,
            min_dup_size: DEFAULT_MIN_DUP_SIZE,
            walked_comfy: Mutex::new(BTreeSet::new()),
        }
    }

    /// Only hash files of at least `bytes` for duplicates. Tests use tiny
    /// fixtures and pass 1.
    pub fn with_min_dup_size(mut self, bytes: u64) -> Self {
        self.min_dup_size = bytes;
        self
    }

    /// The home this scanner resolves stores under.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Store roots that exist, in anchor order, without duplicates.
    fn existing_roots(&self) -> Vec<(Store, PathBuf)> {
        let mut seen = HashSet::new();
        self.roots
            .iter()
            .filter(|(_, p)| p.is_dir() && seen.insert(canonical(p)))
            .cloned()
            .collect()
    }

    /// Every root to search for duplicates: existing stores plus walked
    /// ComfyUI directories.
    fn all_roots(&self) -> Vec<(Store, PathBuf)> {
        let mut roots = self.existing_roots();
        let mut seen: HashSet<PathBuf> = roots.iter().map(|(_, p)| canonical(p)).collect();
        let walked = self
            .walked_comfy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for p in walked {
            if seen.insert(canonical(&p)) {
                roots.push((Store::ComfyUi, p));
            }
        }
        roots
    }

    fn classify(&self, path: &Path) -> Option<Store> {
        let c = canonical(path);
        if let Some((store, _)) = self.roots.iter().find(|(_, r)| canonical(r) == c) {
            return Some(*store);
        }
        is_comfy_models(path).then_some(Store::ComfyUi)
    }

    /// Whether `path` is where the cross-store duplicate item is emitted.
    fn is_dedup_anchor(&self, path: &Path) -> bool {
        self.all_roots()
            .first()
            .is_some_and(|(_, anchor)| canonical(anchor) == canonical(path))
    }

    /// Search every known store for byte-identical model files. Public so
    /// callers that do not go through the orchestrator can ask directly.
    pub async fn duplicate_models(&self) -> Option<CleanableItem> {
        let roots = self.all_roots();
        let anchor = roots.first()?.1.clone();
        let files = tokio::task::spawn_blocking(move || dedup_candidates(&roots))
            .await
            .unwrap_or_default();
        let groups = crate::dedup::find_duplicates(files, self.min_dup_size).await;
        duplicate_item(anchor, groups)
    }

    /// Analyze every store and the duplicate search in one go — what a full
    /// scan through the orchestrator yields for this ecosystem.
    pub async fn scan_all(&self) -> Vec<CleanableItem> {
        let mut items = Vec::new();
        for (_, root) in self.all_roots() {
            if let Ok(mut found) = self.analyze_many(&root).await {
                items.append(&mut found);
            }
        }
        items
    }
}

#[async_trait]
impl EcosystemScanner for ModelScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Models
    }

    /// Claims ComfyUI `models` directories (recording them for the duplicate
    /// search) and known store roots, so the walk does not descend into a
    /// model store and let another scanner claim pieces of it.
    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        if file_name == "models" && is_comfy_models(path) {
            self.walked_comfy
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(path.to_path_buf());
            return true;
        }
        self.root_names.contains(std::ffi::OsStr::new(file_name))
            && self.roots.iter().any(|(_, r)| r == path)
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        Ok(self.analyze_many(path).await?.into_iter().next())
    }

    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        let Some(store) = self.classify(path) else {
            return Ok(Vec::new());
        };
        if !path.is_dir() {
            return Ok(Vec::new());
        }
        let root = path.to_path_buf();
        let mut items = tokio::task::spawn_blocking(move || {
            let now = SystemTime::now();
            match store {
                Store::Ollama => ollama_items(&root, now),
                Store::HuggingFace => hf_items(&root, now),
                Store::LmStudio => lmstudio_items(&root),
                Store::TorchHub => torch_items(&root).into_iter().collect(),
                Store::ComfyUi => comfy_items(&root),
            }
        })
        .await
        .map_err(|e| ScanError::Scanner(format!("model analysis failed: {e}")))?;

        if self.config.ai.detect_duplicate_models && self.is_dedup_anchor(path) {
            if let Some(item) = self.duplicate_models().await {
                items.push(item);
            }
        }
        Ok(items)
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        self.existing_roots().into_iter().map(|(_, p)| p).collect()
    }
}

/// Every place a store may live, in anchor order. `env` yields environment
/// overrides (always `None` outside the real home); `system` adds stores
/// outside home (the Linux Ollama service's).
fn store_roots(
    home: &Path,
    env: &dyn Fn(&str) -> Option<OsString>,
    system: bool,
) -> Vec<(Store, PathBuf)> {
    let mut roots = Vec::new();

    if let Some(dir) = env("OLLAMA_MODELS") {
        roots.push((Store::Ollama, PathBuf::from(dir)));
    }
    roots.push((Store::Ollama, home.join(".ollama/models")));
    if system && cfg!(target_os = "linux") {
        roots.push((
            Store::Ollama,
            PathBuf::from("/usr/share/ollama/.ollama/models"),
        ));
    }

    if let Some(dir) = env("HF_HUB_CACHE") {
        roots.push((Store::HuggingFace, PathBuf::from(dir)));
    } else if let Some(dir) = env("HF_HOME") {
        roots.push((Store::HuggingFace, PathBuf::from(dir).join("hub")));
    }
    roots.push((Store::HuggingFace, home.join(".cache/huggingface/hub")));

    roots.push((Store::LmStudio, home.join(".lmstudio/models")));
    roots.push((Store::LmStudio, home.join(".cache/lm-studio/models")));

    if let Some(dir) = env("TORCH_HOME") {
        roots.push((Store::TorchHub, PathBuf::from(dir).join("hub")));
    }
    roots.push((Store::TorchHub, home.join(".cache/torch/hub")));

    roots.push((Store::ComfyUi, home.join("ComfyUI/models")));
    roots
}

/// A ComfyUI `models` directory: named `models`, parent's name mentions
/// ComfyUI, and it has the `checkpoints` folder every install ships with.
fn is_comfy_models(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "models")
        && path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n.to_string_lossy().to_lowercase().contains("comfy"))
        && path.join("checkpoints").is_dir()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Shared helpers

fn base_item(
    path: PathBuf,
    kind: ArtifactKind,
    risk: RiskLevel,
    size_bytes: u64,
    agent: &str,
) -> CleanableItem {
    CleanableItem {
        id: Uuid::new_v4(),
        path,
        ecosystem: Ecosystem::Models,
        kind,
        risk,
        size_bytes,
        size_display: ByteSize(size_bytes).to_string(),
        last_modified: None,
        days_stale: None,
        project_name: None,
        project_root: None,
        available_actions: Vec::new(),
        details: Vec::new(),
        agent: Some(agent.to_string()),
    }
}

fn action(
    label: impl Into<String>,
    description: impl Into<String>,
    method: ActionMethod,
    risk: RiskLevel,
    estimated: u64,
) -> CleanAction {
    CleanAction {
        id: Uuid::new_v4(),
        label: label.into(),
        description: description.into(),
        method,
        risk,
        estimated_savings_bytes: estimated,
    }
}

fn set_modified(item: &mut CleanableItem, when: Option<DateTime<Utc>>) {
    item.last_modified = when;
    item.days_stale = when.map(staleness::days_since);
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(path).ok()?.modified().ok()
}

/// Too new to be sure it is not a download in progress. A file whose mtime
/// cannot be read counts as fresh: when in doubt, keep.
fn is_fresh(meta: &std::fs::Metadata, now: SystemTime) -> bool {
    match meta.modified() {
        Ok(m) => now
            .duration_since(m)
            .map_or(true, |age| age < IN_PROGRESS_GRACE),
        Err(_) => true,
    }
}

/// Regular files under `dir`, not following symlinks.
fn regular_files(dir: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(dir)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn file_len(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .map(|m| staleness::on_disk_len(&m))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Ollama

#[derive(Deserialize)]
struct ManifestJson {
    config: Option<LayerJson>,
    #[serde(default)]
    layers: Vec<LayerJson>,
}

#[derive(Deserialize)]
struct LayerJson {
    digest: String,
    #[serde(default, rename = "mediaType")]
    media_type: String,
}

#[derive(Deserialize, Default)]
struct ConfigJson {
    model_family: Option<String>,
    model_type: Option<String>,
    file_type: Option<String>,
}

struct OllamaManifest {
    path: PathBuf,
    name: String,
    /// Blob file names (`sha256-<hex>`) with their media type; config first.
    blobs: Vec<(String, String)>,
    config_blob: Option<String>,
}

/// `sha256:<hex>` → `sha256-<hex>`, refusing anything that is not a plain
/// hex digest (a manifest must not be able to point outside `blobs/`).
fn blob_name(digest: &str) -> Option<String> {
    let hex = digest
        .strip_prefix("sha256:")
        .or_else(|| digest.strip_prefix("sha256-"))?;
    (!hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("sha256-{hex}"))
}

/// The name `ollama rm` accepts for a manifest at
/// `manifests/<registry>/<namespace>/<model>/<tag>`.
fn ollama_model_name(manifests: &Path, manifest: &Path) -> Option<String> {
    let rel = manifest.strip_prefix(manifests).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let (tag, path) = parts.split_last()?;
    let name = match path {
        [registry, ns, model] if registry == "registry.ollama.ai" && ns == "library" => {
            model.clone()
        }
        [registry, rest @ ..] if registry == "registry.ollama.ai" && !rest.is_empty() => {
            rest.join("/")
        }
        [] => return None,
        all => all.join("/"),
    };
    Some(format!("{name}:{tag}"))
}

/// Parse every manifest. The flag is false if any manifest could not be
/// parsed, in which case blob references are incomplete and nothing may be
/// declared an orphan.
fn read_manifests(root: &Path) -> (Vec<OllamaManifest>, bool) {
    let manifests_dir = root.join("manifests");
    let mut all_parsed = manifests_dir.is_dir();
    let mut out = Vec::new();
    let mut files = regular_files(&manifests_dir);
    files.sort();
    for path in files {
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<ManifestJson>(&s).ok());
        let Some(json) = parsed else {
            all_parsed = false;
            continue;
        };
        let mut blobs = Vec::new();
        let mut valid = true;
        let config_blob = json.config.as_ref().and_then(|c| blob_name(&c.digest));
        if json.config.is_some() && config_blob.is_none() {
            valid = false;
        }
        if let Some(c) = &config_blob {
            blobs.push((c.clone(), String::new()));
        }
        for layer in &json.layers {
            match blob_name(&layer.digest) {
                Some(b) => blobs.push((b, layer.media_type.clone())),
                None => valid = false,
            }
        }
        let name = ollama_model_name(&manifests_dir, &path);
        match (valid, name) {
            (true, Some(name)) => out.push(OllamaManifest {
                path,
                name,
                blobs,
                config_blob,
            }),
            _ => all_parsed = false,
        }
    }
    (out, all_parsed)
}

fn ollama_items(root: &Path, now: SystemTime) -> Vec<CleanableItem> {
    let blobs_dir = root.join("blobs");
    let (manifests, all_parsed) = read_manifests(root);

    // How many models reference each blob.
    let mut refcount: HashMap<&str, usize> = HashMap::new();
    for m in &manifests {
        let unique: HashSet<&str> = m.blobs.iter().map(|(b, _)| b.as_str()).collect();
        for b in unique {
            *refcount.entry(b).or_default() += 1;
        }
    }

    let mut items = Vec::new();
    for m in &manifests {
        let blobs: BTreeSet<&str> = m.blobs.iter().map(|(b, _)| b.as_str()).collect();
        let (mut unique, mut total, mut shared_bytes, mut shared_count) = (0u64, 0u64, 0u64, 0);
        for b in &blobs {
            let len = file_len(&blobs_dir.join(b));
            total += len;
            if refcount.get(b).copied().unwrap_or(0) > 1 {
                shared_bytes += len;
                shared_count += 1;
            } else {
                unique += len;
            }
        }

        let mut item = base_item(
            m.path.clone(),
            ArtifactKind::OllamaModel,
            RiskLevel::Caution,
            unique,
            "ollama",
        );
        item.project_name = Some(m.name.clone());
        item.project_root = Some(root.to_path_buf());
        set_modified(&mut item, mtime(&m.path).map(DateTime::<Utc>::from));

        item.details.push(Detail::new("Model", m.name.clone()));
        item.details.push(Detail::new(
            "Freed by removing",
            format!("{} (only layers no other model uses)", ByteSize(unique)),
        ));
        item.details
            .push(Detail::new("Size on disk", ByteSize(total).to_string()));
        item.details.push(Detail::new(
            "Shared layers",
            if shared_count == 0 {
                "none".to_string()
            } else {
                format!(
                    "{shared_count} ({}) — kept, other models use them",
                    ByteSize(shared_bytes)
                )
            },
        ));
        if let Some(config) = m
            .config_blob
            .as_ref()
            .and_then(|c| read_ollama_config(&blobs_dir.join(c)))
        {
            if let Some(v) = config.model_family {
                item.details.push(Detail::new("Family", v));
            }
            if let Some(v) = config.model_type {
                item.details.push(Detail::new("Parameters", v));
            }
            if let Some(v) = config.file_type {
                item.details.push(Detail::new("Quantization", v));
            }
        }

        // A name that looks like a flag would be parsed as one by the CLI.
        if !m.name.starts_with('-') {
            item.available_actions.push(action(
                format!("ollama rm {}", m.name),
                format!(
                    "Run `ollama rm {}`. Layers other models share are kept. \
                     Re-download any time with `ollama pull {}`.",
                    m.name, m.name
                ),
                ActionMethod::Command {
                    program: "ollama".into(),
                    args: vec!["rm".into(), m.name.clone()],
                    working_dir: None,
                },
                RiskLevel::Caution,
                unique,
            ));
        }
        items.push(item);
    }

    if all_parsed {
        let referenced: HashSet<&str> = refcount.keys().copied().collect();
        if let Some(item) = ollama_orphans(root, &blobs_dir, &referenced, now) {
            items.push(item);
        }
    }
    items
}

fn read_ollama_config(path: &Path) -> Option<ConfigJson> {
    if std::fs::metadata(path).ok()?.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Blobs no manifest references. Only called when every manifest parsed, so
/// "unreferenced" is known, not guessed.
fn ollama_orphans(
    root: &Path,
    blobs_dir: &Path,
    referenced: &HashSet<&str>,
    now: SystemTime,
) -> Option<CleanableItem> {
    let mut orphans = Vec::new();
    let mut bytes = 0u64;
    let mut skipped_fresh = 0usize;
    let mut newest: Option<SystemTime> = None;

    let mut entries: Vec<_> = std::fs::read_dir(blobs_dir).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // Older Ollama versions named blobs `sha256:<hex>`.
        let normalized = name.replacen("sha256:", "sha256-", 1);
        if !normalized.starts_with("sha256-") || referenced.contains(normalized.as_str()) {
            continue;
        }
        if is_fresh(&meta, now) {
            skipped_fresh += 1;
            continue;
        }
        if let Ok(m) = meta.modified() {
            newest = newest.max(Some(m));
        }
        bytes += staleness::on_disk_len(&meta);
        orphans.push(entry.path());
    }
    if orphans.is_empty() {
        return None;
    }

    let count = orphans.len();
    let mut item = base_item(
        blobs_dir.to_path_buf(),
        ArtifactKind::OllamaOrphanBlobs,
        RiskLevel::Safe,
        bytes,
        "ollama",
    );
    item.project_name = Some("Ollama unreferenced blobs".into());
    item.project_root = Some(root.to_path_buf());
    set_modified(&mut item, newest.map(DateTime::<Utc>::from));
    item.details
        .push(Detail::new("Unreferenced blobs", count.to_string()));
    item.details.push(Detail::new(
        "Why",
        "No installed model's manifest references these — leftovers of removed \
         models and abandoned pulls. Ollama also collects them on restart \
         (unless OLLAMA_NOPRUNE is set).",
    ));
    if skipped_fresh > 0 {
        item.details.push(Detail::new(
            "Skipped",
            format!("{skipped_fresh} recent file(s) — a pull may be in progress"),
        ));
    }
    item.available_actions.push(action(
        format!("Remove {count} unreferenced blobs"),
        "Delete blobs no installed model uses. Installed models are untouched.",
        ActionMethod::RemoveOllamaOrphans {
            store: root.to_path_buf(),
            blobs: orphans,
        },
        RiskLevel::Safe,
        bytes,
    ));
    Some(item)
}

/// Blob file names (`sha256-<hex>`) referenced by any manifest in `store`, or
/// `None` if any manifest could not be parsed — in which case nothing may be
/// declared unreferenced.
pub fn referenced_blobs(store: &Path) -> Option<HashSet<String>> {
    let (manifests, all_parsed) = read_manifests(store);
    if !all_parsed {
        return None;
    }
    Some(
        manifests
            .into_iter()
            .flat_map(|m| m.blobs.into_iter().map(|(name, _)| name))
            .collect(),
    )
}

/// Remove the blobs among `blobs` that are *still* unreferenced, re-reading
/// every manifest first. Returns bytes freed.
///
/// A blob outside `<store>/blobs`, or one a manifest now references, is
/// skipped — the scan's list is a proposal, not an authority.
pub fn remove_orphans_checked(store: &Path, blobs: &[PathBuf]) -> Result<u64, String> {
    let referenced = referenced_blobs(store).ok_or_else(|| {
        "an Ollama manifest could not be read, so no blob can be proven unused".to_string()
    })?;
    let blobs_dir = canonical(&store.join("blobs"));
    let mut freed = 0u64;
    let mut kept = 0usize;
    for blob in blobs {
        let Ok(meta) = std::fs::symlink_metadata(blob) else {
            continue;
        };
        let inside = blob
            .parent()
            .map(|p| canonical(p) == blobs_dir)
            .unwrap_or(false);
        let name = blob
            .file_name()
            .map(|n| n.to_string_lossy().replacen("sha256:", "sha256-", 1))
            .unwrap_or_default();
        if !inside || !meta.file_type().is_file() || !name.starts_with("sha256-") {
            kept += 1;
            continue;
        }
        if referenced.contains(&name) {
            kept += 1;
            continue;
        }
        let size = staleness::on_disk_len(&meta);
        std::fs::remove_file(blob).map_err(|e| format!("{}: {e}", blob.display()))?;
        freed += size;
    }
    if kept > 0 {
        tracing::info!(
            kept,
            "Kept Ollama blobs that are referenced again or out of place"
        );
    }
    Ok(freed)
}

/// Weight blobs of every parsed manifest.
fn ollama_weight_files(root: &Path) -> Vec<PathBuf> {
    let (manifests, _) = read_manifests(root);
    let blobs_dir = root.join("blobs");
    manifests
        .iter()
        .flat_map(|m| m.blobs.iter())
        .filter(|(_, media)| OLLAMA_WEIGHT_TYPES.contains(&media.as_str()))
        .map(|(b, _)| blobs_dir.join(b))
        .filter(|p| p.is_file())
        .collect()
}

// ---------------------------------------------------------------------------
// Hugging Face

/// What a Hugging Face repo directory's revisions reference.
struct HfRepo {
    live_revs: usize,
    detached_revs: usize,
    /// Blobs referenced only by revisions no ref points at.
    detached_blobs: Vec<PathBuf>,
    /// `.incomplete` downloads older than the grace period.
    stale_incomplete: Vec<PathBuf>,
    newest: Option<SystemTime>,
}

/// Blob names the symlinks under a snapshot point at.
fn snapshot_blob_refs(snapshot: &Path, blobs_dir: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    for entry in ignore::WalkBuilder::new(snapshot)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .build()
        .flatten()
    {
        if !entry.path_is_symlink() {
            continue;
        }
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        if let Some(name) = target.file_name() {
            let blob = blobs_dir.join(name);
            if blob.is_file() {
                out.insert(blob);
            }
        }
    }
    out
}

fn inspect_hf_repo(repo: &Path, now: SystemTime) -> HfRepo {
    let blobs_dir = repo.join("blobs");
    let mut newest: Option<SystemTime> = None;

    let mut live: HashSet<String> = HashSet::new();
    for r in regular_files(&repo.join("refs")) {
        if let Ok(s) = std::fs::read_to_string(&r) {
            live.insert(s.trim().to_string());
        }
        newest = newest.max(mtime(&r));
    }

    let mut live_blobs = HashSet::new();
    let mut detached_candidates = HashSet::new();
    let (mut live_revs, mut detached_revs) = (0, 0);
    for snap in subdirs(&repo.join("snapshots")) {
        newest = newest.max(mtime(&snap));
        let rev = snap
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let refs = snapshot_blob_refs(&snap, &blobs_dir);
        // With no refs at all, which revision is current is unknown: treat
        // every one as live rather than guess.
        if live.is_empty() || live.contains(&rev) {
            live_revs += 1;
            live_blobs.extend(refs);
        } else {
            detached_revs += 1;
            detached_candidates.extend(refs);
        }
    }
    let mut detached_blobs: Vec<PathBuf> = detached_candidates
        .into_iter()
        .filter(|b| !live_blobs.contains(b))
        .collect();
    detached_blobs.sort();

    let mut stale_incomplete: Vec<PathBuf> = std::fs::read_dir(&blobs_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".incomplete"))
        .filter(|e| {
            e.metadata()
                .is_ok_and(|m| m.file_type().is_file() && !is_fresh(&m, now))
        })
        .map(|e| e.path())
        .collect();
    stale_incomplete.sort();

    HfRepo {
        live_revs,
        detached_revs,
        detached_blobs,
        stale_incomplete,
        newest,
    }
}

fn hf_items(root: &Path, now: SystemTime) -> Vec<CleanableItem> {
    subdirs(root)
        .into_iter()
        .filter_map(|repo| hf_repo_item(&repo, now))
        .collect()
}

fn hf_repo_item(repo: &Path, now: SystemTime) -> Option<CleanableItem> {
    let dir_name = repo.file_name()?.to_string_lossy().to_string();
    let (repo_type, rest) = ["models", "datasets", "spaces"]
        .into_iter()
        .find_map(|t| dir_name.strip_prefix(&format!("{t}--")).map(|r| (t, r)))?;
    let repo_id = rest.replace("--", "/");

    let size = staleness::compute_dir_size_sync(repo);
    if size == 0 {
        return None;
    }
    let info = inspect_hf_repo(repo, now);
    let detached_bytes: u64 = info.detached_blobs.iter().map(|p| file_len(p)).sum();
    let incomplete_bytes: u64 = info.stale_incomplete.iter().map(|p| file_len(p)).sum();

    let mut item = base_item(
        repo.to_path_buf(),
        ArtifactKind::HuggingFaceModel,
        RiskLevel::Caution,
        size,
        "huggingface",
    );
    item.project_name = Some(repo_id.clone());
    item.project_root = repo.parent().map(Path::to_path_buf);
    let last = info
        .newest
        .map(DateTime::<Utc>::from)
        .or_else(|| staleness::most_recent_modification(repo));
    set_modified(&mut item, last);

    item.details.push(Detail::new("Repo", repo_id.clone()));
    item.details.push(Detail::new(
        "Type",
        repo_type.trim_end_matches('s').to_string(),
    ));
    item.details.push(Detail::new(
        "Revisions",
        format!(
            "{} ({} current, {} old)",
            info.live_revs + info.detached_revs,
            info.live_revs,
            info.detached_revs
        ),
    ));
    if detached_bytes > 0 {
        item.details.push(Detail::new(
            "Old revisions",
            format!("{} no current revision uses", ByteSize(detached_bytes)),
        ));
    }
    if !info.stale_incomplete.is_empty() {
        item.details.push(Detail::new(
            "Failed downloads",
            format!(
                "{} ({})",
                info.stale_incomplete.len(),
                ByteSize(incomplete_bytes)
            ),
        ));
    }
    if let Some(when) = last {
        item.details.push(Detail::new(
            "Last used",
            when.format("%Y-%m-%d").to_string(),
        ));
    }

    let trim_bytes = detached_bytes + incomplete_bytes;
    if trim_bytes > 0 {
        let mut paths = info.detached_blobs.clone();
        paths.extend(info.stale_incomplete.iter().cloned());
        let label = match (detached_bytes > 0, incomplete_bytes > 0) {
            (true, true) => "Remove old revisions and failed downloads",
            (true, false) => "Remove old revisions",
            _ => "Remove failed downloads",
        };
        item.available_actions.push(action(
            label,
            "Delete blobs only old (detached) revisions use, and interrupted \
             downloads — like `hf cache prune`. The current revision stays fully \
             usable; an old revision re-downloads if you ask for it by hash.",
            ActionMethod::RemoveFiles { paths },
            RiskLevel::Safe,
            trim_bytes,
        ));
    }
    item.available_actions.push(action(
        format!("Delete {repo_id} from the cache"),
        format!(
            "Delete {} (like `hf cache rm {repo_id}`). Re-downloaded on next use.",
            repo.display()
        ),
        ActionMethod::RemoveDir {
            path: repo.to_path_buf(),
        },
        RiskLevel::Caution,
        size,
    ));
    Some(item)
}

fn hf_blob_files(root: &Path) -> Vec<PathBuf> {
    subdirs(root)
        .into_iter()
        .flat_map(|repo| {
            std::fs::read_dir(repo.join("blobs"))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.path())
                .filter(|p| p.extension().is_none_or(|e| e != "incomplete"))
                .collect::<Vec<_>>()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// LM Studio, torch hub, ComfyUI

fn lmstudio_items(root: &Path) -> Vec<CleanableItem> {
    let mut items = Vec::new();
    for publisher in subdirs(root) {
        for repo in subdirs(&publisher) {
            let size = staleness::compute_dir_size_sync(&repo);
            if size == 0 {
                continue;
            }
            let name = format!(
                "{}/{}",
                publisher.file_name().unwrap_or_default().to_string_lossy(),
                repo.file_name().unwrap_or_default().to_string_lossy()
            );
            let files: Vec<String> = regular_files(&repo)
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect();

            let mut item = base_item(
                repo.clone(),
                ArtifactKind::LmStudioModel,
                RiskLevel::Caution,
                size,
                "lmstudio",
            );
            item.project_name = Some(name.clone());
            item.project_root = Some(root.to_path_buf());
            set_modified(&mut item, staleness::most_recent_modification(&repo));
            item.details.push(Detail::new("Model", name.clone()));
            item.details.push(Detail::new("Files", files.join(", ")));
            item.available_actions.push(action(
                format!("Delete {name}"),
                format!(
                    "Delete {}. Re-download from LM Studio's model search.",
                    repo.display()
                ),
                ActionMethod::RemoveDir { path: repo.clone() },
                RiskLevel::Caution,
                size,
            ));
            items.push(item);
        }
    }
    items
}

fn lmstudio_weight_files(root: &Path) -> Vec<PathBuf> {
    regular_files(root)
        .into_iter()
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e == "gguf" || e == "safetensors")
        })
        .collect()
}

fn torch_items(root: &Path) -> Option<CleanableItem> {
    let size = staleness::compute_dir_size_sync(root);
    if size == 0 {
        return None;
    }
    let mut item = base_item(
        root.to_path_buf(),
        ArtifactKind::TorchHubCache,
        RiskLevel::Safe,
        size,
        "torch",
    );
    item.project_name = Some("torch hub".into());
    set_modified(&mut item, staleness::most_recent_modification(root));
    item.details.push(Detail::new(
        "Contents",
        "Repos and checkpoints fetched by torch.hub; re-downloaded on next use",
    ));
    item.available_actions.push(action(
        "Remove torch hub cache",
        format!("Delete {}", root.display()),
        ActionMethod::RemoveDir {
            path: root.to_path_buf(),
        },
        RiskLevel::Safe,
        size,
    ));
    Some(item)
}

fn comfy_items(models: &Path) -> Vec<CleanableItem> {
    let install = models.parent().map(Path::to_path_buf);
    let mut items = Vec::new();
    for folder in subdirs(models) {
        let size = staleness::compute_dir_size_sync(&folder);
        if size == 0 {
            continue;
        }
        let folder_name = folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let count = regular_files(&folder).len();
        let mut item = base_item(
            folder.clone(),
            ArtifactKind::ComfyUiModels,
            RiskLevel::Caution,
            size,
            "comfyui",
        );
        item.project_name = Some(format!("ComfyUI {folder_name}"));
        item.project_root = install.clone();
        set_modified(&mut item, staleness::most_recent_modification(&folder));
        item.details
            .push(Detail::new("Folder", folder_name.clone()));
        item.details.push(Detail::new("Files", count.to_string()));
        item.details.push(Detail::new(
            "Note",
            "May include models you trained or cannot download again",
        ));
        item.available_actions.push(action(
            format!("Delete ComfyUI {folder_name}"),
            format!("Delete {}", folder.display()),
            ActionMethod::RemoveDir { path: folder },
            RiskLevel::Caution,
            size,
        ));
        items.push(item);
    }
    items
}

// ---------------------------------------------------------------------------
// Duplicates

/// Model files across every store, candidates for the duplicate search.
fn dedup_candidates(roots: &[(Store, PathBuf)]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for (store, root) in roots {
        match store {
            Store::Ollama => files.extend(ollama_weight_files(root)),
            Store::HuggingFace => files.extend(hf_blob_files(root)),
            Store::LmStudio => files.extend(lmstudio_weight_files(root)),
            Store::ComfyUi => files.extend(regular_files(root)),
            // Checkpoints are fetched by URL, not shared with other stores.
            Store::TorchHub => {}
        }
    }
    files
}

fn duplicate_item(anchor: PathBuf, groups: Vec<DedupGroup>) -> Option<CleanableItem> {
    if groups.is_empty() {
        return None;
    }
    let savings: u64 = groups
        .iter()
        .map(|g| g.size_bytes * g.duplicates.len() as u64)
        .sum();

    let mut item = CleanableItem {
        agent: None,
        ..base_item(
            anchor,
            ArtifactKind::DuplicateModelFiles,
            RiskLevel::Safe,
            savings,
            "",
        )
    };
    item.project_name = Some("Duplicate model files".into());
    item.details
        .push(Detail::new("Duplicate sets", groups.len().to_string()));
    item.details
        .push(Detail::new("Reclaimable", ByteSize(savings).to_string()));
    item.details.push(Detail::new(
        "How",
        "Each extra copy becomes a copy-on-write clone (APFS, btrfs) or a hardlink \
         of the kept file. Nothing is deleted and every tool keeps its file; \
         contents are re-verified before each replacement. Files that are \
         already clones of each other are counted but save nothing more.",
    ));
    for (i, g) in groups.iter().enumerate() {
        let mut lines = vec![format!("{} (kept)", g.keep.display())];
        lines.extend(g.duplicates.iter().map(|d| d.display().to_string()));
        item.details.push(Detail::new(
            format!(
                "Set {}: {} × {}",
                i + 1,
                ByteSize(g.size_bytes),
                g.duplicates.len() + 1
            ),
            lines.join("\n"),
        ));
    }
    item.available_actions.push(action(
        "Share storage between identical copies",
        "Replace byte-identical duplicates with clones or hardlinks of one copy. \
         No model is removed from any tool.",
        ActionMethod::DedupFiles { groups },
        RiskLevel::Safe,
        savings,
    ));
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_names_refuse_path_tricks() {
        assert_eq!(blob_name("sha256:abc123").as_deref(), Some("sha256-abc123"));
        assert_eq!(blob_name("sha256-ABC").as_deref(), Some("sha256-ABC"));
        assert!(blob_name("sha256:../../etc/passwd").is_none());
        assert!(blob_name("sha256:").is_none());
        assert!(blob_name("md5:abc").is_none());
    }

    #[test]
    fn ollama_names_match_what_ollama_rm_accepts() {
        let m = Path::new("/m/manifests");
        assert_eq!(
            ollama_model_name(m, &m.join("registry.ollama.ai/library/llama3.1/70b")).as_deref(),
            Some("llama3.1:70b")
        );
        assert_eq!(
            ollama_model_name(m, &m.join("registry.ollama.ai/jmorgan/mixtral/latest")).as_deref(),
            Some("jmorgan/mixtral:latest")
        );
        assert_eq!(
            ollama_model_name(m, &m.join("hf.co/bartowski/Llama-3.2-GGUF/Q4_K_M")).as_deref(),
            Some("hf.co/bartowski/Llama-3.2-GGUF:Q4_K_M")
        );
    }

    #[test]
    fn env_overrides_come_first_and_system_store_only_when_asked() {
        let home = Path::new("/h");
        let env = |k: &str| match k {
            "OLLAMA_MODELS" => Some(OsString::from("/big/ollama")),
            "HF_HOME" => Some(OsString::from("/big/hf")),
            _ => None,
        };
        let roots = store_roots(home, &env, false);
        assert_eq!(roots[0], (Store::Ollama, PathBuf::from("/big/ollama")));
        assert!(roots.contains(&(Store::HuggingFace, PathBuf::from("/big/hf/hub"))));
        assert!(roots.contains(&(Store::HuggingFace, home.join(".cache/huggingface/hub"))));
        assert!(!roots.iter().any(|(_, p)| p.starts_with("/usr")));

        let none = |_: &str| None;
        let roots = store_roots(home, &none, false);
        assert!(roots.iter().all(|(_, p)| p.starts_with(home)));
    }

    #[test]
    fn sandboxed_scanner_ignores_environment_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let scanner = ModelScanner::new(tmp.path().to_path_buf(), &AppConfig::default());
        let home = canonical(tmp.path());
        assert!(
            scanner
                .roots
                .iter()
                .all(|(_, p)| canonical(p).starts_with(&home) || p.starts_with(tmp.path())),
            "a sandboxed scanner must never look outside its home"
        );
    }

    #[test]
    fn comfy_models_dir_is_recognized_only_with_checkpoints() {
        let tmp = tempfile::tempdir().unwrap();
        let models = tmp.path().join("ComfyUI_windows_portable/ComfyUI/models");
        std::fs::create_dir_all(&models).unwrap();
        assert!(!is_comfy_models(&models));
        std::fs::create_dir_all(models.join("checkpoints")).unwrap();
        assert!(is_comfy_models(&models));
        let other = tmp.path().join("app/models");
        std::fs::create_dir_all(other.join("checkpoints")).unwrap();
        assert!(!is_comfy_models(&other));
    }

    #[test]
    fn unparseable_manifest_disables_orphan_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("models");
        std::fs::create_dir_all(root.join("manifests/registry.ollama.ai/library/x")).unwrap();
        std::fs::write(
            root.join("manifests/registry.ollama.ai/library/x/latest"),
            "not json",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        let blob = root.join("blobs/sha256-abcd");
        std::fs::write(&blob, "maybe referenced by the broken manifest").unwrap();
        crate::testkit::age(&blob, 5);

        let items = ollama_items(&root, SystemTime::now());
        assert!(items
            .iter()
            .all(|i| i.kind != ArtifactKind::OllamaOrphanBlobs));
    }
}
