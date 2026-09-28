//! Fixtures for the models feature: a fake Ollama store, Hugging Face cache,
//! LM Studio library, torch hub and ComfyUI install.
//!
//! One weights file ([`shared_weights`]) is written byte-for-byte into all
//! three of Ollama, Hugging Face and LM Studio, the way a user who tried the
//! same model in three tools ends up with it — that is what the duplicate
//! detector must find. Everything else has distinct content so it is never
//! mistaken for a duplicate.

use std::path::{Path, PathBuf};

use super::{age, FakeHome};

/// Content of the model file duplicated across stores (8 KiB).
pub fn shared_weights() -> Vec<u8> {
    pattern(b"GGUF shared weights ", 8192)
}

/// Deterministic filler: `seed` repeated with a running counter, so every
/// distinct seed yields distinct bytes.
fn pattern(seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut i: u32 = 0;
    while out.len() < len {
        out.extend_from_slice(seed);
        out.extend_from_slice(&i.to_le_bytes());
        i += 1;
    }
    out.truncate(len);
    out
}

/// A fake digest: blake3 of the content, hex — the right shape for a sha256
/// name, which is all the scanner looks at.
fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// What [`ollama_store`] built.
#[derive(Debug, Clone)]
pub struct OllamaFixture {
    /// `~/.ollama/models`.
    pub root: PathBuf,
    /// `llama3.2:1b`: model layer (the shared weights) + config, and the
    /// shared template layer.
    pub llama_manifest: PathBuf,
    /// `qwen2.5:7b`: its own model layer + config, and the shared template.
    pub qwen_manifest: PathBuf,
    /// Bytes only llama references (model + config blobs).
    pub llama_unique_bytes: u64,
    /// Bytes only qwen references.
    pub qwen_unique_bytes: u64,
    /// The llama model layer blob (content = [`shared_weights`]).
    pub llama_model_blob: PathBuf,
    /// The template layer both models reference.
    pub shared_blob: PathBuf,
    /// Every blob some manifest references.
    pub referenced_blobs: Vec<PathBuf>,
    /// Unreferenced blobs two days old: an old orphan and an abandoned partial.
    pub old_orphans: Vec<PathBuf>,
    /// Unreferenced but fresh: a pull in progress. Must survive.
    pub fresh_orphans: Vec<PathBuf>,
}

/// Write a blob into `blobs/` and return (path, size).
fn ollama_blob(home: &FakeHome, root_rel: &str, bytes: &[u8]) -> (PathBuf, String) {
    let hex = digest(bytes);
    let p = home.file(format!("{root_rel}/blobs/sha256-{hex}"), bytes);
    (p, format!("sha256:{hex}"))
}

/// A two-model Ollama store with one shared layer, two old orphans and a
/// fresh in-progress partial.
pub fn ollama_store(home: &FakeHome) -> OllamaFixture {
    let rel = ".ollama/models";
    let root = home.dir(rel);

    let template = pattern(b"{{ .Prompt }} template ", 300);
    let (shared_blob, template_digest) = ollama_blob(home, rel, &template);

    let llama_weights = shared_weights();
    let (llama_model_blob, llama_model_digest) = ollama_blob(home, rel, &llama_weights);
    let llama_config =
        br#"{"model_format":"gguf","model_family":"llama","model_type":"1.2B","file_type":"Q8_0"}"#;
    let (llama_config_blob, llama_config_digest) = ollama_blob(home, rel, llama_config);

    let qwen_weights = pattern(b"qwen weights ", 4096);
    let (qwen_model_blob, qwen_model_digest) = ollama_blob(home, rel, &qwen_weights);
    let qwen_config = br#"{"model_format":"gguf","model_family":"qwen2","model_type":"7.6B","file_type":"Q4_K_M"}"#;
    let (qwen_config_blob, qwen_config_digest) = ollama_blob(home, rel, qwen_config);

    let manifest = |config: &str, config_len: usize, model: &str, model_len: usize| {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"{config}","size":{config_len}}},"layers":[{{"mediaType":"application/vnd.ollama.image.model","digest":"{model}","size":{model_len}}},{{"mediaType":"application/vnd.ollama.image.template","digest":"{template_digest}","size":{}}}]}}"#,
            template.len()
        )
    };

    let llama_manifest = home.file(
        format!("{rel}/manifests/registry.ollama.ai/library/llama3.2/1b"),
        manifest(
            &llama_config_digest,
            llama_config.len(),
            &llama_model_digest,
            llama_weights.len(),
        ),
    );
    let qwen_manifest = home.file(
        format!("{rel}/manifests/registry.ollama.ai/library/qwen2.5/7b"),
        manifest(
            &qwen_config_digest,
            qwen_config.len(),
            &qwen_model_digest,
            qwen_weights.len(),
        ),
    );

    let referenced_blobs = vec![
        shared_blob.clone(),
        llama_model_blob.clone(),
        llama_config_blob.clone(),
        qwen_model_blob.clone(),
        qwen_config_blob.clone(),
    ];
    // Old enough that only the manifest reference protects them.
    for blob in &referenced_blobs {
        age(blob, 2);
    }

    let (old_orphan, _) = ollama_blob(home, rel, &pattern(b"deleted model ", 3000));
    age(&old_orphan, 2);
    let abandoned_hex = digest(b"abandoned pull");
    let abandoned_partial = home.file(
        format!("{rel}/blobs/sha256-{abandoned_hex}-partial-0"),
        pattern(b"abandoned pull ", 1500),
    );
    age(&abandoned_partial, 2);

    let active_hex = digest(b"active pull");
    let fresh_partial = home.file(
        format!("{rel}/blobs/sha256-{active_hex}-partial"),
        pattern(b"active pull ", 1200),
    );
    let (fresh_blob, _) = ollama_blob(home, rel, &pattern(b"just finished pull ", 900));

    OllamaFixture {
        root,
        llama_manifest,
        qwen_manifest,
        llama_unique_bytes: (llama_weights.len() + llama_config.len()) as u64,
        qwen_unique_bytes: (qwen_weights.len() + qwen_config.len()) as u64,
        llama_model_blob,
        shared_blob,
        referenced_blobs,
        old_orphans: vec![old_orphan, abandoned_partial],
        fresh_orphans: vec![fresh_partial, fresh_blob],
    }
}

/// What [`huggingface_cache`] built.
#[derive(Debug, Clone)]
pub struct HfFixture {
    /// `~/.cache/huggingface/hub`.
    pub root: PathBuf,
    /// `models--acme--tiny-llm`.
    pub repo: PathBuf,
    /// The revision `refs/main` points at.
    pub live_snapshot: PathBuf,
    /// A revision no ref points at any more.
    pub detached_snapshot: PathBuf,
    /// Blob of the live `model.safetensors` (content = [`shared_weights`]).
    pub live_weights_blob: PathBuf,
    /// Blob used by both revisions — must survive detached cleanup.
    pub shared_blob: PathBuf,
    /// Blob only the detached revision uses.
    pub detached_only_blob: PathBuf,
    /// A failed download two days old.
    pub incomplete_blob: PathBuf,
    /// A dataset repo with one live revision.
    pub dataset: PathBuf,
}

/// Point `link` at `target` (a relative path, as huggingface_hub writes
/// them). Where symlinks are unavailable, a copy — which is what
/// huggingface_hub itself falls back to on Windows.
fn hf_link(link: &Path, target_rel: &str) {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).expect("create snapshot dir");
    }
    // `void dev seed` may run twice over the same directory.
    let _ = std::fs::remove_file(link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(target_rel, link).expect("symlink snapshot file");
    #[cfg(not(unix))]
    {
        let base = link.parent().expect("snapshot parent");
        std::fs::copy(base.join(target_rel), link).expect("copy snapshot file");
    }
}

/// A Hugging Face hub cache with one model repo (a live and a detached
/// revision, symlinked snapshots, a stale `.incomplete`) and one dataset.
pub fn huggingface_cache(home: &FakeHome) -> HfFixture {
    let root = home.dir(".cache/huggingface/hub");
    let repo = home.dir(".cache/huggingface/hub/models--acme--tiny-llm");

    let blob = |bytes: &[u8]| -> (PathBuf, String) {
        let hex = digest(bytes);
        let p = repo.join("blobs").join(&hex);
        std::fs::create_dir_all(repo.join("blobs")).expect("blobs dir");
        std::fs::write(&p, bytes).expect("write blob");
        (p, hex)
    };

    let (live_weights_blob, live_weights) = blob(&shared_weights());
    let (_, live_config) = blob(br#"{"architectures":["TinyLlm"],"rev":"new"}"#);
    let (shared_blob, tokenizer) = blob(&pattern(b"tokenizer vocab ", 1024));
    let (detached_only_blob, old_weights) = blob(&pattern(b"old revision weights ", 2048));

    let incomplete_blob = repo
        .join("blobs")
        .join(format!("{}.incomplete", digest(b"interrupted")));
    std::fs::write(&incomplete_blob, pattern(b"interrupted ", 700)).expect("incomplete");
    age(&incomplete_blob, 2);

    let live_rev = "1111111111111111111111111111111111111111";
    let old_rev = "0000000000000000000000000000000000000000";
    std::fs::create_dir_all(repo.join("refs")).expect("refs");
    std::fs::write(repo.join("refs/main"), live_rev).expect("ref");

    let live_snapshot = repo.join("snapshots").join(live_rev);
    hf_link(
        &live_snapshot.join("model.safetensors"),
        &format!("../../blobs/{live_weights}"),
    );
    hf_link(
        &live_snapshot.join("config.json"),
        &format!("../../blobs/{live_config}"),
    );
    hf_link(
        &live_snapshot.join("tokenizer/tokenizer.json"),
        &format!("../../../blobs/{tokenizer}"),
    );

    let detached_snapshot = repo.join("snapshots").join(old_rev);
    hf_link(
        &detached_snapshot.join("model.safetensors"),
        &format!("../../blobs/{old_weights}"),
    );
    hf_link(
        &detached_snapshot.join("tokenizer/tokenizer.json"),
        &format!("../../../blobs/{tokenizer}"),
    );

    let dataset = home.dir(".cache/huggingface/hub/datasets--acme--evals");
    let data = pattern(b"eval rows ", 1500);
    let data_hex = digest(&data);
    home.file(
        format!(".cache/huggingface/hub/datasets--acme--evals/blobs/{data_hex}"),
        &data,
    );
    let ds_rev = "2222222222222222222222222222222222222222";
    home.file(
        ".cache/huggingface/hub/datasets--acme--evals/refs/main",
        ds_rev,
    );
    hf_link(
        &dataset.join("snapshots").join(ds_rev).join("data.parquet"),
        &format!("../../blobs/{data_hex}"),
    );

    HfFixture {
        root,
        repo,
        live_snapshot,
        detached_snapshot,
        live_weights_blob,
        shared_blob,
        detached_only_blob,
        incomplete_blob,
        dataset,
    }
}

/// An LM Studio library holding the shared weights as a `.gguf`. Returns the
/// gguf path.
pub fn lmstudio_models(home: &FakeHome) -> PathBuf {
    home.file(
        ".lmstudio/models/lmstudio-community/tiny-llm-GGUF/tiny-llm-Q8_0.gguf",
        shared_weights(),
    )
}

/// A torch hub cache with one checkpoint. Returns the hub dir.
pub fn torch_hub(home: &FakeHome) -> PathBuf {
    home.file(
        ".cache/torch/hub/checkpoints/resnet18-f37072fd.pth",
        pattern(b"resnet ", 2048),
    );
    home.path(".cache/torch/hub")
}

/// A ComfyUI install with checkpoints and loras. Returns its `models` dir.
pub fn comfyui_models(home: &FakeHome) -> PathBuf {
    home.file(
        "ComfyUI/models/checkpoints/sd15-pruned.safetensors",
        pattern(b"sd15 ", 3072),
    );
    home.file(
        "ComfyUI/models/loras/my-style.safetensors",
        pattern(b"my style lora ", 1024),
    );
    home.file("ComfyUI/models/vae/put_vae_here", "");
    home.path("ComfyUI/models")
}

/// Everything above, in one sandbox.
#[derive(Debug, Clone)]
pub struct ModelsFixture {
    pub ollama: OllamaFixture,
    pub hf: HfFixture,
    pub lmstudio_gguf: PathBuf,
    pub torch_hub: PathBuf,
    pub comfyui: PathBuf,
}

/// Build every model store into `home`.
pub fn all(home: &FakeHome) -> ModelsFixture {
    ModelsFixture {
        ollama: ollama_store(home),
        hf: huggingface_cache(home),
        lmstudio_gguf: lmstudio_models(home),
        torch_hub: torch_hub(home),
        comfyui: comfyui_models(home),
    }
}

/// Seed a realistic models layout into `home` for `void dev seed`.
pub fn seed(home: &FakeHome) {
    let _ = all(home);
}
