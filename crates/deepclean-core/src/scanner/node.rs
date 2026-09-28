use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytesize::ByteSize;
use tracing::debug;
use uuid::Uuid;

use crate::error::ScanError;
use crate::model::*;
use crate::scanner::EcosystemScanner;
use crate::staleness;

pub struct NodeScanner;

/// Detected package manager for a Node.js project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
}

impl PackageManager {
    /// Detect the package manager from lock files in the project root.
    fn detect(project_root: &Path) -> PackageManager {
        if project_root.join("pnpm-lock.yaml").exists() {
            PackageManager::Pnpm
        } else if project_root.join("yarn.lock").exists() {
            PackageManager::Yarn
        } else {
            // Default to npm (package-lock.json or no lock file)
            PackageManager::Npm
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Yarn => "yarn",
            Self::Pnpm => "pnpm",
        }
    }
}

/// Framework build caches that live beside `package.json` and are rebuilt by
/// the next `dev`/`build`. `.angular` is claimed as a whole but only its
/// `cache/` is removed: the rest can hold per-developer config.
const FRAMEWORK_CACHES: [&str; 7] = [
    ".next",
    ".turbo",
    ".parcel-cache",
    ".svelte-kit",
    ".nuxt",
    ".angular",
    ".expo",
];

/// Lock files that mark a directory as a Node project root even without a
/// `package.json` beside the `node_modules` (e.g. a half-deleted checkout).
const LOCK_FILES: [&str; 5] = [
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "bun.lock",
];

fn is_node_project_dir(dir: &Path) -> bool {
    dir.join("package.json").is_file() || LOCK_FILES.iter().any(|f| dir.join(f).is_file())
}

fn action(
    label: impl Into<String>,
    description: impl Into<String>,
    method: ActionMethod,
    risk: RiskLevel,
    estimated_savings_bytes: u64,
) -> CleanAction {
    CleanAction {
        id: Uuid::new_v4(),
        label: label.into(),
        description: description.into(),
        method,
        risk,
        estimated_savings_bytes,
    }
}

#[async_trait]
impl EcosystemScanner for NodeScanner {
    fn ecosystem(&self) -> Ecosystem {
        Ecosystem::Node
    }

    fn is_candidate(&self, file_name: &str, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        match file_name {
            "node_modules" => is_node_project_dir(parent),
            name if FRAMEWORK_CACHES.contains(&name) => parent.join("package.json").is_file(),
            _ => false,
        }
    }

    async fn analyze(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        if FRAMEWORK_CACHES.contains(&file_name.as_str()) {
            return self.analyze_framework_cache(path, &file_name).await;
        }

        // For global cache locations, handle them differently
        if file_name != "node_modules" {
            return self.analyze_global_cache(path).await;
        }

        let Some(parent) = path.parent() else {
            return Ok(None);
        };

        if !is_node_project_dir(parent) {
            return Ok(None);
        }

        let project_name = parent
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());

        let pkg_manager = PackageManager::detect(parent);
        debug!(
            "Node project '{}' uses {} ({})",
            project_name,
            pkg_manager.name(),
            path.display()
        );

        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        // No package-manager cache commands here: they clean the *global*
        // cache, which is its own item with a measured size. Offering them on
        // every `node_modules` either showed an estimate of 0 or, if measured,
        // counted the same cache once per project.
        let actions = vec![action(
            "Remove node_modules/",
            "Delete the entire node_modules directory",
            ActionMethod::RemoveDir {
                path: path.to_path_buf(),
            },
            RiskLevel::Safe,
            size_bytes,
        )];

        Ok(Some(CleanableItem {
            details: vec![Detail::new("Package manager", pkg_manager.name())],
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Node,
            kind: ArtifactKind::NodeModules,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: Some(project_name),
            project_root: Some(parent.to_path_buf()),
            available_actions: actions,
        }))
    }

    fn global_locations(&self) -> Vec<PathBuf> {
        super::existing_home_dirs([
            ".npm/_cacache",                // npm
            ".cache/yarn",                  // Yarn v1
            "Library/Caches/Yarn",          // Yarn, macOS
            ".local/share/pnpm/store",      // pnpm
            "Library/pnpm/store",           // pnpm, macOS
            ".bun/install/cache",           // Bun
            "Library/Caches/ms-playwright", // Playwright browsers, macOS
            ".cache/ms-playwright",         // Playwright browsers, Linux
            "AppData/Local/ms-playwright",  // Playwright browsers, Windows
            ".cache/puppeteer",             // Puppeteer browsers
        ])
    }
}

/// A versioned browser build directory, e.g. `chromium-1140` (Playwright) or
/// `mac_arm-121.0.6167.85` (Puppeteer).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserBuild {
    /// What the version is compared within: `chromium`, `chrome/mac_arm`.
    browser: String,
    version: Vec<u64>,
    path: PathBuf,
}

/// Split `name-1.2.3` into (`name`, [1, 2, 3]). `None` when the suffix is not
/// a dotted number — Playwright MCP's `mcp-chrome-ac81000` profile dir is not
/// a build and must never be treated as an old one.
fn parse_build_name(name: &str) -> Option<(&str, Vec<u64>)> {
    let (browser, version) = name.rsplit_once('-')?;
    if browser.is_empty() || version.is_empty() {
        return None;
    }
    let parts: Option<Vec<u64>> = version.split('.').map(|p| p.parse().ok()).collect();
    Some((browser, parts?))
}

/// Every build except the newest of each browser.
fn old_builds(builds: &[BrowserBuild]) -> Vec<PathBuf> {
    let mut newest: std::collections::HashMap<&str, &Vec<u64>> = std::collections::HashMap::new();
    for b in builds {
        let entry = newest.entry(b.browser.as_str()).or_insert(&b.version);
        if b.version > **entry {
            *entry = &b.version;
        }
    }
    let mut old: Vec<PathBuf> = builds
        .iter()
        .filter(|b| {
            newest
                .get(b.browser.as_str())
                .is_some_and(|v| **v != b.version)
        })
        .map(|b| b.path.clone())
        .collect();
    old.sort();
    old
}

/// Visible subdirectories of `dir`, sorted by name.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    (!name.starts_with('.')).then(|| (name, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Playwright keeps `<browser>-<revision>` directly under its cache root.
/// Returns the builds and the names of anything else found there.
fn playwright_builds(root: &Path) -> (Vec<BrowserBuild>, Vec<String>) {
    let mut builds = Vec::new();
    let mut other = Vec::new();
    for (name, path) in subdirs(root) {
        match parse_build_name(&name) {
            Some((browser, version)) => builds.push(BrowserBuild {
                browser: browser.to_string(),
                version,
                path,
            }),
            None => other.push(name),
        }
    }
    (builds, other)
}

/// Puppeteer keeps `<browser>/<platform>-<version>` under its cache root.
fn puppeteer_builds(root: &Path) -> (Vec<BrowserBuild>, Vec<String>) {
    let mut builds = Vec::new();
    let mut other = Vec::new();
    for (browser_dir, browser_path) in subdirs(root) {
        for (name, path) in subdirs(&browser_path) {
            match parse_build_name(&name) {
                Some((platform, version)) => builds.push(BrowserBuild {
                    browser: format!("{browser_dir}/{platform}"),
                    version,
                    path,
                }),
                None => other.push(format!("{browser_dir}/{name}")),
            }
        }
    }
    (builds, other)
}

impl NodeScanner {
    /// `.next`, `.turbo`, `.svelte-kit`… — framework build output and caches
    /// rebuilt on the next dev server start or build.
    async fn analyze_framework_cache(
        &self,
        path: &Path,
        dir_name: &str,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let Some(parent) = path.parent() else {
            return Ok(None);
        };
        // `.angular` also holds non-cache state; only its cache is claimed.
        let target = if dir_name == ".angular" {
            path.join("cache")
        } else {
            path.to_path_buf()
        };
        if !target.is_dir() {
            return Ok(None);
        }

        let size_bytes = staleness::compute_dir_size(&target).await;
        if size_bytes == 0 {
            return Ok(None);
        }
        let last_modified = staleness::most_recent_modification(&target);
        let shown = if dir_name == ".angular" {
            ".angular/cache"
        } else {
            dir_name
        };

        Ok(Some(CleanableItem {
            details: vec![Detail::new("Cache", shown)],
            agent: None,
            id: Uuid::new_v4(),
            path: target.clone(),
            ecosystem: Ecosystem::Node,
            kind: ArtifactKind::FrameworkBuildCache,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale: last_modified.map(staleness::days_since),
            project_name: parent.file_name().map(|n| n.to_string_lossy().to_string()),
            project_root: Some(parent.to_path_buf()),
            available_actions: vec![action(
                format!("Remove {shown}/"),
                format!("Delete {shown} (rebuilt on the next dev or build run)"),
                ActionMethod::RemoveDir { path: target },
                RiskLevel::Safe,
                size_bytes,
            )],
        }))
    }

    /// Headless browsers downloaded by Playwright or Puppeteer. Agents that
    /// drive a browser (Playwright MCP, computer-use harnesses) pull a new
    /// build with every version bump and never remove the old one.
    async fn analyze_browser_cache(
        &self,
        path: &Path,
        is_playwright: bool,
    ) -> Result<Option<CleanableItem>, ScanError> {
        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }
        let last_modified = staleness::most_recent_modification(path);

        let (builds, other) = if is_playwright {
            playwright_builds(path)
        } else {
            puppeteer_builds(path)
        };
        let old = old_builds(&builds);
        let mut old_size = 0;
        let mut builds_size = 0;
        for b in &builds {
            let size = staleness::compute_dir_size(&b.path).await;
            builds_size += size;
            if old.contains(&b.path) {
                old_size += size;
            }
        }

        let mut details: Vec<Detail> = builds
            .iter()
            .map(|b| {
                let name = b
                    .path
                    .strip_prefix(path)
                    .unwrap_or(&b.path)
                    .to_string_lossy()
                    .to_string();
                let state = if old.contains(&b.path) {
                    "old"
                } else {
                    "newest"
                };
                Detail::new("Build", format!("{name} ({state})"))
            })
            .collect();
        if !other.is_empty() {
            details.push(Detail::new("Other data (kept)", other.join(", ")));
        }

        let mut actions = Vec::new();
        if !old.is_empty() && old_size > 0 {
            actions.push(action(
                "Remove old browser builds",
                format!(
                    "Delete {} superseded build(s), keeping the newest of each browser",
                    old.len()
                ),
                ActionMethod::RemoveDirs { paths: old },
                RiskLevel::Safe,
                old_size,
            ));
        }
        if is_playwright {
            actions.push(action(
                "npx playwright uninstall --all",
                "Run `npx playwright uninstall --all` to remove every Playwright browser",
                ActionMethod::Command {
                    program: "npx".into(),
                    args: vec!["playwright".into(), "uninstall".into(), "--all".into()],
                    working_dir: None,
                },
                RiskLevel::Safe,
                // Removes browser builds only, not other data beside them.
                builds_size,
            ));
        }
        // Anything that is not a build (Playwright MCP keeps a browser
        // profile — cookies, logins — in `mcp-chrome-*`) makes removing the
        // whole directory a real loss, not a re-download.
        let whole_risk = if other.is_empty() {
            RiskLevel::Safe
        } else {
            RiskLevel::Caution
        };
        actions.push(action(
            "Remove all browsers",
            format!(
                "Delete {} (browsers are re-downloaded on the next install)",
                path.display()
            ),
            ActionMethod::RemoveDir {
                path: path.to_path_buf(),
            },
            whole_risk,
            size_bytes,
        ));

        let (kind, name) = if is_playwright {
            (ArtifactKind::PlaywrightBrowsers, "Playwright browsers")
        } else {
            (ArtifactKind::PuppeteerBrowsers, "Puppeteer browsers")
        };

        Ok(Some(CleanableItem {
            details,
            agent: Some(
                if is_playwright {
                    "playwright"
                } else {
                    "puppeteer"
                }
                .into(),
            ),
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Node,
            kind,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale: last_modified.map(staleness::days_since),
            project_name: Some(name.into()),
            project_root: None,
            available_actions: actions,
        }))
    }

    /// Analyze a global cache directory (npm cache, yarn cache, pnpm store,
    /// Bun cache, browser downloads).
    async fn analyze_global_cache(&self, path: &Path) -> Result<Option<CleanableItem>, ScanError> {
        if !path.is_dir() {
            return Ok(None);
        }

        if path.ends_with("ms-playwright") {
            return self.analyze_browser_cache(path, true).await;
        }
        if path.ends_with("puppeteer") {
            return self.analyze_browser_cache(path, false).await;
        }

        let size_bytes = staleness::compute_dir_size(path).await;
        if size_bytes == 0 {
            return Ok(None);
        }

        let last_modified = staleness::most_recent_modification(path);
        let days_stale = last_modified.map(staleness::days_since);

        let path_str = path.to_string_lossy();

        let command = |program: &str, args: &[&str]| ActionMethod::Command {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            working_dir: None,
        };

        let (kind, label, description, method) = if path.ends_with(".bun/install/cache") {
            (
                ArtifactKind::BunCache,
                "bun pm cache rm",
                "Run `bun pm cache rm` to clear Bun's global package cache",
                command("bun", &["pm", "cache", "rm"]),
            )
        } else if path_str.contains(".npm") {
            (
                ArtifactKind::NpmCache,
                "npm cache clean",
                "Run `npm cache clean --force` to clear npm cache",
                command("npm", &["cache", "clean", "--force"]),
            )
        } else if path_str.contains("yarn") || path_str.contains("Yarn") {
            (
                ArtifactKind::YarnCache,
                "yarn cache clean",
                "Run `yarn cache clean` to clear Yarn cache",
                command("yarn", &["cache", "clean"]),
            )
        } else {
            (
                ArtifactKind::PnpmStore,
                "pnpm store prune",
                "Run `pnpm store prune` to prune the pnpm store",
                command("pnpm", &["store", "prune"]),
            )
        };

        let mut actions = vec![action(
            label,
            description,
            method,
            RiskLevel::Safe,
            size_bytes,
        )];
        if kind == ArtifactKind::BunCache {
            actions.push(action(
                "Remove directory",
                format!("Delete {}", path.display()),
                ActionMethod::RemoveDir {
                    path: path.to_path_buf(),
                },
                RiskLevel::Safe,
                size_bytes,
            ));
        }

        Ok(Some(CleanableItem {
            details: Vec::new(),
            agent: None,
            id: Uuid::new_v4(),
            path: path.to_path_buf(),
            ecosystem: Ecosystem::Node,
            kind,
            risk: RiskLevel::Safe,
            size_bytes,
            size_display: ByteSize(size_bytes).to_string(),
            last_modified,
            days_stale,
            project_name: None,
            project_root: None,
            available_actions: actions,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_candidate_node_modules() {
        let scanner = NodeScanner;
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp.path().join("myapp/node_modules");
        std::fs::create_dir_all(&nm).unwrap();
        // A stray node_modules with no project beside it is not claimed.
        assert!(!scanner.is_candidate("node_modules", &nm));
        std::fs::write(tmp.path().join("myapp/package.json"), "{}").unwrap();
        assert!(scanner.is_candidate("node_modules", &nm));
    }

    #[test]
    fn node_modules_beside_only_a_lockfile_is_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp.path().join("app/node_modules");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(tmp.path().join("app/pnpm-lock.yaml"), "").unwrap();
        assert!(NodeScanner.is_candidate("node_modules", &nm));
    }

    #[test]
    fn framework_caches_need_a_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("web");
        std::fs::create_dir_all(app.join(".next")).unwrap();
        assert!(!NodeScanner.is_candidate(".next", &app.join(".next")));
        std::fs::write(app.join("package.json"), "{}").unwrap();
        for name in FRAMEWORK_CACHES {
            assert!(NodeScanner.is_candidate(name, &app.join(name)), "{name}");
        }
    }

    #[tokio::test]
    async fn analyze_next_cache_is_a_safe_framework_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("web");
        std::fs::create_dir_all(app.join(".next/cache")).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(app.join(".next/cache/x"), vec![0u8; 256]).unwrap();

        let item = NodeScanner
            .analyze(&app.join(".next"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, ArtifactKind::FrameworkBuildCache);
        assert_eq!(item.risk, RiskLevel::Safe);
        assert_eq!(item.project_name.as_deref(), Some("web"));
        assert_eq!(item.size_bytes, 256);
        assert!(matches!(
            &item.available_actions[0].method,
            ActionMethod::RemoveDir { path } if path == &app.join(".next")
        ));
    }

    #[tokio::test]
    async fn angular_only_its_cache_subdir_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("ng");
        std::fs::create_dir_all(app.join(".angular/cache/18")).unwrap();
        std::fs::write(app.join("package.json"), "{}").unwrap();
        std::fs::write(app.join(".angular/cache/18/x"), vec![0u8; 10]).unwrap();
        std::fs::write(app.join(".angular/other.json"), "{}").unwrap();

        let item = NodeScanner
            .analyze(&app.join(".angular"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(item.path, app.join(".angular/cache"));
        assert!(matches!(
            &item.available_actions[0].method,
            ActionMethod::RemoveDir { path } if path == &app.join(".angular/cache")
        ));
    }

    #[tokio::test]
    async fn node_modules_item_has_no_global_cache_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        std::fs::write(project.join("package.json"), "{}").unwrap();
        std::fs::write(project.join("node_modules/x"), "x").unwrap();
        let item = NodeScanner
            .analyze(&project.join("node_modules"))
            .await
            .unwrap()
            .unwrap();
        assert!(item
            .available_actions
            .iter()
            .all(|a| !matches!(a.method, ActionMethod::Command { .. })));
        assert!(item
            .available_actions
            .iter()
            .all(|a| a.estimated_savings_bytes == item.size_bytes));
    }

    #[tokio::test]
    async fn global_npm_cache_estimate_is_measured_size() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join(".npm/_cacache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("blob"), vec![0u8; 700]).unwrap();
        let item = NodeScanner.analyze(&cache).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::NpmCache);
        assert_eq!(item.available_actions[0].estimated_savings_bytes, 700);
    }

    #[tokio::test]
    async fn bun_cache_offers_bun_command_then_remove() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join(".bun/install/cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("pkg.tgz"), vec![0u8; 50]).unwrap();
        let item = NodeScanner.analyze(&cache).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::BunCache);
        match &item.available_actions[0].method {
            ActionMethod::Command { program, args, .. } => {
                assert_eq!(program, "bun");
                assert_eq!(args, &["pm", "cache", "rm"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            &item.available_actions[1].method,
            ActionMethod::RemoveDir { path } if path == &cache
        ));
    }

    #[test]
    fn parse_build_names() {
        assert_eq!(
            parse_build_name("chromium-1140"),
            Some(("chromium", vec![1140]))
        );
        assert_eq!(
            parse_build_name("chromium_headless_shell-1217"),
            Some(("chromium_headless_shell", vec![1217]))
        );
        assert_eq!(
            parse_build_name("mac_arm-121.0.6167.85"),
            Some(("mac_arm", vec![121, 0, 6167, 85]))
        );
        // Playwright MCP's browser profile: hex suffix, not a build.
        assert_eq!(parse_build_name("mcp-chrome-ac81000"), None);
        assert_eq!(parse_build_name("chromium"), None);
    }

    #[test]
    fn old_builds_keeps_newest_per_browser() {
        let b = |browser: &str, v: Vec<u64>, p: &str| BrowserBuild {
            browser: browser.into(),
            version: v,
            path: PathBuf::from(p),
        };
        let builds = vec![
            b("chromium", vec![1208], "/c/chromium-1208"),
            b("chromium", vec![1217], "/c/chromium-1217"),
            b("chromium", vec![1140], "/c/chromium-1140"),
            b("firefox", vec![1466], "/c/firefox-1466"),
            b(
                "chrome/mac_arm",
                vec![121, 0, 1],
                "/p/chrome/mac_arm-121.0.1",
            ),
            b(
                "chrome/mac_arm",
                vec![121, 0, 10],
                "/p/chrome/mac_arm-121.0.10",
            ),
        ];
        assert_eq!(
            old_builds(&builds),
            vec![
                PathBuf::from("/c/chromium-1140"),
                PathBuf::from("/c/chromium-1208"),
                PathBuf::from("/p/chrome/mac_arm-121.0.1"),
            ]
        );
    }

    #[tokio::test]
    async fn playwright_cache_removes_old_builds_first_and_spares_mcp_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Library/Caches/ms-playwright");
        for d in [
            "chromium-1208",
            "chromium-1217",
            "ffmpeg-1011",
            "mcp-chrome-ac81000",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("blob"), vec![0u8; 100]).unwrap();
        }

        let item = NodeScanner.analyze(&root).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::PlaywrightBrowsers);
        assert_eq!(item.agent.as_deref(), Some("playwright"));
        let first = &item.available_actions[0];
        assert_eq!(first.label, "Remove old browser builds");
        assert_eq!(first.risk, RiskLevel::Safe);
        assert_eq!(first.estimated_savings_bytes, 100);
        match &first.method {
            ActionMethod::RemoveDirs { paths } => {
                assert_eq!(paths, &vec![root.join("chromium-1208")]);
            }
            other => panic!("{other:?}"),
        }
        match &item.available_actions[1].method {
            ActionMethod::Command { program, args, .. } => {
                assert_eq!(program, "npx");
                assert_eq!(args, &["playwright", "uninstall", "--all"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(item.available_actions[1].estimated_savings_bytes, 300);
        let whole = item.available_actions.last().unwrap();
        assert!(matches!(&whole.method, ActionMethod::RemoveDir { path } if path == &root));
        // The MCP profile would be lost with the whole directory.
        assert_eq!(whole.risk, RiskLevel::Caution);
    }

    #[tokio::test]
    async fn puppeteer_cache_groups_by_browser_and_platform() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".cache/puppeteer");
        for d in [
            "chrome/mac_arm-120.0.6099.109",
            "chrome/mac_arm-121.0.6167.85",
            "chrome-headless-shell/mac_arm-121.0.6167.85",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("blob"), vec![0u8; 10]).unwrap();
        }
        let item = NodeScanner.analyze(&root).await.unwrap().unwrap();
        assert_eq!(item.kind, ArtifactKind::PuppeteerBrowsers);
        match &item.available_actions[0].method {
            ActionMethod::RemoveDirs { paths } => {
                assert_eq!(paths, &vec![root.join("chrome/mac_arm-120.0.6099.109")]);
            }
            other => panic!("{other:?}"),
        }
        let whole = item.available_actions.last().unwrap();
        assert_eq!(whole.risk, RiskLevel::Safe);
    }

    #[test]
    fn is_candidate_rejects_other_dirs() {
        let scanner = NodeScanner;
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("myapp/target");
        assert!(!scanner.is_candidate("target", &target));
        assert!(!scanner.is_candidate("src", &tmp.path().join("src")));
    }

    #[test]
    fn detect_npm_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        assert_eq!(PackageManager::detect(tmp.path()), PackageManager::Npm);
    }

    #[test]
    fn detect_yarn_from_lock() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("yarn.lock"), "").unwrap();
        assert_eq!(PackageManager::detect(tmp.path()), PackageManager::Yarn);
    }

    #[test]
    fn detect_pnpm_from_lock() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(PackageManager::detect(tmp.path()), PackageManager::Pnpm);
    }

    #[tokio::test]
    async fn analyze_node_modules_with_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("node_modules/.package-lock.json")).unwrap();
        std::fs::write(project.join("package.json"), r#"{"name": "myapp"}"#).unwrap();
        std::fs::write(
            project.join("node_modules/some-package"),
            "fake package content",
        )
        .unwrap();

        let scanner = NodeScanner;
        let result = scanner
            .analyze(&project.join("node_modules"))
            .await
            .unwrap();
        assert!(result.is_some());

        let item = result.unwrap();
        assert_eq!(item.ecosystem, Ecosystem::Node);
        assert_eq!(item.kind, ArtifactKind::NodeModules);
        assert!(item.size_bytes > 0);
        assert_eq!(item.project_name.as_deref(), Some("myapp"));
        assert!(!item.available_actions.is_empty());
    }

    #[tokio::test]
    async fn analyze_node_modules_without_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("myapp");
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        std::fs::write(
            project.join("node_modules/some-package"),
            "fake package content",
        )
        .unwrap();

        let scanner = NodeScanner;
        let result = scanner
            .analyze(&project.join("node_modules"))
            .await
            .unwrap();
        assert!(result.is_none());
    }
}
