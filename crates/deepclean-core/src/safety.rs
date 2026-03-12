use std::path::{Path, PathBuf};

/// Ensures we never delete critical system or user files.
pub struct SafetyChecker {
    home: PathBuf,
    blocked_names: Vec<&'static str>,
    sentinel_names: Vec<&'static str>,
    extra_blocked: Vec<PathBuf>,
}

impl SafetyChecker {
    pub fn new(extra_blocked: Vec<PathBuf>) -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
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
        ]
    }

    /// Returns `true` if the path is safe to operate on.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };

        // Check home-relative blocked directories
        for blocked in &self.home_blocked_paths() {
            if canonical.starts_with(blocked) {
                return false;
            }
        }

        // Check blocked names anywhere in the path
        for component in canonical.components() {
            let name = component.as_os_str().to_string_lossy();
            if self.blocked_names.contains(&name.as_ref()) {
                return false;
            }
        }

        // Check extra user-configured blocked paths
        for blocked in &self.extra_blocked {
            let blocked_canonical = blocked.canonicalize().unwrap_or_else(|_| blocked.clone());
            if canonical.starts_with(&blocked_canonical) {
                return false;
            }
        }

        // Check for sentinel files inside the target directory
        if canonical.is_dir() {
            if self.contains_sentinel(&canonical) {
                return false;
            }
        }

        true
    }

    /// Upgrade risk for paths directly under home or symlinks.
    pub fn validate_risk(&self, path: &Path, base_risk: crate::model::RiskLevel) -> crate::model::RiskLevel {
        use crate::model::RiskLevel;

        let mut risk = base_risk;

        // Paths directly under home are more dangerous
        if let Some(parent) = path.parent() {
            if parent == self.home {
                risk = risk.max(RiskLevel::Caution);
            }
        }

        // Symlinks are suspicious
        if path.is_symlink() {
            risk = risk.max(RiskLevel::Caution);
        }

        risk
    }

    /// Check if a directory contains any sentinel files (shallow check).
    fn contains_sentinel(&self, dir: &Path) -> bool {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return false,
        };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
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
    fn sentinel_detection() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".env"), "SECRET=x").unwrap();
        let checker = SafetyChecker::new(vec![]);
        assert!(!checker.is_path_allowed(tmp.path()));
    }
}
