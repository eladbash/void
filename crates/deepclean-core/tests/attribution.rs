//! Usage-by-agent over a realistic mix of synthetic items.

use std::path::PathBuf;

use deepclean_core::attribution::{by_agent, summary_line};
use deepclean_core::model::{ArtifactKind, CleanableItem, Ecosystem, RiskLevel};
use uuid::Uuid;

const MB: u64 = 1 << 20;

fn item(
    path: &str,
    kind: ArtifactKind,
    eco: Ecosystem,
    agent: Option<&str>,
    size: u64,
) -> CleanableItem {
    CleanableItem {
        id: Uuid::new_v4(),
        path: PathBuf::from(path),
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

fn mix() -> Vec<CleanableItem> {
    use ArtifactKind::*;
    use Ecosystem::*;
    vec![
        item(
            "/r/app/.claude/worktrees/fix-login",
            AgentWorktree,
            Worktrees,
            Some("claude"),
            300 * MB,
        ),
        // Nested inside the worktree: the Node scanner also found it.
        item(
            "/r/app/.claude/worktrees/fix-login/node_modules",
            NodeModules,
            Node,
            Some("claude"),
            250 * MB,
        ),
        item(
            "/h/.claude/projects/-r-app",
            AgentTranscripts,
            AgentData,
            Some("claude"),
            60 * MB,
        ),
        item(
            "/h/.claude/file-history",
            AgentFileHistory,
            AgentData,
            Some("claude"),
            10 * MB,
        ),
        item(
            "/h/.cache/ms-playwright",
            PlaywrightBrowsers,
            Node,
            Some("playwright"),
            40 * MB,
        ),
        item(
            "/h/.cursor/worktrees/app/x",
            AgentWorktree,
            Worktrees,
            Some("cursor"),
            200 * MB,
        ),
        item(
            "/h/Library/Application Support/Cursor/User/globalStorage/state.vscdb",
            EditorStateDb,
            AgentData,
            Some("cursor"),
            5 * MB,
        ),
        item(
            "/h/.ollama/models/manifests/llama3",
            OllamaModel,
            Models,
            Some("ollama"),
            900 * MB,
        ),
        item(
            "/h/.ollama/models/blobs",
            OllamaOrphanBlobs,
            Models,
            Some("ollama"),
            100 * MB,
        ),
        // Unattributed: excluded entirely.
        item("/r/other/node_modules", NodeModules, Node, None, 999 * MB),
    ]
}

#[test]
fn agents_ranked_by_total_without_double_counting() {
    let usage = by_agent(&mix());
    let order: Vec<_> = usage.iter().map(|u| u.agent.as_str()).collect();
    assert_eq!(order, vec!["ollama", "claude", "cursor", "playwright"]);

    let claude = &usage[1];
    assert_eq!(claude.display_name, "Claude Code");
    assert_eq!(
        claude.total_bytes,
        370 * MB,
        "nested node_modules must not be added"
    );
    assert_eq!(claude.item_count, 3);
    let cats: Vec<_> = claude
        .categories
        .iter()
        .map(|c| (c.label.as_str(), c.bytes))
        .collect();
    assert_eq!(
        cats,
        vec![
            ("Worktrees", 300 * MB),
            ("Transcripts", 60 * MB),
            ("File history", 10 * MB)
        ]
    );

    let ollama = &usage[0];
    assert_eq!(ollama.display_name, "Ollama");
    assert_eq!(ollama.categories.len(), 1);
    assert_eq!(ollama.categories[0].label, "Models");
    assert_eq!(ollama.categories[0].count, 2);

    assert_eq!(usage[2].categories[1].label, "Editor state");
    assert_eq!(usage[3].categories[0].label, "Browsers");
}

#[test]
fn summary_line_reads_naturally() {
    let usage = by_agent(&mix());
    let line = summary_line(&usage[1]);
    assert!(line.starts_with("Claude Code: 370"), "{line}");
    assert!(line.contains("— Worktrees 300"), "{line}");
    assert!(line.ends_with("File history 10.0 MiB"), "{line}");
}

#[test]
fn empty_input_is_empty() {
    assert!(by_agent(&[]).is_empty());
}
