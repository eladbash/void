//! Fixtures for the worktrees feature.
//!
//! Everything is built with real `git` (via [`super::git`], which isolates
//! config), so the scanner and executor see exactly what an agent leaves
//! behind: `git worktree add` under `<repo>/.claude/worktrees/<name>` on a
//! `worktree-<name>` branch, locks, stale admin entries, orphans.

use std::path::{Path, PathBuf};

use super::{git, git_repo, FakeHome};

/// Ignore rules every fixture repo commits, so build artifacts and `.env`
/// never make a worktree look dirty — as in a real project.
const GITIGNORE: &str = "node_modules/\ntarget/\n.venv/\n.env\n.claude/worktrees/\n";

/// A repo on `main` with a committed `.gitignore`, `package.json` and
/// `Cargo.toml`, so every worktree of it is a Node + Rust checkout whose
/// `node_modules` and `target` are real build artifacts.
pub fn repo(dir: &Path) -> PathBuf {
    let repo = git_repo(dir);
    std::fs::write(repo.join(".gitignore"), GITIGNORE).expect("write .gitignore");
    std::fs::write(repo.join("package.json"), "{\"name\":\"fixture\"}\n").expect("package.json");
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "project files"]);
    repo.canonicalize().unwrap_or(repo)
}

/// `git worktree add -b <branch> <path>` from `main`.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create worktree parent");
    }
    let target = path.to_string_lossy().into_owned();
    git(
        repo,
        &["worktree", "add", "-q", "-b", branch, &target, "main"],
    );
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A Claude Code worktree: `<repo>/.claude/worktrees/<name>` on branch
/// `worktree-<name>`, identical to `main`.
pub fn claude_worktree(repo: &Path, name: &str) -> PathBuf {
    add_worktree(
        repo,
        &repo.join(".claude/worktrees").join(name),
        &format!("worktree-{name}"),
    )
}

/// Commit a new file in `worktree`, returning nothing — the branch now has
/// one commit `main` does not.
pub fn commit_in(worktree: &Path, file: &str) {
    std::fs::write(worktree.join(file), format!("{file}\n")).expect("write file");
    git(worktree, &["add", file]);
    git(worktree, &["commit", "-q", "-m", &format!("add {file}")]);
}

/// Clean worktree whose branch has a commit that is fast-forward merged into
/// `main`.
pub fn clean_merged(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    commit_in(&wt, &format!("{name}.txt"));
    git(
        repo,
        &["merge", "-q", "--ff-only", &format!("worktree-{name}")],
    );
    wt
}

/// Clean worktree whose commit is "pushed" (a remote-tracking ref contains
/// it) but not merged into `main`. Offline: the remote ref is written
/// directly, no remote exists.
pub fn clean_unmerged_pushed(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    commit_in(&wt, &format!("{name}.txt"));
    let branch = format!("worktree-{name}");
    git(
        repo,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{branch}"),
            &format!("refs/heads/{branch}"),
        ],
    );
    wt
}

/// Clean worktree with a commit that exists nowhere else.
pub fn unpushed(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    commit_in(&wt, &format!("{name}.txt"));
    wt
}

/// Worktree with a modified tracked file.
pub fn dirty(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    std::fs::write(wt.join("README.md"), "# edited by an agent\n").expect("modify");
    wt
}

/// Worktree with an untracked, unignored file.
pub fn untracked(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    std::fs::write(wt.join("notes.txt"), "agent notes\n").expect("untracked");
    wt
}

/// Worktree locked the way Claude Code locks one while an agent runs.
pub fn locked(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    git(
        repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "claude agent 4242 is running",
            &wt.to_string_lossy(),
        ],
    );
    wt
}

/// Give `worktree` a `node_modules` and a Cargo `target` (both ignored).
pub fn add_artifacts(worktree: &Path) {
    let home = FakeHome::at(worktree);
    home.sized_file("node_modules/left-pad/index.js", 4096);
    home.sized_file("node_modules/.package-lock.json", 512);
    home.sized_file("target/debug/fixture", 8192);
}

/// Put an (ignored) `.env` at the worktree root.
pub fn add_env(worktree: &Path) {
    std::fs::write(worktree.join(".env"), "API_KEY=secret\n").expect("write .env");
}

/// A worktree dir whose admin entry in the repo was deleted: the repo no
/// longer knows it.
pub fn orphan_forgotten(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    std::fs::write(wt.join("scratch.txt"), "leftover\n").expect("write scratch");
    let admin = crate::git::resolve_gitdir_link(&wt).expect("worktree has a gitdir link");
    std::fs::remove_dir_all(admin).expect("delete admin entry");
    wt
}

/// A worktree-looking dir whose `.git` points at a repository that is gone.
pub fn orphan_missing_repo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create orphan dir");
    std::fs::write(
        dir.join(".git"),
        format!(
            "gitdir: {}\n",
            dir.join("../../gone-repo/.git/worktrees/x").display()
        ),
    )
    .expect("write .git file");
    std::fs::write(dir.join("main.py"), "print('hi')\n").expect("write file");
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// A worktree whose directory was deleted by hand: git keeps a prunable
/// admin entry. Returns the (now missing) worktree path.
pub fn prunable(repo: &Path, name: &str) -> PathBuf {
    let wt = claude_worktree(repo, name);
    std::fs::remove_dir_all(&wt).expect("delete worktree dir");
    wt
}

/// Agent branches with no worktree: one merged into `main`, one carrying an
/// unmerged commit. Returns `(merged, unmerged)`.
pub fn agent_branches(repo: &Path, merged: &str, unmerged: &str) -> (String, String) {
    git(repo, &["branch", merged, "main"]);
    let tree = git(repo, &["rev-parse", "main^{tree}"]);
    let sha = git(
        repo,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            "main",
            "-m",
            "unmerged agent work",
        ],
    );
    git(repo, &["branch", unmerged, sha.trim()]);
    (merged.to_string(), unmerged.to_string())
}

/// A Cursor worktree at `<home>/.cursor/worktrees/<repo-name>/<name>`.
pub fn cursor_worktree(home: &FakeHome, repo: &Path, name: &str) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    let path = home.path(format!(".cursor/worktrees/{repo_name}/{name}"));
    add_worktree(repo, &path, &format!("cursor/{name}"))
}

/// Seed a realistic worktrees layout into `home` for `void dev seed`.
///
/// `~/code/webapp` has one Claude worktree of every kind; `~/code/api` has
/// Cursor worktrees; `~/.codex/worktrees` holds an orphan.
pub fn seed(home: &FakeHome) {
    let webapp = repo(&home.path("code/webapp"));
    home.sized_file("code/webapp/node_modules/react/index.js", 2048);

    let merged = clean_merged(&webapp, "fix-login");
    add_artifacts(&merged);
    let pushed = clean_unmerged_pushed(&webapp, "add-search");
    add_artifacts(&pushed);
    let wip = unpushed(&webapp, "refactor-db");
    add_artifacts(&wip);
    let edited = dirty(&webapp, "dark-mode");
    add_artifacts(&edited);
    untracked(&webapp, "notes");
    locked(&webapp, "running-agent");
    let secret = clean_merged(&webapp, "with-env");
    add_env(&secret);
    add_artifacts(&secret);
    orphan_forgotten(&webapp, "forgotten");
    prunable(&webapp, "deleted-by-hand");
    agent_branches(&webapp, "claude/old-experiment", "claude/unfinished");

    let api = repo(&home.path("code/api"));
    let c1 = cursor_worktree(home, &api, "k3j2");
    add_artifacts(&c1);
    cursor_worktree(home, &api, "p9x1");

    orphan_missing_repo(&home.path(".codex/worktrees/a1b2/api"));
}
