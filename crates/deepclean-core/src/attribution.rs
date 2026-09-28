//! Usage by AI tool: "Claude used 38 GB this month: worktrees 30,
//! transcripts 6, Playwright 2."

use std::collections::HashMap;

use bytesize::ByteSize;
use serde::{Deserialize, Serialize};

use crate::model::{ArtifactKind, CleanableItem};

/// Space attributed to one AI tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUsage {
    /// Stable id: "claude", "cursor", "codex", "conductor", "ollama", …
    pub agent: String,
    /// Human name: "Claude Code", "Cursor", …
    pub display_name: String,
    pub total_bytes: u64,
    pub item_count: usize,
    /// Largest first.
    pub categories: Vec<CategoryUsage>,
}

/// One slice of an agent's usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryUsage {
    /// "Worktrees", "Transcripts", "Browsers", "Models", …
    pub label: String,
    pub bytes: u64,
    pub count: usize,
}

/// Human name for an agent id. Unknown ids are title-cased so a scanner
/// adding a new tool still reads sensibly before this table learns it.
pub fn display_name(agent: &str) -> String {
    match agent {
        "claude" => "Claude Code".into(),
        "cursor" => "Cursor".into(),
        "codex" => "Codex".into(),
        "conductor" => "Conductor".into(),
        "windsurf" => "Windsurf".into(),
        "vscode" => "VS Code".into(),
        "ollama" => "Ollama".into(),
        "huggingface" => "Hugging Face".into(),
        "lmstudio" => "LM Studio".into(),
        "comfyui" => "ComfyUI".into(),
        "torch" => "PyTorch".into(),
        "playwright" => "Playwright".into(),
        other => title_case(other),
    }
}

fn title_case(id: &str) -> String {
    id.split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The slice of an agent's usage an item falls into.
pub fn category_label(item: &CleanableItem) -> String {
    use ArtifactKind::*;
    match item.kind {
        AgentWorktree | OrphanWorktree | PrunableWorktreeRefs | MergedAgentBranches => {
            "Worktrees".into()
        }
        AgentTranscripts => "Transcripts".into(),
        AgentFileHistory => "File history".into(),
        AgentDebugLogs => "Debug logs".into(),
        AgentCache => "Caches".into(),
        EditorStateDb | EditorWorkspaceStorage => "Editor state".into(),
        OllamaModel | OllamaOrphanBlobs | HuggingFaceModel | LmStudioModel | TorchHubCache
        | ComfyUiModels => "Models".into(),
        DuplicateModelFiles => "Duplicates".into(),
        PlaywrightBrowsers | PuppeteerBrowsers => "Browsers".into(),
        _ => item.ecosystem.display_name().into(),
    }
}

/// Running totals for one agent: bytes, items, and per-category (bytes, items).
type Tally = (u64, usize, HashMap<String, (u64, usize)>);

/// Group `items` by their `agent`, largest agent first. Items nested inside
/// another item are not double-counted.
///
/// Nesting is judged across all attributed items, not per agent: bytes on
/// disk are counted once, by the outermost item that holds them.
pub fn by_agent(items: &[CleanableItem]) -> Vec<AgentUsage> {
    let mut attributed: Vec<&CleanableItem> = items.iter().filter(|i| i.agent.is_some()).collect();
    // Shallowest first, so an ancestor is always kept before its descendants
    // are considered; the stable sort keeps input order among equals.
    attributed.sort_by_key(|i| i.path.components().count());

    let mut kept: Vec<&CleanableItem> = Vec::new();
    for item in attributed {
        if kept.iter().any(|k| item.path.starts_with(&k.path)) {
            continue;
        }
        kept.push(item);
    }

    let mut agents: HashMap<String, Tally> = HashMap::new();
    for item in kept {
        let Some(agent) = item.agent.as_ref() else {
            continue;
        };
        let entry = agents.entry(agent.clone()).or_default();
        entry.0 += item.size_bytes;
        entry.1 += 1;
        let cat = entry.2.entry(category_label(item)).or_default();
        cat.0 += item.size_bytes;
        cat.1 += 1;
    }

    let mut out: Vec<AgentUsage> = agents
        .into_iter()
        .map(|(agent, (total_bytes, item_count, cats))| {
            let mut categories: Vec<CategoryUsage> = cats
                .into_iter()
                .map(|(label, (bytes, count))| CategoryUsage {
                    label,
                    bytes,
                    count,
                })
                .collect();
            categories.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
            AgentUsage {
                display_name: display_name(&agent),
                agent,
                total_bytes,
                item_count,
                categories,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.total_bytes
            .cmp(&a.total_bytes)
            .then_with(|| a.agent.cmp(&b.agent))
    });
    out
}

/// "Claude Code: 38.2 GB — Worktrees 30.1 GB, Transcripts 6.0 GB, Browsers 2.1 GB"
pub fn summary_line(usage: &AgentUsage) -> String {
    let head = format!("{}: {}", usage.display_name, ByteSize(usage.total_bytes));
    if usage.categories.is_empty() {
        return head;
    }
    let parts: Vec<String> = usage
        .categories
        .iter()
        .map(|c| format!("{} {}", c.label, ByteSize(c.bytes)))
        .collect();
    format!("{head} — {}", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ecosystem, RiskLevel};
    use uuid::Uuid;

    fn item(
        path: &str,
        kind: ArtifactKind,
        eco: Ecosystem,
        agent: Option<&str>,
        size: u64,
    ) -> CleanableItem {
        CleanableItem {
            id: Uuid::new_v4(),
            path: path.into(),
            ecosystem: eco,
            kind,
            risk: RiskLevel::Safe,
            size_bytes: size,
            size_display: String::new(),
            last_modified: None,
            days_stale: None,
            project_name: None,
            project_root: None,
            available_actions: Vec::new(),
            details: Vec::new(),
            agent: agent.map(str::to_string),
        }
    }

    #[test]
    fn names() {
        assert_eq!(display_name("claude"), "Claude Code");
        assert_eq!(display_name("huggingface"), "Hugging Face");
        assert_eq!(display_name("lmstudio"), "LM Studio");
        assert_eq!(display_name("torch"), "PyTorch");
        assert_eq!(display_name("vscode"), "VS Code");
        assert_eq!(display_name("zed"), "Zed");
        assert_eq!(display_name("gemini-cli"), "Gemini Cli");
    }

    #[test]
    fn categories_from_kinds() {
        let c = |k, e| category_label(&item("/x", k, e, None, 0));
        assert_eq!(
            c(ArtifactKind::MergedAgentBranches, Ecosystem::Worktrees),
            "Worktrees"
        );
        assert_eq!(
            c(ArtifactKind::AgentFileHistory, Ecosystem::AgentData),
            "File history"
        );
        assert_eq!(
            c(ArtifactKind::EditorStateDb, Ecosystem::AgentData),
            "Editor state"
        );
        assert_eq!(
            c(ArtifactKind::OllamaOrphanBlobs, Ecosystem::Models),
            "Models"
        );
        assert_eq!(
            c(ArtifactKind::DuplicateModelFiles, Ecosystem::Models),
            "Duplicates"
        );
        assert_eq!(
            c(ArtifactKind::PuppeteerBrowsers, Ecosystem::Node),
            "Browsers"
        );
        assert_eq!(c(ArtifactKind::NodeModules, Ecosystem::Node), "Node.js");
    }

    #[test]
    fn unattributed_items_are_skipped() {
        let items = vec![item(
            "/a",
            ArtifactKind::NodeModules,
            Ecosystem::Node,
            None,
            100,
        )];
        assert!(by_agent(&items).is_empty());
    }

    #[test]
    fn groups_sorts_and_dedupes_nesting() {
        let items = vec![
            item(
                "/h/.claude/projects/x",
                ArtifactKind::AgentTranscripts,
                Ecosystem::AgentData,
                Some("claude"),
                60,
            ),
            item(
                "/r/.claude/worktrees/a",
                ArtifactKind::AgentWorktree,
                Ecosystem::Worktrees,
                Some("claude"),
                300,
            ),
            // Inside the worktree above: already counted there.
            item(
                "/r/.claude/worktrees/a/node_modules",
                ArtifactKind::NodeModules,
                Ecosystem::Node,
                Some("claude"),
                200,
            ),
            item(
                "/h/.cursor/worktrees/b",
                ArtifactKind::AgentWorktree,
                Ecosystem::Worktrees,
                Some("cursor"),
                500,
            ),
            item(
                "/h/.cache/ms-playwright",
                ArtifactKind::PlaywrightBrowsers,
                Ecosystem::Node,
                Some("claude"),
                21,
            ),
        ];
        let usage = by_agent(&items);
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].agent, "cursor");
        assert_eq!(usage[1].agent, "claude");
        assert_eq!(usage[1].total_bytes, 381);
        assert_eq!(usage[1].item_count, 3);
        let labels: Vec<_> = usage[1]
            .categories
            .iter()
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Worktrees", "Transcripts", "Browsers"]);
    }

    #[test]
    fn sibling_with_shared_prefix_is_not_nested() {
        let items = vec![
            item(
                "/m/model",
                ArtifactKind::OllamaModel,
                Ecosystem::Models,
                Some("ollama"),
                1,
            ),
            item(
                "/m/model-2",
                ArtifactKind::OllamaModel,
                Ecosystem::Models,
                Some("ollama"),
                2,
            ),
        ];
        assert_eq!(by_agent(&items)[0].total_bytes, 3);
    }

    #[test]
    fn summary_line_lists_categories() {
        let u = AgentUsage {
            agent: "claude".into(),
            display_name: "Claude Code".into(),
            total_bytes: 3_000,
            item_count: 2,
            categories: vec![
                CategoryUsage {
                    label: "Worktrees".into(),
                    bytes: 2_000,
                    count: 1,
                },
                CategoryUsage {
                    label: "Transcripts".into(),
                    bytes: 1_000,
                    count: 1,
                },
            ],
        };
        let line = summary_line(&u);
        assert!(line.starts_with("Claude Code: "), "{line}");
        assert!(line.contains(" — Worktrees "), "{line}");
        assert!(line.contains(", Transcripts "), "{line}");
        let empty = AgentUsage {
            categories: vec![],
            ..u
        };
        assert!(!summary_line(&empty).contains('—'));
    }
}
