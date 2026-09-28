//! Which scanner handles which ecosystem.
//!
//! Shared by the desktop app, the CLI and the MCP server so all three scan
//! exactly the same things.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::AppConfig;
use crate::error::ScanError;
use crate::model::{CleanableItem, Ecosystem};
use crate::scanner::apple::AppleScanner;
use crate::scanner::docker::DockerScanner;
use crate::scanner::dotnet::DotNetScanner;
use crate::scanner::go::GoScanner;
use crate::scanner::homebrew::HomebrewScanner;
use crate::scanner::java::JavaScanner;
use crate::scanner::jetbrains::JetBrainsScanner;
use crate::scanner::node::NodeScanner;
use crate::scanner::python::PythonScanner;
use crate::scanner::rust::RustScanner;
use crate::scanner::system::SystemScanner;
use crate::scanner::{agents, models, projects, worktrees, EcosystemScanner};

/// Scanners for every enabled ecosystem, rooted at the effective home.
pub fn build(config: &AppConfig) -> Vec<Arc<dyn EcosystemScanner>> {
    build_with_home(config, crate::paths::home_or_root())
}

/// Scanners for every enabled ecosystem, rooted at `home`.
///
/// Returned in [`Ecosystem::ALL`] order, not config order: the walker hands a
/// directory to the *first* scanner that claims it, and the worktree scanner
/// must claim `.claude/worktrees` before the Node scanner claims the
/// `node_modules` inside it.
pub fn build_with_home(config: &AppConfig, home: PathBuf) -> Vec<Arc<dyn EcosystemScanner>> {
    Ecosystem::ALL
        .into_iter()
        .filter(|eco| config.enabled_ecosystems.contains(eco))
        .map(|eco| -> Arc<dyn EcosystemScanner> {
            match eco {
                Ecosystem::Rust => Scoped::wrap(RustScanner, &home),
                Ecosystem::Node => Scoped::wrap(NodeScanner, &home),
                Ecosystem::Apple => Scoped::wrap(AppleScanner, &home),
                Ecosystem::Docker => Scoped::wrap(DockerScanner, &home),
                Ecosystem::Go => Scoped::wrap(GoScanner, &home),
                Ecosystem::System => Scoped::wrap(SystemScanner, &home),
                Ecosystem::Python => Scoped::wrap(PythonScanner, &home),
                Ecosystem::Java => Scoped::wrap(JavaScanner, &home),
                Ecosystem::Homebrew => Scoped::wrap(HomebrewScanner, &home),
                Ecosystem::JetBrains => Scoped::wrap(JetBrainsScanner, &home),
                Ecosystem::DotNet => Scoped::wrap(DotNetScanner, &home),
                Ecosystem::Worktrees => {
                    Arc::new(worktrees::WorktreeScanner::new(home.clone(), config))
                }
                Ecosystem::AgentData => {
                    Arc::new(agents::AgentDataScanner::new(home.clone(), config))
                }
                Ecosystem::Models => Arc::new(
                    models::ModelScanner::new(home.clone(), config)
                        .with_min_dup_size((config.ai.min_duplicate_model_mb * 1024 * 1024).max(1)),
                ),
                Ecosystem::Projects => {
                    Arc::new(projects::ProjectScanner::new(home.clone(), config))
                }
            }
        })
        .collect()
}

/// Roots a home-agnostic scanner at `home`.
///
/// The original scanners resolve their global caches through
/// [`crate::paths::home_dir`]. Wrapping them scopes that lookup to `home` for
/// every call, and — when `home` is not the real home — drops any global
/// location outside it (`go env` paths, the Docker socket), so a sandboxed
/// or test scan can never report, or offer to clean, the real machine.
struct Scoped<S> {
    inner: S,
    home: PathBuf,
    sandboxed: bool,
}

impl<S: EcosystemScanner + 'static> Scoped<S> {
    fn wrap(inner: S, home: &Path) -> Arc<dyn EcosystemScanner> {
        let sandboxed = crate::paths::sync_scope(home.to_path_buf(), crate::paths::is_sandboxed);
        let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        Arc::new(Self {
            inner,
            home,
            sandboxed,
        })
    }
}

#[async_trait]
impl<S: EcosystemScanner> EcosystemScanner for Scoped<S> {
    fn ecosystem(&self) -> Ecosystem {
        self.inner.ecosystem()
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        crate::paths::sync_scope(self.home.clone(), || {
            self.inner.is_candidate(file_name, path)
        })
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        crate::paths::scope(self.home.clone(), self.inner.analyze(path)).await
    }

    async fn analyze_many(&self, path: &Path) -> Result<Vec<CleanableItem>, ScanError> {
        crate::paths::scope(self.home.clone(), self.inner.analyze_many(path)).await
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        let locations =
            crate::paths::sync_scope(self.home.clone(), || self.inner.global_locations());
        if !self.sandboxed {
            return locations;
        }
        locations
            .into_iter()
            .filter(|loc| {
                loc.canonicalize()
                    .unwrap_or_else(|_| loc.clone())
                    .starts_with(&self.home)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enabled_ecosystem_gets_exactly_one_scanner_in_display_order() {
        let config = AppConfig::default();
        let scanners = build_with_home(&config, PathBuf::from("/nonexistent"));
        let ecos: Vec<Ecosystem> = scanners.iter().map(|s| s.ecosystem()).collect();
        assert_eq!(ecos, Ecosystem::ALL.to_vec());
    }

    #[test]
    fn sandboxed_legacy_scanners_only_see_the_sandbox() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join(".cargo/registry/cache")).unwrap();
        // Scan roots are the caller's choice (the Projects scanner scans
        // them); a sandbox caller points them at the sandbox.
        let config = AppConfig {
            scan_roots: vec![home.clone()],
            ..Default::default()
        };
        for scanner in build_with_home(&config, home.clone()) {
            for loc in scanner.global_locations() {
                assert!(
                    loc.starts_with(&home),
                    "{:?} reached outside the sandbox: {}",
                    scanner.ecosystem(),
                    loc.display()
                );
            }
        }
        let rust = build_with_home(
            &AppConfig {
                enabled_ecosystems: vec![Ecosystem::Rust],
                ..Default::default()
            },
            home.clone(),
        );
        assert_eq!(
            rust[0].global_locations(),
            vec![home.join(".cargo/registry")]
        );
    }

    #[test]
    fn disabled_ecosystems_are_skipped() {
        let config = AppConfig {
            enabled_ecosystems: vec![Ecosystem::Node, Ecosystem::Worktrees],
            ..Default::default()
        };
        let ecos: Vec<Ecosystem> = build_with_home(&config, PathBuf::from("/x"))
            .iter()
            .map(|s| s.ecosystem())
            .collect();
        // Worktrees first regardless of config order.
        assert_eq!(ecos, vec![Ecosystem::Worktrees, Ecosystem::Node]);
    }
}
