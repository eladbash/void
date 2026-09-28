use std::path::{Path, PathBuf};

/// Agent instruction files: sentinels for deletion, but not for a recoverable
/// move to the Trash. See [`SafetyChecker::is_trash_allowed`].
const AGENT_DOCS: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// Which sentinel files block an operation on a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sentinels {
    All,
    ExceptAgentDocs,
    Ignore,
}

/// Ensures we never delete critical system or user files.
pub struct SafetyChecker {
    home: PathBuf,
    blocked_names: Vec<&'static str>,
    sentinel_names: Vec<&'static str>,
    extra_blocked: Vec<PathBuf>,
}

impl SafetyChecker {
    pub fn new(extra_blocked: Vec<PathBuf>) -> Self {
        Self::with_home(crate::paths::home_or_root(), extra_blocked)
    }

    /// A checker that treats `home` as the user's home directory.
    ///
    /// Tests and the development sandbox use this so the protected set is
    /// computed against the fake home rather than the real one.
    pub fn with_home(home: PathBuf, extra_blocked: Vec<PathBuf>) -> Self {
        // Candidate paths are canonicalized before comparison, so home must be
        // too — on macOS a temp-dir home under /var is really /private/var,
        // and an uncanonicalized home would silently protect nothing.
        let home = home.canonicalize().unwrap_or(home);
        Self {
            home,
            blocked_names: vec![
                ".ssh",
                ".gnupg",
                ".aws",
                ".config",
                ".gitconfig",
                ".zshrc",
                ".bashrc",
                ".env",
                ".Trash",
            ],
            sentinel_names: vec![
                ".env",
                "credentials",
                "secrets.yaml",
                "secrets.yml",
                "id_rsa",
                "id_ed25519",
                // AI agent state that holds credentials or irreplaceable
                // user-authored context. A directory holding one of these is
                // never deleted wholesale — agent data cleanup must target the
                // generated subdirectories (transcripts, debug logs) instead.
                ".claude.json",
                "auth.json",
                "CLAUDE.md",
                "AGENTS.md",
            ],
            extra_blocked,
        }
    }

    /// Hardcoded paths under home that must never be touched.
    fn home_blocked_paths(&self) -> Vec<PathBuf> {
        vec![
            self.home.join("Library/Keychains"),
            self.home.join("Library/Preferences"),
            self.home.join("Documents"),
            self.home.join("Desktop"),
            self.home.join("Pictures"),
            self.home.join("Downloads"),
            // Agent configuration and memory: user-authored or credentials.
            // Generated agent data (transcripts, debug logs, worktrees) lives
            // in sibling directories that stay cleanable.
            self.home.join(".claude.json"),
            self.home.join(".claude/settings.json"),
            self.home.join(".claude/settings.local.json"),
            self.home.join(".claude/memory"),
            self.home.join(".claude/skills"),
            self.home.join(".claude/agents"),
            self.home.join(".claude/commands"),
            self.home.join(".claude/plugins"),
            self.home.join(".claude/history.jsonl"),
            self.home.join(".codex/auth.json"),
            self.home.join(".codex/config.toml"),
            self.home.join(".codex/memories"),
        ]
    }

    /// The home directory this checker protects.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Returns `true` if the path is safe to delete.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        self.check_path(path, Sentinels::All)
    }

    /// Returns `true` if the path is safe to use as a command's working directory.
    ///
    /// Commands like `cargo clean` run *inside* a project root but never delete
    /// it, so sentinel files there ("don't delete this directory") must not
    /// block them.
    pub fn is_workdir_allowed(&self, path: &Path) -> bool {
        self.check_path(path, Sentinels::Ignore)
    }

    /// Returns `true` if the path may be moved to the Trash.
    ///
    /// Stricter than a working directory, looser than a delete in exactly one
    /// way: agent instruction files (`CLAUDE.md`, `AGENTS.md`) do not block a
    /// *recoverable* move. Nearly every AI-built project carries one, and
    /// refusing to trash those would make the project graveyard useless — the
    /// file travels to the Trash with its project and can be put back.
    /// Credentials and `.env` files still block.
    pub fn is_trash_allowed(&self, path: &Path) -> bool {
        self.check_path(path, Sentinels::ExceptAgentDocs)
    }

    /// Whether `canonical` is an AI editor's generated state under a Linux
    /// `~/.config/<Editor>/User/` — the one part of `.config` Void cleans.
    ///
    /// `.config` stays a blocked name everywhere else: it holds credentials
    /// and hand-written dotfiles. Only the editors' state database (vacuumed,
    /// never deleted), its backup, and per-workspace storage are exempt.
    fn is_editor_state(&self, canonical: &Path) -> bool {
        let Ok(rel) = canonical.strip_prefix(self.home.join(".config")) else {
            return false;
        };
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        let editor = matches!(
            parts.first().map(String::as_str),
            Some("Cursor" | "Code" | "Windsurf")
        );
        if !editor || parts.get(1).map(String::as_str) != Some("User") {
            return false;
        }
        match parts.get(2).map(String::as_str) {
            Some("workspaceStorage") => parts.len() >= 4,
            Some("globalStorage") => matches!(
                parts.get(3).map(String::as_str),
                Some("state.vscdb" | "state.vscdb.backup")
            ),
            _ => false,
        }
    }

    /// Blocked-name check, honouring the editor-state exemption.
    fn has_blocked_name(&self, canonical: &Path) -> bool {
        let editor_state = self.is_editor_state(canonical);
        canonical.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            if editor_state && name == ".config" {
                return false;
            }
            self.blocked_names.contains(&name.as_ref())
        })
    }

    fn check_path(&self, path: &Path, sentinels: Sentinels) -> bool {
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };

        // Home itself, the filesystem root, and anything *containing* home
        // are never deletable, whatever a scanner claims.
        if canonical == self.home
            || canonical.parent().is_none()
            || self.home.starts_with(&canonical)
        {
            return false;
        }

        // Check home-relative blocked directories
        for blocked in &self.home_blocked_paths() {
            if canonical.starts_with(blocked) {
                return false;
            }
        }

        // Check blocked names anywhere in the path
        if self.has_blocked_name(&canonical) {
            return false;
        }

        // Check extra user-configured blocked paths
        for blocked in &self.extra_blocked {
            let blocked_canonical = blocked.canonicalize().unwrap_or_else(|_| blocked.clone());
            if canonical.starts_with(&blocked_canonical) {
                return false;
            }
        }

        // Check for sentinel files inside the target directory
        if sentinels != Sentinels::Ignore
            && canonical.is_dir()
            && self.contains_sentinel(&canonical, sentinels == Sentinels::ExceptAgentDocs)
        {
            return false;
        }

        true
    }

    /// Paths that must never be touched, whether directly or via a command.
    ///
    /// Distinct from [`Self::home_blocked_paths`]: those also cover directories
    /// whose *contents* Void legitimately cleans (`~/Downloads`), whereas
    /// nothing under these may be written to at all.
    fn absolutely_protected(&self) -> Vec<PathBuf> {
        vec![
            self.home.join("Library/Keychains"),
            self.home.join("Library/Preferences"),
        ]
    }

    /// The one protected container whose *contents* Void legitimately cleans.
    ///
    /// The System scanner exists to clear old installers out of `~/Downloads`.
    /// Nothing comparable applies to Documents, Desktop or Pictures, so those
    /// stay blocked all the way down.
    fn cleanable_container(&self) -> PathBuf {
        self.home.join("Downloads")
    }

    /// Check a single command argument that resolves to a filesystem path.
    ///
    /// Narrower than [`Self::is_path_allowed`] in exactly one place: an entry
    /// *inside* `~/Downloads` may be acted on, though the folder itself may
    /// not. Every other protected location is refused for the whole subtree.
    fn is_command_path_allowed(&self, raw: &str, working_dir: Option<&Path>) -> bool {
        let expanded = if let Some(rest) = raw.strip_prefix("~/") {
            self.home.join(rest)
        } else {
            let candidate = PathBuf::from(raw);
            if candidate.is_absolute() {
                candidate
            } else {
                // A relative argument means nothing without the directory the
                // command runs in — `rm -rf Downloads` is only dangerous once
                // resolved. Checked only when it names something real, so
                // ordinary subcommands are not mistaken for paths.
                match working_dir.map(|dir| dir.join(&candidate)) {
                    Some(joined) if joined.exists() => joined,
                    _ => return true,
                }
            }
        };

        let canonical = expanded.canonicalize().unwrap_or(expanded);
        let cleanable = self.cleanable_container();

        for protected in self.home_blocked_paths() {
            if protected == cleanable {
                // The folder itself, never; its entries, yes.
                if canonical == protected {
                    return false;
                }
            } else if canonical.starts_with(&protected) {
                return false;
            }
        }

        // The home directory itself is never a valid target.
        if canonical == self.home || canonical == Path::new("/") {
            return false;
        }

        for protected in self.absolutely_protected() {
            if canonical.starts_with(&protected) {
                return false;
            }
        }

        if self.has_blocked_name(&canonical) {
            return false;
        }

        for blocked in &self.extra_blocked {
            let blocked_canonical = blocked.canonicalize().unwrap_or_else(|_| blocked.clone());
            if canonical.starts_with(&blocked_canonical) {
                return false;
            }
        }

        true
    }

    /// Validate every path-like argument of a command before running it.
    ///
    /// `ActionMethod::Command` previously bypassed safety entirely — only its
    /// working directory was checked — so `rm -rf` and the `osascript` trash
    /// one-liner could reach any path at all. Arguments are scanned for
    /// absolute and `~`-relative paths, including ones embedded in quoted
    /// script strings.
    pub fn is_command_allowed(
        &self,
        program: &str,
        args: &[String],
        working_dir: Option<&Path>,
    ) -> Result<(), String> {
        for arg in args {
            for token in arg.split(['"', '\'', ' ', '\t', '\n', '(', ')', ',']) {
                let token = token.trim_end_matches(['.', ';', ':']);
                if token.is_empty() {
                    continue;
                }
                if !self.is_command_path_allowed(token, working_dir) {
                    return Err(format!(
                        "`{program}` was blocked from operating on a protected path: {token}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Upgrade risk for paths directly under home or symlinks.
    pub fn validate_risk(
        &self,
        path: &Path,
        base_risk: crate::model::RiskLevel,
    ) -> crate::model::RiskLevel {
        use crate::model::RiskLevel;

        // A path directly under home, or a symlink, is worth a second look.
        let directly_under_home = path.parent() == Some(self.home.as_path());
        let suspicious = directly_under_home || path.is_symlink();

        if suspicious {
            base_risk.max(RiskLevel::Caution)
        } else {
            base_risk
        }
    }

    /// Check if a directory contains any sentinel files (shallow check).
    fn contains_sentinel(&self, dir: &Path, except_agent_docs: bool) -> bool {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return false,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if except_agent_docs && AGENT_DOCS.contains(&name_str.as_ref()) {
                continue;
            }
            if self.sentinel_names.contains(&name_str.as_ref()) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_ssh_directory() {
        let checker = SafetyChecker::new(vec![]);
        let home = dirs::home_dir().unwrap();
        assert!(!checker.is_path_allowed(&home.join(".ssh")));
        assert!(!checker.is_path_allowed(&home.join(".ssh/id_rsa")));
    }

    #[test]
    fn blocks_documents() {
        let checker = SafetyChecker::new(vec![]);
        let home = dirs::home_dir().unwrap();
        assert!(!checker.is_path_allowed(&home.join("Documents")));
    }

    #[test]
    fn allows_normal_project_path() {
        let checker = SafetyChecker::new(vec![]);
        let home = dirs::home_dir().unwrap();
        assert!(checker.is_path_allowed(&home.join("projects/myapp/target")));
    }

    #[test]
    fn blocks_extra_configured_paths() {
        let home = dirs::home_dir().unwrap();
        let blocked = home.join("important-project");
        let checker = SafetyChecker::new(vec![blocked.clone()]);
        assert!(!checker.is_path_allowed(&blocked.join("target")));
    }

    #[test]
    fn sentinel_does_not_block_command_working_dir() {
        // A project root holding a `.env` must still be usable as the working
        // dir for `cargo clean` — the command never deletes that directory.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("trading");
        std::fs::create_dir_all(project.join("target")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(project.join(".env"), "SECRET=x").unwrap();

        let checker = SafetyChecker::new(vec![]);
        assert!(checker.is_workdir_allowed(&project));
        // ...but deleting that same directory is still refused.
        assert!(!checker.is_path_allowed(&project));
    }

    #[test]
    fn blocked_paths_still_block_command_working_dir() {
        let home = dirs::home_dir().unwrap();
        let blocked = home.join("important-project");
        let checker = SafetyChecker::new(vec![blocked.clone()]);
        assert!(!checker.is_workdir_allowed(&blocked));
        assert!(!checker.is_workdir_allowed(&home.join(".ssh")));
        assert!(!checker.is_workdir_allowed(&home.join("Documents")));
    }

    #[test]
    fn agent_docs_block_deletion_but_not_trashing() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("ai-app");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("CLAUDE.md"), "# notes").unwrap();
        let checker = SafetyChecker::with_home(tmp.path().to_path_buf(), vec![]);
        assert!(!checker.is_path_allowed(&project));
        assert!(checker.is_trash_allowed(&project));

        // Credentials still block even a recoverable move.
        std::fs::write(project.join(".env"), "KEY=1").unwrap();
        assert!(!checker.is_trash_allowed(&project));
    }

    #[test]
    fn linux_editor_state_is_the_only_cleanable_part_of_dot_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let checker = SafetyChecker::with_home(home.clone(), vec![]);
        let user = home.join(".config/Cursor/User");
        assert!(checker.is_path_allowed(&user.join("workspaceStorage/abc123")));
        assert!(checker.is_path_allowed(&user.join("globalStorage/state.vscdb.backup")));
        assert!(checker
            .is_command_allowed(
                "sqlite3",
                &[
                    user.join("globalStorage/state.vscdb").display().to_string(),
                    "VACUUM;".into()
                ],
                None
            )
            .is_ok());
        // Everything else in .config stays blocked.
        assert!(!checker.is_path_allowed(&user.join("settings.json")));
        assert!(!checker.is_path_allowed(&user.join("workspaceStorage")));
        assert!(!checker.is_path_allowed(&home.join(".config/gh/hosts.yml")));
        assert!(!checker.is_path_allowed(&home.join(".config/Cursor")));
    }

    #[test]
    fn home_root_and_ancestors_are_never_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("me");
        std::fs::create_dir(&home).unwrap();
        let checker = SafetyChecker::with_home(home.clone(), vec![]);
        assert!(!checker.is_path_allowed(&home));
        assert!(!checker.is_path_allowed(tmp.path()));
        assert!(!checker.is_path_allowed(Path::new("/")));
        assert!(!checker.is_trash_allowed(&home));
    }

    #[test]
    fn sentinel_detection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".env"), "SECRET=x").unwrap();
        let checker = SafetyChecker::new(vec![]);
        assert!(!checker.is_path_allowed(tmp.path()));
    }
}
