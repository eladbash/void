//! Fixtures for the agents feature: fake `~/.claude`, `~/.codex` and
//! Cursor-family `User/` directories.
//!
//! Each builder returns the paths tests assert on — what should be offered
//! and what must survive — so a test never has to re-derive the layout.

use std::path::{Path, PathBuf};

use super::{age, age_tree, FakeHome};

/// Old enough to be past the default 30-day retention.
pub const OLD_DAYS: u64 = 60;
/// Well inside the retention window.
pub const RECENT_DAYS: u64 = 2;

pub const OLD_SESSION: &str = "11111111-1111-4111-8111-111111111111";
pub const RECENT_SESSION: &str = "22222222-2222-4222-8222-222222222222";
/// A session whose main transcript Claude Code already deleted, leaving the
/// subagent directory behind (the known cleanup gap).
pub const ORPHAN_SIDECAR: &str = "33333333-3333-4333-8333-333333333333";
pub const BIG_SESSION: &str = "44444444-4444-4444-8444-444444444444";

/// Claude Code's encoding of a project path as a directory name.
pub fn encode_project(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '.' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// Paths inside a fake `~/.claude`.
#[derive(Debug, Clone)]
pub struct ClaudeFixture {
    pub root: PathBuf,
    /// The project directory under `projects/`.
    pub project_dir: PathBuf,
    /// The real project folder the transcripts belong to.
    pub project_path: PathBuf,
    pub old_transcripts: Vec<PathBuf>,
    pub recent_transcripts: Vec<PathBuf>,
    /// `<uuid>/` dirs that should be offered (old session, orphaned).
    pub old_sidecars: Vec<PathBuf>,
    /// `<uuid>/` dir of the recent session — must survive.
    pub recent_sidecar: PathBuf,
    pub old_file_history: PathBuf,
    pub recent_file_history: PathBuf,
    pub old_debug: PathBuf,
    pub recent_debug: PathBuf,
    pub old_caches: Vec<PathBuf>,
    /// Files that must never be offered or touched.
    pub protected: Vec<PathBuf>,
}

/// Build a fake `~/.claude` plus `~/.claude.json`.
///
/// `cleanup_period_days`: `Some(n)` writes `settings.json` with that value,
/// `None` writes settings without the key. `big_transcript_len`: size of an
/// extra old transcript meant to trip `large_session_file_mb`.
pub fn claude_home(
    home: &FakeHome,
    cleanup_period_days: Option<i64>,
    big_transcript_len: usize,
) -> ClaudeFixture {
    let root = home.dir(".claude");
    let project_path = home.dir("code/my-app");
    let enc = encode_project(&project_path);
    let pdir = format!(".claude/projects/{enc}");

    let line = br#"{"type":"user","message":"hello"}
"#;
    let old_t = home.file(format!("{pdir}/{OLD_SESSION}.jsonl"), line);
    let recent_t = home.file(format!("{pdir}/{RECENT_SESSION}.jsonl"), line);
    let big_t = home.sized_file(format!("{pdir}/{BIG_SESSION}.jsonl"), big_transcript_len);

    let old_side = home.dir(format!("{pdir}/{OLD_SESSION}"));
    home.file(
        format!("{pdir}/{OLD_SESSION}/subagents/agent-a.jsonl"),
        line,
    );
    home.file(format!("{pdir}/{OLD_SESSION}/tool-results/r1.txt"), "out");
    let orphan_side = home.dir(format!("{pdir}/{ORPHAN_SIDECAR}"));
    home.file(
        format!("{pdir}/{ORPHAN_SIDECAR}/subagents/agent-b.jsonl"),
        line,
    );
    let recent_side = home.dir(format!("{pdir}/{RECENT_SESSION}"));
    home.file(
        format!("{pdir}/{RECENT_SESSION}/subagents/agent-c.jsonl"),
        line,
    );
    // Per-project auto-memory: user data living beside the transcripts.
    let project_memory = home.file(format!("{pdir}/memory/MEMORY.md"), "- remember this\n");

    let old_fh = home.dir(format!(".claude/file-history/{OLD_SESSION}"));
    home.file(
        format!(".claude/file-history/{OLD_SESSION}/abc@v1"),
        "before",
    );
    let recent_fh = home.dir(format!(".claude/file-history/{RECENT_SESSION}"));
    home.file(
        format!(".claude/file-history/{RECENT_SESSION}/def@v1"),
        "before",
    );

    let old_debug = home.file(format!(".claude/debug/{OLD_SESSION}.txt"), "debug old");
    let recent_debug = home.file(format!(".claude/debug/{RECENT_SESSION}.txt"), "debug new");

    let old_caches = vec![
        home.file(".claude/paste-cache/p1.txt", "pasted"),
        home.dir(".claude/image-cache/sess"),
        home.file(".claude/shell-snapshots/snapshot-zsh-1.sh", "export A=1"),
        home.file(".claude/statsig/statsig.cached.evaluations", "{}"),
    ];
    home.file(".claude/image-cache/sess/img.png", "png");

    let settings = match cleanup_period_days {
        Some(n) => format!(r#"{{"cleanupPeriodDays": {n}, "model": "opus"}}"#),
        None => r#"{"model": "opus"}"#.to_string(),
    };
    let protected = vec![
        home.file(".claude/settings.json", settings),
        home.file(".claude/settings.local.json", "{}"),
        home.file(".claude/history.jsonl", line),
        home.file(".claude/CLAUDE.md", "# my rules\n"),
        home.file(".claude/memory/notes.md", "memory\n"),
        home.file(".claude/skills/s/SKILL.md", "skill\n"),
        home.file(".claude/agents/reviewer.md", "agent\n"),
        home.file(".claude/commands/ship.md", "cmd\n"),
        home.file(".claude/plugins/installed.json", "{}"),
        home.file(".claude.json", r#"{"oauthAccount": "x"}"#),
        project_memory,
    ];

    // Age everything old first, then set the recent ones — including the
    // protected files, which are old but must still never be offered.
    age_tree(&root, OLD_DAYS);
    age(&home.path(".claude.json"), OLD_DAYS);
    for p in [&recent_t, &recent_debug] {
        age(p, RECENT_DAYS);
    }
    age_tree(&recent_side, RECENT_DAYS);
    age_tree(&recent_fh, RECENT_DAYS);

    ClaudeFixture {
        root,
        project_dir: home.path(&pdir),
        project_path,
        old_transcripts: vec![old_t, big_t],
        recent_transcripts: vec![recent_t],
        old_sidecars: vec![old_side, orphan_side],
        recent_sidecar: recent_side,
        old_file_history: old_fh,
        recent_file_history: recent_fh,
        old_debug,
        recent_debug,
        old_caches,
        protected,
    }
}

/// Paths inside a fake `~/.codex`.
#[derive(Debug, Clone)]
pub struct CodexFixture {
    pub root: PathBuf,
    pub old_sessions: Vec<PathBuf>,
    pub recent_sessions: Vec<PathBuf>,
    pub old_archived: Vec<PathBuf>,
    pub protected: Vec<PathBuf>,
}

/// Build a fake `~/.codex` with a dated sessions tree and archived sessions.
pub fn codex_home(home: &FakeHome) -> CodexFixture {
    let root = home.dir(".codex");
    let line = br#"{"type":"session_meta"}
"#;
    let old = home.file(
        ".codex/sessions/2026/07/01/rollout-2026-07-01T10-00-00-aaaa.jsonl",
        line,
    );
    let recent = home.file(
        ".codex/sessions/2026/09/27/rollout-2026-09-27T10-00-00-bbbb.jsonl",
        line,
    );
    let archived = home.file(
        ".codex/archived_sessions/rollout-2026-06-01T10-00-00-cccc.jsonl",
        line,
    );
    let protected = vec![
        home.file(".codex/auth.json", r#"{"OPENAI_API_KEY": "sk-test"}"#),
        home.file(".codex/config.toml", "model = \"o3\"\n"),
        home.file(".codex/memories/m.md", "memory\n"),
        home.file(".codex/AGENTS.md", "# rules\n"),
    ];
    age_tree(&root, OLD_DAYS);
    age(&recent, RECENT_DAYS);
    CodexFixture {
        root,
        old_sessions: vec![old],
        recent_sessions: vec![recent],
        old_archived: vec![archived],
        protected,
    }
}

/// Paths inside a fake VS Code-family `User/` directory.
#[derive(Debug, Clone)]
pub struct EditorFixture {
    pub user_dir: PathBuf,
    pub state_db: PathBuf,
    pub backup: PathBuf,
    /// `workspaceStorage/<hash>` whose folder is gone.
    pub orphan_workspace: PathBuf,
    /// `workspaceStorage/<hash>` whose folder exists.
    pub live_workspace: PathBuf,
    /// `workspaceStorage/<hash>` for a remote (ssh) workspace.
    pub remote_workspace: PathBuf,
}

/// Build a fake `<app>/User` directory (macOS layout, which the scanner
/// checks on every platform) with a `state_db_len`-byte state database.
pub fn editor_user(home: &FakeHome, app: &str, state_db_len: usize) -> EditorFixture {
    let base = format!("Library/Application Support/{app}/User");
    let user_dir = home.dir(&base);
    let state_db = home.sized_file(format!("{base}/globalStorage/state.vscdb"), state_db_len);
    let backup = home.sized_file(format!("{base}/globalStorage/state.vscdb.backup"), 2048);

    let live_folder = home.dir(format!("code/{app}-live"));
    let gone_folder = home.path(format!("code/{app}-deleted project"));
    let uri = |p: &Path| format!("file://{}", p.to_string_lossy().replace(' ', "%20"));

    let live_ws = home.dir(format!("{base}/workspaceStorage/aaaa1111"));
    home.file(
        format!("{base}/workspaceStorage/aaaa1111/workspace.json"),
        format!(r#"{{"folder": "{}"}}"#, uri(&live_folder)),
    );
    home.sized_file(
        format!("{base}/workspaceStorage/aaaa1111/state.vscdb"),
        1024,
    );

    let orphan_ws = home.dir(format!("{base}/workspaceStorage/bbbb2222"));
    home.file(
        format!("{base}/workspaceStorage/bbbb2222/workspace.json"),
        format!(r#"{{"folder": "{}"}}"#, uri(&gone_folder)),
    );
    home.sized_file(
        format!("{base}/workspaceStorage/bbbb2222/state.vscdb"),
        4096,
    );

    let remote_ws = home.dir(format!("{base}/workspaceStorage/cccc3333"));
    home.file(
        format!("{base}/workspaceStorage/cccc3333/workspace.json"),
        r#"{"folder": "vscode-remote://ssh-remote+box/home/me/app"}"#,
    );

    EditorFixture {
        user_dir,
        state_db,
        backup,
        orphan_workspace: orphan_ws,
        live_workspace: live_ws,
        remote_workspace: remote_ws,
    }
}

/// Seed a realistic agents layout into `home` for `void dev seed`.
pub fn seed(home: &FakeHome) {
    claude_home(home, None, 64 * 1024);
    codex_home(home);
    // Sparse, so it looks like the 60 GB monsters people report without
    // costing disk; allocation-aware sizing means only a lowered threshold
    // (`AgentDataScanner::with_state_db_threshold`) will flag it.
    let cursor = editor_user(home, "Cursor", 4096);
    if let Ok(f) = std::fs::OpenOptions::new()
        .write(true)
        .open(&cursor.state_db)
    {
        let _ = f.set_len(200 * 1024 * 1024);
    }
}
