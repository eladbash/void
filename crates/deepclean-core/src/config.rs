use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Ecosystem;

/// How the results list is grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Grouping {
    #[default]
    Ecosystem,
    Project,
    Risk,
}

/// Row density in the results list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

/// Which color theme to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// Presentation preferences. These never affect what a scan finds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub theme: Theme,
    pub density: Density,
    pub grouping: Grouping,
    /// Show the pre-clean review dialog. Turning this off skips the risk review.
    pub confirm_before_cleaning: bool,
    /// When an item offers both a Trash action and a permanent one, pick the Trash action.
    pub prefer_trash: bool,
    pub launch_at_login: bool,
    pub show_menu_bar_icon: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            density: Density::default(),
            grouping: Grouping::default(),
            confirm_before_cleaning: true,
            prefer_trash: true,
            launch_at_login: false,
            show_menu_bar_icon: true,
        }
    }
}

/// Application configuration.
///
/// `#[serde(default)]` is load-bearing: it lets a config written by an older
/// build gain new fields without failing to parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Root directories to scan for projects.
    pub scan_roots: Vec<PathBuf>,
    /// Paths to never touch.
    pub blocked_paths: Vec<PathBuf>,
    /// Which ecosystems to scan.
    pub enabled_ecosystems: Vec<Ecosystem>,
    /// Days since last modification before an artifact is *displayed* as stale.
    ///
    /// This is a presentation threshold only — it does not change what a scan
    /// walks or what it returns.
    pub staleness_threshold_days: u64,
    /// Max CPU usage percentage for background scanning.
    pub cpu_threshold_percent: u8,
    /// Number of threads for the parallel directory walker.
    pub walker_threads: usize,
    /// Max concurrent analysis tasks.
    pub max_concurrent_analyses: usize,
    /// Presentation preferences.
    pub ui: UiPrefs,
}

impl Default for AppConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        Self {
            scan_roots: vec![home],
            blocked_paths: Vec::new(),
            enabled_ecosystems: vec![
                Ecosystem::Rust,
                Ecosystem::Node,
                Ecosystem::Apple,
                Ecosystem::Docker,
                Ecosystem::Go,
                Ecosystem::System,
                Ecosystem::Python,
                Ecosystem::Java,
                Ecosystem::Homebrew,
                Ecosystem::JetBrains,
                Ecosystem::DotNet,
            ],
            staleness_threshold_days: 30,
            cpu_threshold_percent: 70,
            walker_threads: (cpus / 2).max(1),
            max_concurrent_analyses: 8,
            ui: UiPrefs::default(),
        }
    }
}

impl AppConfig {
    /// Read config from `path`.
    ///
    /// A missing or unreadable file yields defaults. So does a corrupt one —
    /// failing to launch because a JSON file got truncated would be a worse
    /// outcome than losing preferences, so the error is returned alongside the
    /// defaults for the caller to log rather than propagated.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None)
            }
            Err(err) => {
                return (
                    Self::default(),
                    Some(format!("could not read {}: {err}", path.display())),
                )
            }
        };

        match serde_json::from_str::<Self>(&raw) {
            Ok(config) => (config.sanitized(), None),
            Err(err) => (
                Self::default(),
                Some(format!("could not parse {}: {err}", path.display())),
            ),
        }
    }

    /// Write config to `path`, creating parent directories as needed.
    ///
    /// Writes to a sibling temp file and renames, so a crash mid-write leaves
    /// the previous config intact rather than a half-written one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Clamp values that would break a scan rather than merely tune it.
    ///
    /// The config file is plain JSON that users are invited to edit, so these
    /// are enforced on the way in rather than trusted from the UI. Zero
    /// concurrency in particular is not "slow" — it is a permanent deadlock on
    /// an empty semaphore.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.max_concurrent_analyses = self.max_concurrent_analyses.max(1);
        self.walker_threads = self.walker_threads.max(1);
        self.staleness_threshold_days = self.staleness_threshold_days.max(1);
        self.cpu_threshold_percent = self.cpu_threshold_percent.clamp(1, 100);
        self
    }

    /// Config file name within the app config directory.
    pub const FILE_NAME: &'static str = "config.json";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults_without_error() {
        let tmp = tempfile::tempdir().unwrap();
        let (config, err) = AppConfig::load(&tmp.path().join("nope.json"));
        assert!(err.is_none());
        assert_eq!(config.staleness_threshold_days, 30);
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");

        let config = AppConfig {
            staleness_threshold_days: 90,
            blocked_paths: vec![PathBuf::from("/keep/this")],
            ui: UiPrefs {
                density: Density::Compact,
                ..UiPrefs::default()
            },
            ..AppConfig::default()
        };
        config.save(&path).unwrap();

        let (loaded, err) = AppConfig::load(&path);
        assert!(err.is_none());
        assert_eq!(loaded.staleness_threshold_days, 90);
        assert_eq!(loaded.blocked_paths, vec![PathBuf::from("/keep/this")]);
        assert_eq!(loaded.ui.density, Density::Compact);
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/deeper/config.json");
        AppConfig::default().save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn partial_config_fills_missing_fields_from_defaults() {
        // A config written before a field existed must still load.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{"staleness_threshold_days": 7}"#).unwrap();

        let (config, err) = AppConfig::load(&path);
        assert!(err.is_none(), "partial config should parse: {err:?}");
        assert_eq!(config.staleness_threshold_days, 7);
        assert_eq!(config.max_concurrent_analyses, 8);
        assert!(config.ui.confirm_before_cleaning);
        assert_eq!(config.enabled_ecosystems.len(), 11);
    }

    #[test]
    fn corrupt_config_falls_back_to_defaults_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let (config, err) = AppConfig::load(&path);
        assert!(err.is_some(), "corrupt config should report a problem");
        assert_eq!(config.staleness_threshold_days, 30);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        AppConfig::default().save(&path).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "found temp files: {leftovers:?}");
    }

    #[test]
    fn overwriting_preserves_previous_on_parse_of_new() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");

        AppConfig {
            staleness_threshold_days: 10,
            ..AppConfig::default()
        }
        .save(&path)
        .unwrap();

        AppConfig {
            staleness_threshold_days: 20,
            ..AppConfig::default()
        }
        .save(&path)
        .unwrap();

        let (loaded, _) = AppConfig::load(&path);
        assert_eq!(loaded.staleness_threshold_days, 20);
    }

    #[test]
    fn sanitizing_rejects_values_that_would_wedge_a_scan() {
        // Zero permits makes every acquire pend forever, which would hang the
        // scan rather than slow it.
        let config = AppConfig {
            max_concurrent_analyses: 0,
            walker_threads: 0,
            staleness_threshold_days: 0,
            cpu_threshold_percent: 0,
            ..AppConfig::default()
        }
        .sanitized();

        assert_eq!(config.max_concurrent_analyses, 1);
        assert_eq!(config.walker_threads, 1);
        assert_eq!(config.staleness_threshold_days, 1);
        assert_eq!(config.cpu_threshold_percent, 1);
    }

    #[test]
    fn a_hand_edited_zero_is_clamped_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.json");
        std::fs::write(&path, r#"{"max_concurrent_analyses": 0}"#).unwrap();

        let (config, err) = AppConfig::load(&path);
        assert!(err.is_none());
        assert_eq!(
            config.max_concurrent_analyses, 1,
            "a config edited by hand must not be able to deadlock the scanner"
        );
    }

    #[test]
    fn sanitizing_leaves_sensible_values_alone() {
        let config = AppConfig {
            max_concurrent_analyses: 8,
            walker_threads: 4,
            staleness_threshold_days: 30,
            cpu_threshold_percent: 70,
            ..AppConfig::default()
        }
        .sanitized();
        assert_eq!(config.max_concurrent_analyses, 8);
        assert_eq!(config.walker_threads, 4);
        assert_eq!(config.staleness_threshold_days, 30);
        assert_eq!(config.cpu_threshold_percent, 70);
    }
}
