//! Model manager and duplicate detection, end to end: the real scanner over a
//! fake home, the real executor (with a directory trash) acting on what it
//! found, and assertions on what is left on disk.

use std::path::{Path, PathBuf};

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::model::*;
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::models::ModelScanner;
use deepclean_core::scanner::EcosystemScanner;
use deepclean_core::testkit::{models, FakeHome};
use deepclean_core::trash::TrashBackend;

struct Env {
    _tmp: tempfile::TempDir,
    home: FakeHome,
    trash: PathBuf,
    fx: models::ModelsFixture,
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path().join("home"));
    let trash = tmp.path().join("trash");
    std::fs::create_dir_all(&trash).unwrap();
    let fx = models::all(&home);
    Env {
        _tmp: tmp,
        home,
        trash,
        fx,
    }
}

fn scanner(env: &Env) -> ModelScanner {
    ModelScanner::new(env.home.root().to_path_buf(), &AppConfig::default()).with_min_dup_size(1)
}

/// Run one action through the real executor; returns (succeeded, bytes, error).
async fn run(env: &Env, item: &CleanableItem, action: &CleanAction) -> (bool, u64, Option<String>) {
    let safety = SafetyChecker::with_home(env.home.root().to_path_buf(), vec![]);
    let exec = ActionExecutor::with_trash(safety, TrashBackend::Directory(env.trash.clone()));
    let mut rx = exec.execute_batch(vec![(item.clone(), action.clone())]);
    let (mut ok, mut bytes, mut err) = (false, 0, None);
    while let Some(ev) = rx.recv().await {
        match ev {
            ActionEvent::Completed { bytes_freed, .. } => {
                ok = true;
                bytes = bytes_freed;
            }
            ActionEvent::Failed { error, .. } => err = Some(error),
            ActionEvent::BatchComplete { .. } => break,
            _ => {}
        }
    }
    (ok, bytes, err)
}

fn one(items: &[CleanableItem], kind: ArtifactKind) -> &CleanableItem {
    let found: Vec<_> = items.iter().filter(|i| i.kind == kind).collect();
    assert_eq!(
        found.len(),
        1,
        "expected exactly one {kind:?}, got {}",
        found.len()
    );
    found[0]
}

fn named<'a>(items: &'a [CleanableItem], name: &str) -> &'a CleanableItem {
    items
        .iter()
        .find(|i| i.project_name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no item named {name}"))
}

/// Read a file through its symlink, as the model's tool would.
fn read_through(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{} unreadable: {e}", path.display()))
}

#[tokio::test]
async fn finds_every_store_with_the_right_kinds() {
    let env = setup();
    let items = scanner(&env).scan_all().await;

    let kinds: Vec<ArtifactKind> = items.iter().map(|i| i.kind).collect();
    for kind in [
        ArtifactKind::OllamaModel,
        ArtifactKind::OllamaOrphanBlobs,
        ArtifactKind::HuggingFaceModel,
        ArtifactKind::LmStudioModel,
        ArtifactKind::TorchHubCache,
        ArtifactKind::ComfyUiModels,
        ArtifactKind::DuplicateModelFiles,
    ] {
        assert!(kinds.contains(&kind), "missing {kind:?} in {kinds:?}");
    }
    assert!(items.iter().all(|i| i.ecosystem == Ecosystem::Models));

    assert_eq!(
        named(&items, "llama3.2:1b").agent.as_deref(),
        Some("ollama")
    );
    assert_eq!(
        named(&items, "acme/tiny-llm").agent.as_deref(),
        Some("huggingface")
    );
    named(&items, "acme/evals");
    assert_eq!(
        named(&items, "lmstudio-community/tiny-llm-GGUF")
            .agent
            .as_deref(),
        Some("lmstudio")
    );
    // ComfyUI: one item per non-empty folder; the empty placeholder folder
    // is not listed.
    named(&items, "ComfyUI checkpoints");
    named(&items, "ComfyUI loras");
    assert!(items
        .iter()
        .all(|i| i.project_name.as_deref() != Some("ComfyUI vae")));

    // Lowest-risk action first, everywhere.
    for item in &items {
        let risks: Vec<RiskLevel> = item.available_actions.iter().map(|a| a.risk).collect();
        let mut sorted = risks.clone();
        sorted.sort();
        assert_eq!(
            risks, sorted,
            "{:?} actions out of order",
            item.project_name
        );
    }
}

#[tokio::test]
async fn duplicate_item_is_emitted_exactly_once_through_the_orchestrator_path() {
    let env = setup();
    let s = scanner(&env);
    let mut dups = 0;
    for root in s.global_locations() {
        dups += s
            .analyze_many(&root)
            .await
            .unwrap()
            .iter()
            .filter(|i| i.kind == ArtifactKind::DuplicateModelFiles)
            .count();
    }
    assert_eq!(dups, 1);
}

#[tokio::test]
async fn duplicate_detection_can_be_turned_off() {
    let env = setup();
    let mut config = AppConfig::default();
    config.ai.detect_duplicate_models = false;
    let s = ModelScanner::new(env.home.root().to_path_buf(), &config).with_min_dup_size(1);
    let items = s.scan_all().await;
    assert!(items
        .iter()
        .all(|i| i.kind != ArtifactKind::DuplicateModelFiles));
}

#[tokio::test]
async fn default_threshold_ignores_small_files() {
    let env = setup();
    let s = ModelScanner::new(env.home.root().to_path_buf(), &AppConfig::default());
    assert!(s.duplicate_models().await.is_none());
}

#[tokio::test]
async fn ollama_sizes_count_only_layers_unique_to_each_model() {
    let env = setup();
    let items = scanner(&env).scan_all().await;
    let o = &env.fx.ollama;

    let llama = named(&items, "llama3.2:1b");
    assert_eq!(llama.kind, ArtifactKind::OllamaModel);
    assert_eq!(llama.risk, RiskLevel::Caution);
    assert_eq!(llama.size_bytes, o.llama_unique_bytes);
    assert_eq!(llama.path, o.llama_manifest);
    let qwen = named(&items, "qwen2.5:7b");
    assert_eq!(qwen.size_bytes, o.qwen_unique_bytes);

    let detail = |item: &CleanableItem, label: &str| {
        item.details
            .iter()
            .find(|d| d.label == label)
            .map(|d| d.value.clone())
            .unwrap_or_else(|| panic!("no {label} detail"))
    };
    assert!(detail(llama, "Shared layers").starts_with("1 "));
    assert_eq!(detail(llama, "Parameters"), "1.2B");
    assert_eq!(detail(llama, "Quantization"), "Q8_0");
    assert_eq!(detail(qwen, "Family"), "qwen2");

    // The action is `ollama rm`, emitted but never run here.
    let action = &llama.available_actions[0];
    assert_eq!(action.estimated_savings_bytes, o.llama_unique_bytes);
    match &action.method {
        ActionMethod::Command { program, args, .. } => {
            assert_eq!(program, "ollama");
            assert_eq!(args, &["rm".to_string(), "llama3.2:1b".to_string()]);
        }
        other => panic!("expected a command, got {other:?}"),
    }
}

#[tokio::test]
async fn orphan_blob_cleanup_removes_only_old_unreferenced_blobs() {
    let env = setup();
    let items = scanner(&env).scan_all().await;
    let o = &env.fx.ollama;
    let orphans = one(&items, ArtifactKind::OllamaOrphanBlobs);
    assert_eq!(orphans.risk, RiskLevel::Safe);

    let expected: u64 = o
        .old_orphans
        .iter()
        .map(|p| std::fs::metadata(p).unwrap().len())
        .sum();
    assert_eq!(orphans.size_bytes, expected);

    let action = &orphans.available_actions[0];
    match &action.method {
        ActionMethod::RemoveOllamaOrphans { store, blobs } => {
            assert_eq!(store, &o.root);
            let mut got = blobs.clone();
            got.sort();
            let mut want = o.old_orphans.clone();
            want.sort();
            assert_eq!(got, want);
        }
        other => panic!("expected RemoveOllamaOrphans, got {other:?}"),
    }

    let (ok, freed, err) = run(&env, orphans, action).await;
    assert!(ok, "orphan cleanup failed: {err:?}");
    assert_eq!(freed, expected);

    for p in &o.old_orphans {
        assert!(!p.exists(), "{} should be gone", p.display());
    }
    for p in &o.fresh_orphans {
        assert!(p.exists(), "fresh {} must survive", p.display());
    }
    for p in &o.referenced_blobs {
        assert!(p.exists(), "referenced {} must survive", p.display());
    }

    // A rescan: every manifest still resolves, and the only leftovers are
    // the fresh files.
    let after = scanner(&env).scan_all().await;
    assert!(after
        .iter()
        .all(|i| i.kind != ArtifactKind::OllamaOrphanBlobs));
    assert_eq!(
        named(&after, "llama3.2:1b").size_bytes,
        o.llama_unique_bytes
    );
}

#[tokio::test]
async fn hf_old_revision_cleanup_keeps_the_live_revision_readable() {
    let env = setup();
    let hf = &env.fx.hf;
    let live_files = [
        "model.safetensors",
        "config.json",
        "tokenizer/tokenizer.json",
    ];
    let before: Vec<Vec<u8>> = live_files
        .iter()
        .map(|f| read_through(&hf.live_snapshot.join(f)))
        .collect();

    let items = scanner(&env).scan_all().await;
    let repo = named(&items, "acme/tiny-llm");
    assert_eq!(repo.kind, ArtifactKind::HuggingFaceModel);
    assert_eq!(repo.path, hf.repo);

    let trim = &repo.available_actions[0];
    assert_eq!(trim.risk, RiskLevel::Safe);
    let expected = std::fs::metadata(&hf.detached_only_blob).unwrap().len()
        + std::fs::metadata(&hf.incomplete_blob).unwrap().len();
    assert_eq!(trim.estimated_savings_bytes, expected);
    match &trim.method {
        ActionMethod::RemoveFiles { paths } => {
            assert!(paths.contains(&hf.detached_only_blob));
            assert!(paths.contains(&hf.incomplete_blob));
            assert!(!paths.contains(&hf.shared_blob), "shared blob listed");
            assert!(!paths.contains(&hf.live_weights_blob), "live blob listed");
        }
        other => panic!("expected RemoveFiles, got {other:?}"),
    }
    // The whole-repo delete is there too, as the riskier option.
    let delete = repo.available_actions.last().unwrap();
    assert_eq!(delete.risk, RiskLevel::Caution);
    assert!(matches!(&delete.method, ActionMethod::RemoveDir { path } if path == &hf.repo));

    let (ok, _, err) = run(&env, repo, trim).await;
    assert!(ok, "trim failed: {err:?}");

    assert!(!hf.detached_only_blob.exists());
    assert!(!hf.incomplete_blob.exists());
    assert!(hf.shared_blob.exists());
    for (f, content) in live_files.iter().zip(before) {
        assert_eq!(
            read_through(&hf.live_snapshot.join(f)),
            content,
            "live {f} changed"
        );
    }
}

#[tokio::test]
async fn dedup_makes_copies_share_storage_and_all_stay_readable() {
    let env = setup();
    let items = scanner(&env).scan_all().await;
    let dup = one(&items, ArtifactKind::DuplicateModelFiles);
    assert_eq!(dup.risk, RiskLevel::Safe);

    let copies = [
        env.fx.ollama.llama_model_blob.clone(),
        env.fx.hf.live_weights_blob.clone(),
        env.fx.lmstudio_gguf.clone(),
    ];
    let weights = models::shared_weights();
    let action = &dup.available_actions[0];
    match &action.method {
        ActionMethod::DedupFiles { groups } => {
            assert_eq!(groups.len(), 1, "only the shared weights are duplicated");
            let g = &groups[0];
            let mut members = vec![g.keep.clone()];
            members.extend(g.duplicates.iter().cloned());
            members.sort();
            let mut want = copies.to_vec();
            want.sort();
            assert_eq!(members, want);
            assert_eq!(g.size_bytes, weights.len() as u64);
        }
        other => panic!("expected DedupFiles, got {other:?}"),
    }
    assert_eq!(action.estimated_savings_bytes, 2 * weights.len() as u64);

    let (ok, saved, err) = run(&env, dup, action).await;
    assert!(ok, "dedup failed: {err:?}");
    assert_eq!(saved, 2 * weights.len() as u64);

    for p in &copies {
        assert_eq!(read_through(p), weights, "{} changed", p.display());
    }
    // And through the Hugging Face snapshot symlink.
    assert_eq!(
        read_through(&env.fx.hf.live_snapshot.join("model.safetensors")),
        weights
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let inodes: Vec<u64> = copies
            .iter()
            .map(|p| std::fs::metadata(p).unwrap().ino())
            .collect();
        // Hardlinks share an inode; clones do not but share extents. Either
        // way the content is identical (checked above) and no temp file is
        // left behind.
        if inodes.windows(2).all(|w| w[0] == w[1]) {
            assert!(std::fs::metadata(&copies[0]).unwrap().nlink() >= 3);
        }
    }
    for p in &copies {
        let tmp = PathBuf::from(format!("{}.void-dedup-tmp", p.display()));
        assert!(!tmp.exists());
    }
}

#[tokio::test]
async fn tampered_duplicate_is_not_replaced() {
    let env = setup();
    let items = scanner(&env).scan_all().await;
    let dup = one(&items, ArtifactKind::DuplicateModelFiles);
    let action = dup.available_actions[0].clone();
    let ActionMethod::DedupFiles { groups } = &action.method else {
        panic!("expected DedupFiles");
    };

    // Every duplicate changes after the scan: nothing may be replaced.
    let mut tampered = models::shared_weights();
    tampered[100] ^= 0xff;
    for d in &groups[0].duplicates {
        std::fs::write(d, &tampered).unwrap();
    }

    let (ok, _, err) = run(&env, dup, &action).await;
    assert!(!ok, "dedup must fail when every duplicate changed");
    assert!(err.unwrap().contains("changed"));
    for d in &groups[0].duplicates {
        assert_eq!(std::fs::read(d).unwrap(), tampered);
    }
    assert_eq!(
        std::fs::read(&groups[0].keep).unwrap(),
        models::shared_weights()
    );
}

#[tokio::test]
async fn torch_and_comfyui_actions_remove_only_their_folder() {
    let env = setup();
    let items = scanner(&env).scan_all().await;

    let torch = one(&items, ArtifactKind::TorchHubCache);
    assert_eq!(torch.risk, RiskLevel::Safe);
    let (ok, _, err) = run(&env, torch, &torch.available_actions[0]).await;
    assert!(ok, "{err:?}");
    assert!(!env.fx.torch_hub.exists());

    let loras = named(&items, "ComfyUI loras");
    assert_eq!(loras.risk, RiskLevel::Caution);
    let (ok, _, err) = run(&env, loras, &loras.available_actions[0]).await;
    assert!(ok, "{err:?}");
    assert!(!env.fx.comfyui.join("loras").exists());
    assert!(env
        .fx
        .comfyui
        .join("checkpoints/sd15-pruned.safetensors")
        .exists());
}

#[tokio::test]
async fn walk_claims_a_comfyui_install_outside_home_layout() {
    let env = setup();
    let elsewhere = env
        .home
        .dir("code/comfy-experiments/ComfyUI/models/checkpoints");
    std::fs::write(elsewhere.join("x.safetensors"), models::shared_weights()).unwrap();
    let models_dir = elsewhere.parent().unwrap().to_path_buf();

    let s = scanner(&env);
    assert!(s.is_candidate("models", &models_dir));
    assert!(!s.is_candidate("models", &env.home.dir("some/app/models")));

    let items = s.analyze_many(&models_dir).await.unwrap();
    assert!(items.iter().any(|i| i.kind == ArtifactKind::ComfyUiModels));

    // The walked install joins the duplicate search: four copies now.
    let dup = s.duplicate_models().await.unwrap();
    let ActionMethod::DedupFiles { groups } = &dup.available_actions[0].method else {
        panic!("expected DedupFiles");
    };
    assert_eq!(groups[0].duplicates.len(), 3);
}

#[tokio::test]
async fn empty_home_finds_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let s = ModelScanner::new(home.root().to_path_buf(), &AppConfig::default());
    assert!(s.global_locations().is_empty());
    assert!(s.scan_all().await.is_empty());
}

#[tokio::test]
async fn orchestrator_reports_each_model_once() {
    use deepclean_core::scanner::{registry, ScanOrchestrator};

    let env = setup();
    let config = AppConfig {
        scan_roots: vec![env.home.root().to_path_buf()],
        enabled_ecosystems: vec![Ecosystem::Models],
        walker_threads: 1,
        ..Default::default()
    };
    let scanners = registry::build_with_home(&config, env.home.root().to_path_buf());
    let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
    let mut items = Vec::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            ScanEvent::ItemFound { item } => items.push(item),
            ScanEvent::ScanComplete { .. } => break,
            _ => {}
        }
    }
    // The walk over home claims the store roots and the globals are not
    // analyzed a second time.
    let llama = items
        .iter()
        .filter(|i| i.project_name.as_deref() == Some("llama3.2:1b"))
        .count();
    assert_eq!(llama, 1);
    assert_eq!(
        items
            .iter()
            .filter(|i| i.kind == ArtifactKind::HuggingFaceModel)
            .count(),
        2
    );
    assert_eq!(
        items
            .iter()
            .filter(|i| i.kind == ArtifactKind::OllamaOrphanBlobs)
            .count(),
        1
    );
}

#[tokio::test]
async fn orphan_blob_referenced_after_the_scan_survives_the_clean() {
    // A pull that starts between scan and clean makes an "orphan" live again.
    // The executor re-reads the manifests and must keep it.
    let env = setup();
    let items = scanner(&env).scan_all().await;
    let o = &env.fx.ollama;
    let orphans = one(&items, ArtifactKind::OllamaOrphanBlobs);
    let revived = o.old_orphans[0].clone();
    let digest = revived
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replacen("sha256-", "sha256:", 1);
    let manifest = o
        .root
        .join("manifests/registry.ollama.ai/library/revived/latest");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        &manifest,
        format!(r#"{{"layers":[{{"digest":"{digest}","mediaType":"application/vnd.ollama.image.model"}}]}}"#),
    )
    .unwrap();

    let (ok, _, err) = run(&env, orphans, &orphans.available_actions[0]).await;
    assert!(ok, "cleanup failed: {err:?}");
    assert!(
        revived.exists(),
        "a blob referenced after the scan was deleted"
    );
    for p in &o.old_orphans[1..] {
        assert!(!p.exists(), "{} should be gone", p.display());
    }
}

#[tokio::test]
async fn orphan_cleanup_is_refused_when_a_manifest_becomes_unreadable() {
    let env = setup();
    let items = scanner(&env).scan_all().await;
    let o = &env.fx.ollama;
    let orphans = one(&items, ArtifactKind::OllamaOrphanBlobs);
    let broken = o
        .root
        .join("manifests/registry.ollama.ai/library/broken/latest");
    std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
    std::fs::write(&broken, "{ not json").unwrap();

    let (ok, _, _) = run(&env, orphans, &orphans.available_actions[0]).await;
    assert!(!ok, "cleanup must refuse when references cannot be proven");
    for p in &o.old_orphans {
        assert!(p.exists(), "{} must survive", p.display());
    }
}
