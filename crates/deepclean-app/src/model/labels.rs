//! Display names for ecosystems, artifact kinds and agents.
//!
//! The matches are exhaustive on purpose: adding an `Ecosystem` or
//! `ArtifactKind` variant to `deepclean-core` without a label here fails to
//! compile, instead of rendering a raw identifier in front of a user.

use deepclean_core::model::{ArtifactKind, CleanableItem, Ecosystem};

/// Ecosystems about AI tools, grouped ahead of the rest in Results.
pub const AI_ECOSYSTEMS: [Ecosystem; 3] = [
    Ecosystem::Worktrees,
    Ecosystem::AgentData,
    Ecosystem::Models,
];

pub fn eco_name(e: Ecosystem) -> &'static str {
    e.display_name()
}

pub fn is_ai(e: Ecosystem) -> bool {
    AI_ECOSYSTEMS.contains(&e)
}

/// The serde name (`jet_brains`, `agent_data`), used as a stable key.
pub fn eco_id(e: Ecosystem) -> &'static str {
    match e {
        Ecosystem::Rust => "rust",
        Ecosystem::Node => "node",
        Ecosystem::Apple => "apple",
        Ecosystem::Docker => "docker",
        Ecosystem::Go => "go",
        Ecosystem::System => "system",
        Ecosystem::Python => "python",
        Ecosystem::Java => "java",
        Ecosystem::Homebrew => "homebrew",
        Ecosystem::JetBrains => "jet_brains",
        Ecosystem::DotNet => "dot_net",
        Ecosystem::Worktrees => "worktrees",
        Ecosystem::AgentData => "agent_data",
        Ecosystem::Models => "models",
        Ecosystem::Projects => "projects",
    }
}

/// The serde name of a kind (`node_modules`), shown as "Kind" in the drawer.
pub fn kind_id(kind: ArtifactKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn kind_name(kind: ArtifactKind) -> &'static str {
    use ArtifactKind::*;
    match kind {
        TargetDir => "target/",
        CargoRegistry => "Cargo registry",
        CargoGitCheckouts => "Cargo git checkouts",
        NodeModules => "node_modules/",
        NpmCache => "npm cache",
        YarnCache => "Yarn cache",
        PnpmStore => "pnpm store",
        DerivedData => "DerivedData/",
        Archives => "Archives/",
        DeviceSupport => "Device support",
        CocoaPodsCache => "CocoaPods cache",
        SwiftPackageCache => "Swift package cache",
        DanglingImages => "Dangling images",
        UnusedImages => "Unused images",
        BuildCache => "Build cache",
        StoppedContainers => "Stopped containers",
        UnusedVolumes => "Unused volumes",
        GoBuildCache => "Go build cache",
        GoModCache => "Go module cache",
        GoTestCache => "Go test cache",
        DownloadsDir => "Downloads",
        TrashBin => "Trash",
        SystemLogs => "System logs",
        PipCache => "pip cache",
        PycacheDir => "__pycache__/",
        VenvDir => ".venv/",
        CondaCache => "Conda cache",
        GradleCache => "Gradle cache",
        MavenRepository => "Maven repository",
        GradleBuildDir => "build/",
        HomebrewCache => "Homebrew cache",
        JetBrainsCache => "JetBrains cache",
        NuGetCache => "NuGet cache",
        DotNetBinObj => "bin/ obj/",
        SimulatorDevices => "iOS Simulators",
        SimulatorCaches => "Simulator caches",
        SimulatorRuntimes => "Simulator runtimes",
        UvCache => "uv cache",
        PythonToolCache => "Python tool cache",
        BunCache => "Bun cache",
        FrameworkBuildCache => "Framework build cache",
        PlaywrightBrowsers => "Playwright browsers",
        PuppeteerBrowsers => "Puppeteer browsers",
        MavenTarget => "target/",
        ContainerVmDisk => "Container VM disk",
        DockerData => "Docker data",
        AgentWorktree => "Agent worktree",
        OrphanWorktree => "Orphaned worktree",
        PrunableWorktreeRefs => "Stale worktree records",
        MergedAgentBranches => "Merged agent branches",
        AgentTranscripts => "Agent transcripts",
        AgentFileHistory => "File-history snapshots",
        AgentDebugLogs => "Agent debug logs",
        AgentCache => "Agent cache",
        EditorStateDb => "Editor state database",
        EditorWorkspaceStorage => "Editor workspace storage",
        OllamaModel => "Ollama model",
        OllamaOrphanBlobs => "Orphaned Ollama blobs",
        HuggingFaceModel => "Hugging Face model",
        LmStudioModel => "LM Studio model",
        TorchHubCache => "Torch hub cache",
        ComfyUiModels => "ComfyUI models",
        DuplicateModelFiles => "Duplicate model files",
        StaleProject => "Stale project",
    }
}

fn last_segment(path: &str) -> Option<&str> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
}

/// A short label for what the artifact is, used as the row's primary name.
pub fn kind_label(item: &CleanableItem) -> String {
    // A Downloads entry is a specific file the user recognises by name; the
    // kind is the least useful thing to call it.
    if item.kind == ArtifactKind::DownloadsDir {
        let path = item.path.to_string_lossy();
        return last_segment(&path).unwrap_or("Downloads").to_string();
    }
    kind_name(item.kind).to_string()
}

/// Secondary label, suppressed when it merely restates the kind. Several
/// scanners set `project_name` to a synthetic label like "Trash (412 items)".
pub fn project_label(item: &CleanableItem) -> Option<String> {
    let name = item.project_name.as_deref()?;
    let kind = kind_label(item);
    if name == kind || name.starts_with(&kind) {
        return None;
    }
    Some(name.to_string())
}

/// Human names for `item.agent` ids. Unknown ids are title-cased.
pub fn agent_name(id: &str) -> String {
    let known = match id {
        "claude" => "Claude Code",
        "cursor" => "Cursor",
        "codex" => "Codex",
        "conductor" => "Conductor",
        "windsurf" => "Windsurf",
        "copilot" => "GitHub Copilot",
        "gemini" => "Gemini CLI",
        "aider" => "Aider",
        "zed" => "Zed",
        "vscode" => "VS Code",
        "ollama" => "Ollama",
        "lmstudio" => "LM Studio",
        "huggingface" => "Hugging Face",
        "comfyui" => "ComfyUI",
        "playwright" => "Playwright",
        "puppeteer" => "Puppeteer",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    id.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::item;

    #[test]
    fn ai_ecosystems_lead_the_display_order() {
        assert_eq!(&Ecosystem::ALL[..3], &AI_ECOSYSTEMS);
    }

    #[test]
    fn eco_ids_match_serde() {
        for e in Ecosystem::ALL {
            assert_eq!(serde_json::to_value(e).unwrap(), eco_id(e), "{e:?}");
        }
    }

    #[test]
    fn agent_names_are_human_unknown_ids_are_title_cased() {
        assert_eq!(agent_name("claude"), "Claude Code");
        assert_eq!(agent_name("lmstudio"), "LM Studio");
        assert_eq!(agent_name("some-new_tool"), "Some New Tool");
    }

    #[test]
    fn kind_and_project_labels() {
        let mut i = item();
        i.kind = ArtifactKind::AgentWorktree;
        assert_eq!(kind_label(&i), "Agent worktree");

        i.kind = ArtifactKind::DownloadsDir;
        i.path = "/Users/dev/Downloads/xcode.xip".into();
        assert_eq!(kind_label(&i), "xcode.xip");

        i.kind = ArtifactKind::HomebrewCache;
        i.project_name = Some("Homebrew cache".into());
        assert_eq!(project_label(&i), None);

        i.kind = ArtifactKind::OllamaModel;
        i.project_name = Some("llama3:8b".into());
        assert_eq!(project_label(&i).as_deref(), Some("llama3:8b"));
        assert_eq!(kind_id(ArtifactKind::NuGetCache), "nu_get_cache");
    }
}
