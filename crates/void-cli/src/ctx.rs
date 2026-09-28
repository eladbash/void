//! Where the CLI reads and writes, resolved once from the global flags.
//!
//! Two modes:
//!
//! - **Normal**: the real home, the desktop app's config and history (so the
//!   GUI's History screen shows CLI and MCP cleans), the system Trash.
//! - **Sandbox** (`--home <dir>`): every home-relative lookup is pointed at
//!   `<dir>`, the scan is confined to it, and config, history, plans and the
//!   Trash all live inside it. Nothing a sandbox run does can reach the real
//!   home, the real app settings or the real Trash.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use deepclean_core::action::ActionExecutor;
use deepclean_core::config::AppConfig;
use deepclean_core::guard::{self, GuardStatus};
use deepclean_core::history::{CleanRun, History};
use deepclean_core::model::{CleanableItem, Ecosystem, ScanEvent};
use deepclean_core::plan::{self, Plan, PlanFilter, PlanStore, PLAN_TTL};
use deepclean_core::safety::SafetyChecker;
use deepclean_core::scanner::{registry, ScanOrchestrator};
use deepclean_core::trash::TrashBackend;

pub use deepclean_core::sandbox::{confine, SANDBOX_CONFIG, SANDBOX_PLANS, SANDBOX_TRASH};

/// The desktop app's bundle identifier; its config directory is named after it.
const APP_ID: &str = "com.void.app";

pub struct Ctx {
    pub home: PathBuf,
    pub sandbox: bool,
    pub config: AppConfig,
    pub config_path: PathBuf,
    pub history_path: PathBuf,
    pub plans: PlanStore,
    pub trash: TrashBackend,
    pub json: bool,
    /// Warning from loading the config, reported once to stderr.
    pub config_warning: Option<String>,
}

impl Ctx {
    pub fn new(home: Option<PathBuf>, config: Option<PathBuf>, json: bool) -> Result<Self, String> {
        let sandbox = home.is_some();
        let home = match home {
            Some(dir) => {
                let dir = absolute(&dir);
                if !dir.is_dir() {
                    return Err(format!(
                        "--home {} is not a directory (create it, or run `void dev seed {}`)",
                        dir.display(),
                        dir.display()
                    ));
                }
                let dir = dir.canonicalize().unwrap_or(dir);
                // Before anything reads home — including AppConfig::default.
                deepclean_core::paths::set_home_override(&dir);
                dir
            }
            None => deepclean_core::paths::home_dir()
                .ok_or("could not determine the home directory; pass --home")?,
        };

        let config_path = match config {
            Some(path) => absolute(&path),
            None if sandbox => home.join(SANDBOX_CONFIG).join(AppConfig::FILE_NAME),
            None => dirs::config_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(APP_ID)
                .join(AppConfig::FILE_NAME),
        };
        let history_path = config_path
            .parent()
            .map(|dir| dir.join(History::FILE_NAME))
            .unwrap_or_else(|| PathBuf::from(History::FILE_NAME));

        let (mut config, config_warning) = AppConfig::load(&config_path);
        if sandbox {
            config.scan_roots = vec![home.clone()];
        }

        let plan_dir = match std::env::var_os("VOID_PLAN_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ if sandbox => home.join(SANDBOX_PLANS),
            _ => dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("void")
                .join("plans"),
        };

        let trash = if sandbox {
            TrashBackend::Directory(home.join(SANDBOX_TRASH))
        } else {
            TrashBackend::System
        };

        Ok(Self {
            home,
            sandbox,
            config,
            config_path,
            history_path,
            plans: PlanStore::new(plan_dir),
            trash,
            json,
            config_warning,
        })
    }

    pub fn safety(&self) -> SafetyChecker {
        SafetyChecker::with_home(self.home.clone(), self.config.blocked_paths.clone())
    }

    pub fn executor(&self) -> ActionExecutor {
        ActionExecutor::with_trash(self.safety(), self.trash.clone())
    }

    /// Scan with every enabled ecosystem, or only `only` when non-empty.
    /// Progress goes to stderr when `progress` is set.
    pub async fn scan(&self, only: &[Ecosystem], progress: bool) -> Vec<CleanableItem> {
        let mut config = self.config.clone();
        if !only.is_empty() {
            config.enabled_ecosystems = only.to_vec();
        }
        let scanners = registry::build_with_home(&config, self.home.clone());
        let enabled = config.enabled_ecosystems.clone();
        let tty = std::io::stderr().is_terminal();
        let start = std::time::Instant::now();

        if progress {
            eprintln!(
                "Scanning {} …",
                config
                    .scan_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
        let mut items: Vec<CleanableItem> = Vec::new();
        let mut bytes = 0u64;
        while let Some(event) = rx.recv().await {
            match event {
                ScanEvent::ItemFound { item } => {
                    bytes += item.size_bytes;
                    items.push(item);
                    if progress && tty {
                        eprint!(
                            "\r  {} items, {} so far",
                            items.len(),
                            bytesize::ByteSize(bytes)
                        );
                    }
                }
                ScanEvent::Progress { message, .. } if progress => {
                    if tty {
                        eprint!("\r  {message}");
                    } else {
                        eprintln!("  {message}");
                    }
                }
                ScanEvent::PermissionDenied { path } if progress => {
                    if tty {
                        eprintln!();
                    }
                    eprintln!("  could not read {}", path.display());
                }
                ScanEvent::ScanComplete { .. } => break,
                _ => {}
            }
        }

        items.retain(|i| enabled.contains(&i.ecosystem));
        if self.sandbox {
            items = confine(items, &self.home);
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.size_bytes));

        if progress {
            if tty {
                eprint!("\r\x1b[2K");
            }
            let total: u64 = items.iter().map(|i| i.size_bytes).sum();
            eprintln!(
                "Found {} items ({}) in {:.1}s",
                items.len(),
                bytesize::ByteSize(total),
                start.elapsed().as_secs_f64()
            );
        }
        items
    }

    /// Build a plan, save it, and clear out expired ones.
    pub fn save_plan(&self, items: &[CleanableItem], filter: &PlanFilter) -> Result<Plan, String> {
        let plan = plan::build_plan(items, filter);
        self.plans.save(&plan)?;
        self.plans.prune_older_than(PLAN_TTL);
        Ok(plan)
    }

    /// Load a plan and check it may be applied here: built against this
    /// home, and nothing in it above Caution (Danger stays in the desktop
    /// app, behind typed confirmation).
    pub fn load_applicable_plan(&self, id: &str) -> Result<Plan, String> {
        let id = uuid::Uuid::parse_str(id.trim())
            .map_err(|_| format!("`{id}` is not a plan id (expected a UUID from `void plan`)"))?;
        let plan = self.plans.load(id)?;
        let plan_home = plan
            .home
            .canonicalize()
            .unwrap_or_else(|_| plan.home.clone());
        let here = self
            .home
            .canonicalize()
            .unwrap_or_else(|_| self.home.clone());
        if plan_home != here {
            return Err(format!(
                "plan {id} was built for home {}, not {}; build a new plan here",
                plan.home.display(),
                self.home.display()
            ));
        }
        if plan.highest_risk() > deepclean_core::model::RiskLevel::Caution {
            return Err(format!(
                "plan {id} contains a Danger action; those run only from the Void app, with typed confirmation"
            ));
        }
        Ok(plan)
    }

    /// Append a run to the history file the desktop app reads.
    pub fn record_history(&self, mut run: CleanRun, trigger: &str) -> Result<(), String> {
        if run.items.is_empty() {
            return Ok(());
        }
        run.trigger = Some(trigger.to_string());
        let (mut history, warning) = History::load(&self.history_path);
        if let Some(warning) = warning {
            // A corrupt history must not be overwritten with a one-run file.
            return Err(format!("not recording this clean: {warning}"));
        }
        history.push(run);
        history
            .save(&self.history_path)
            .map_err(|err| format!("could not save {}: {err}", self.history_path.display()))
    }

    /// Guard verdict for the volume holding `path` (home when `None`).
    pub fn guard_status(&self, path: Option<&Path>) -> Option<GuardStatus> {
        let path = path.unwrap_or(&self.home);
        let usage = deepclean_core::disk::usage_for_path(path)?;
        Some(guard::evaluate(&usage, &self.config.guard))
    }
}

/// Resolve a user-supplied path against the current directory.
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}
