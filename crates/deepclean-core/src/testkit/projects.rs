//! Fixtures for the projects feature (the "project graveyard").
//!
//! Every stale fixture is stale on both clocks the scanner reads: its commit
//! is back-dated with `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE`, and its files
//! are aged with [`age_tree`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{age_tree, git, FakeHome};

/// How old the stale fixtures are.
pub const STALE_DAYS: u64 = 200;

/// Commit everything in `dir`, dated `days_ago`.
pub fn commit_dated(dir: &Path, message: &str, days_ago: u64) {
    git(dir, &["add", "-A"]);
    let when = SystemTime::now() - Duration::from_secs(days_ago * 86_400);
    let secs = when.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let date = format!("@{secs} +0000");
    let out = Command::new("git")
        .args(["commit", "-q", "-m", message])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Void Test")
        .env("GIT_AUTHOR_EMAIL", "test@void.invalid")
        .env("GIT_COMMITTER_NAME", "Void Test")
        .env("GIT_COMMITTER_EMAIL", "test@void.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("git must be installed to run the fixtures");
    assert!(
        out.status.success(),
        "git commit failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo at `home/code/<name>` with a README and one commit `days_ago`.
fn project(home: &FakeHome, name: &str, days_ago: u64) -> PathBuf {
    let repo = home.dir(format!("code/{name}"));
    git(&repo, &["init", "-q", "-b", "main"]);
    home.file(format!("code/{name}/README.md"), format!("# {name}\n"));
    home.file(format!("code/{name}/src/main.rs"), "fn main() {}\n");
    commit_dated(&repo, "initial", days_ago);
    repo
}

/// Give `repo` a bare "remote" under `home/.remotes` and push `main` to it.
fn push_to_bare_remote(home: &FakeHome, repo: &Path, name: &str) -> PathBuf {
    let remote = home.dir(format!(".remotes/{name}.git"));
    git(&remote, &["init", "-q", "--bare"]);
    let remote_str = remote.to_string_lossy().to_string();
    git(repo, &["remote", "add", "origin", &remote_str]);
    git(repo, &["push", "-q", "origin", "main"]);
    remote
}

/// Abandoned 200 days ago, but every commit is on its remote and the tree
/// is clean: trashing it loses nothing.
pub fn stale_pushed(home: &FakeHome, name: &str) -> PathBuf {
    let repo = project(home, name, STALE_DAYS);
    push_to_bare_remote(home, &repo, name);
    age_tree(&repo, STALE_DAYS);
    repo
}

/// Abandoned 200 days ago, never pushed, with an ignored `node_modules`.
pub fn stale_unpushed_with_node_modules(home: &FakeHome, name: &str) -> PathBuf {
    let repo = home.dir(format!("code/{name}"));
    git(&repo, &["init", "-q", "-b", "main"]);
    home.file(
        format!("code/{name}/package.json"),
        format!("{{\"name\":\"{name}\"}}\n"),
    );
    home.file(format!("code/{name}/index.js"), "console.log('hi')\n");
    home.file(format!("code/{name}/.gitignore"), "node_modules/\n");
    commit_dated(&repo, "initial", STALE_DAYS);
    home.sized_file(format!("code/{name}/node_modules/left-pad/index.js"), 4096);
    home.sized_file(format!("code/{name}/node_modules/react/index.js"), 8192);
    age_tree(&repo, STALE_DAYS);
    repo
}

/// Pushed, but with an uncommitted edit that was also abandoned.
pub fn stale_dirty(home: &FakeHome, name: &str) -> PathBuf {
    let repo = project(home, name, STALE_DAYS);
    push_to_bare_remote(home, &repo, name);
    home.file(
        format!("code/{name}/src/main.rs"),
        "fn main() { todo!() }\n",
    );
    age_tree(&repo, STALE_DAYS);
    repo
}

/// Committed today: not stale.
pub fn fresh(home: &FakeHome, name: &str) -> PathBuf {
    let repo = project(home, name, 0);
    home.sized_file(format!("code/{name}/node_modules/dep/index.js"), 1024);
    repo
}

/// Seed a realistic projects layout into `home` for `void dev seed`.
pub fn seed(home: &FakeHome) {
    stale_pushed(home, "old-landing-page");
    stale_unpushed_with_node_modules(home, "agent-spike-todo-app");
    stale_dirty(home, "half-finished-cli");
    fresh(home, "current-work");
}
