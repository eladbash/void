//! Fixture builders for a fake home directory.
//!
//! One kit serves three audiences:
//!
//! - **Unit and integration tests** build exactly the state they assert on —
//!   a dirty worktree, an orphaned Ollama blob, a three-month-old transcript —
//!   inside a temp dir, and never touch the real home.
//! - **CI** runs those same tests on every push.
//! - **Development**: `void dev seed <dir>` calls [`seed_all`] to build a
//!   realistic sandbox, and `void --home <dir> scan` scans it. Every feature
//!   can be exercised end to end, destructively, with nothing at stake.
//!
//! Each feature owns one submodule and a `seed(&FakeHome)` function that
//! [`seed_all`] calls.

pub mod agents;
pub mod models;
pub mod projects;
pub mod worktrees;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// A directory standing in for `$HOME`.
#[derive(Debug, Clone)]
pub struct FakeHome {
    root: PathBuf,
}

impl FakeHome {
    /// Use `root` (created if missing) as the fake home.
    pub fn at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).expect("create fake home");
        // Canonical, so paths built here compare equal to what scanners and
        // the safety checker see after their own canonicalization.
        let root = root.canonicalize().unwrap_or(root);
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path of `rel` inside the fake home.
    pub fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.root.join(rel)
    }

    /// Create a directory (and parents).
    pub fn dir(&self, rel: impl AsRef<Path>) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(&p).expect("create dir");
        p
    }

    /// Write a file (creating parents) with `bytes` of content.
    pub fn file(&self, rel: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> PathBuf {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&p, contents).expect("write file");
        p
    }

    /// Write a file of `len` bytes of filler.
    pub fn sized_file(&self, rel: impl AsRef<Path>, len: usize) -> PathBuf {
        self.file(rel, vec![b'v'; len])
    }
}

/// Set a path's modification time to `days` ago. Directories too.
pub fn age(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    let file = std::fs::File::options()
        .read(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))
        .expect("open for set_modified");
    file.set_modified(when).expect("set mtime");
}

/// Age a path and everything under it.
pub fn age_tree(path: &Path, days: u64) {
    for entry in ignore::WalkBuilder::new(path)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .build()
        .flatten()
    {
        // Children first would be nicer, but set_modified on a directory does
        // not bump its parent, so order does not matter. An entry can vanish
        // between the listing and the open (git's own lock and temp files);
        // there is nothing left to age then.
        let path = entry.path();
        let when = SystemTime::now() - Duration::from_secs(days * 86_400);
        match std::fs::File::options()
            .read(true)
            .open(path)
            .or_else(|_| std::fs::File::open(path))
        {
            Ok(file) => file.set_modified(when).expect("set mtime"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!("open for set_modified: {err:?}"),
        }
    }
}

/// Run git in `dir` with a fixed identity and no user/system config, so
/// fixtures behave the same on every machine and in CI.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        // No background `gc --auto` or maintenance touching the fixture
        // while tests read or age it.
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "gc.auto")
        .env("GIT_CONFIG_VALUE_0", "0")
        .env("GIT_CONFIG_KEY_1", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_1", "false")
        .env("GIT_AUTHOR_NAME", "Void Test")
        .env("GIT_AUTHOR_EMAIL", "test@void.invalid")
        .env("GIT_COMMITTER_NAME", "Void Test")
        .env("GIT_COMMITTER_EMAIL", "test@void.invalid")
        .output()
        .expect("git must be installed to run the fixtures");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `git init` a repo on branch `main` with one commit.
pub fn git_repo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create repo dir");
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "# fixture\n").expect("write readme");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "initial"]);
    dir.to_path_buf()
}

/// Build every feature's fixture into `home`. Used by `void dev seed`.
pub fn seed_all(home: &FakeHome) {
    worktrees::seed(home);
    agents::seed(home);
    models::seed(home);
    projects::seed(home);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_moves_mtime_into_the_past() {
        let tmp = tempfile::tempdir().unwrap();
        let home = FakeHome::at(tmp.path());
        let f = home.file("a/b.txt", "x");
        age(&f, 10);
        let days =
            crate::staleness::days_since(std::fs::metadata(&f).unwrap().modified().unwrap().into());
        assert!((9..=10).contains(&days), "got {days}");
    }

    #[test]
    fn git_repo_has_a_commit_on_main() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = git_repo(&tmp.path().join("r"));
        assert_eq!(git(&repo, &["branch", "--show-current"]).trim(), "main");
    }
}
