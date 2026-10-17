//! The update check, as the UI sees it: when it is due and what to say.

use chrono::{DateTime, Duration, Utc};
use deepclean_core::config::UpdatePrefs;
use deepclean_core::update::{Update, Version};

/// The automatic check runs at most this often.
pub const CHECK_EVERY: Duration = Duration::hours(24);

/// Where the update check stands, for Settings → About.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking {
        manual: bool,
    },
    UpToDate {
        latest: String,
    },
    Available(Box<Update>),
    Failed(String),
}

impl UpdateStatus {
    /// One line for the About section.
    pub fn summary(&self, current: &str) -> String {
        match self {
            UpdateStatus::Idle => format!("Void {current}"),
            UpdateStatus::Checking { .. } => "Checking for updates…".into(),
            UpdateStatus::UpToDate { .. } => format!("Void {current} is the latest version"),
            UpdateStatus::Available(u) => format!("Void {} is available", u.version),
            UpdateStatus::Failed(err) => format!("Couldn't check for updates: {err}"),
        }
    }
}

/// Where installing an update stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum InstallState {
    #[default]
    Idle,
    Downloading {
        done: u64,
        total: u64,
    },
    Installing,
    /// Installed; the app is quitting so the new version can start.
    Restarting,
    /// Installed into a sandbox's fake target; nothing is restarted.
    Installed {
        path: String,
    },
    /// A `.deb`: the user installs it with this command.
    Manual {
        package: String,
        command: String,
    },
    Failed(String),
}

impl InstallState {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            InstallState::Downloading { .. } | InstallState::Installing | InstallState::Restarting
        )
    }
}

/// Whether the automatic check should run now.
pub fn check_due(prefs: &UpdatePrefs, now: DateTime<Utc>) -> bool {
    prefs.check_automatically
        && prefs
            .last_checked_at
            .is_none_or(|last| now - last >= CHECK_EVERY || last > now)
}

/// The newer release an earlier check found, unless the user skipped it:
/// what the update notice offers between checks.
pub fn pending_version(prefs: &UpdatePrefs, current: &str) -> Option<Version> {
    let latest = Version::parse(prefs.latest_version.as_deref()?).ok()?;
    let current = Version::parse(current).ok()?;
    let skipped = prefs
        .skipped_version
        .as_deref()
        .and_then(|v| Version::parse(v).ok());
    (latest > current && skipped.as_ref() != Some(&latest)).then_some(latest)
}

/// A newer release worth telling the user about: in the window's banner and
/// the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub version: Version,
    pub notes_url: Option<String>,
}

impl Notice {
    /// The tray menu item.
    pub fn tray_label(&self) -> String {
        format!("Update to v{}…", self.version)
    }
}

/// What to announce: this session's check, or failing that what an earlier
/// check found. Nothing for a skipped version or one that isn't newer.
pub fn notice(status: &UpdateStatus, prefs: &UpdatePrefs, current: &str) -> Option<Notice> {
    let found = match status {
        UpdateStatus::Available(u) => Notice {
            version: u.version.clone(),
            notes_url: Some(u.notes_url.clone()),
        },
        _ => Notice {
            version: pending_version(prefs, current)?,
            notes_url: prefs.latest_notes_url.clone(),
        },
    };
    let current = Version::parse(current).ok()?;
    let skipped = prefs.skipped_version.as_deref() == Some(found.version.to_string().as_str());
    (found.version > current && !skipped).then_some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(last: Option<DateTime<Utc>>) -> UpdatePrefs {
        UpdatePrefs {
            last_checked_at: last,
            ..Default::default()
        }
    }

    #[test]
    fn checks_once_a_day_when_on() {
        let now = Utc::now();
        assert!(check_due(&prefs(None), now));
        assert!(!check_due(&prefs(Some(now - Duration::hours(23))), now));
        assert!(check_due(&prefs(Some(now - Duration::hours(24))), now));
        // A clock that went backwards doesn't block checks forever.
        assert!(check_due(&prefs(Some(now + Duration::days(3))), now));
    }

    #[test]
    fn never_checks_when_off() {
        let off = UpdatePrefs {
            check_automatically: false,
            ..Default::default()
        };
        assert!(!check_due(&off, Utc::now()));
    }

    #[test]
    fn pending_version_respects_current_and_skipped() {
        let mut p = UpdatePrefs {
            latest_version: Some("1.3.0".into()),
            ..Default::default()
        };
        assert_eq!(pending_version(&p, "1.2.1"), Some(Version::new(1, 3, 0)));
        assert_eq!(pending_version(&p, "1.3.0"), None);
        p.skipped_version = Some("1.3.0".into());
        assert_eq!(pending_version(&p, "1.2.1"), None);
        p.latest_version = Some("1.4.0".into());
        assert_eq!(pending_version(&p, "1.2.1"), Some(Version::new(1, 4, 0)));
    }

    fn available(v: &str) -> UpdateStatus {
        UpdateStatus::Available(Box::new(Update {
            version: Version::parse(v).unwrap(),
            notes_url: format!("https://example.com/v{v}"),
            asset: None,
            manifest: None,
            signature: None,
        }))
    }

    #[test]
    fn announces_a_found_update_until_it_is_skipped() {
        let mut prefs = UpdatePrefs::default();
        let n = notice(&available("1.3.0"), &prefs, "1.2.1").unwrap();
        assert_eq!(n.tray_label(), "Update to v1.3.0…");
        assert_eq!(n.notes_url.as_deref(), Some("https://example.com/v1.3.0"));
        prefs.skipped_version = Some("1.3.0".into());
        assert_eq!(notice(&available("1.3.0"), &prefs, "1.2.1"), None);
        // A newer release than the skipped one is announced again.
        assert!(notice(&available("1.3.1"), &prefs, "1.2.1").is_some());
    }

    #[test]
    fn remembers_an_earlier_check_between_launches() {
        let prefs = UpdatePrefs {
            latest_version: Some("1.3.0".into()),
            latest_notes_url: Some("notes".into()),
            ..Default::default()
        };
        let n = notice(&UpdateStatus::Idle, &prefs, "1.2.1").unwrap();
        assert_eq!(n.version, Version::new(1, 3, 0));
        assert_eq!(n.notes_url.as_deref(), Some("notes"));
        // After updating, the same record announces nothing.
        assert_eq!(notice(&UpdateStatus::Idle, &prefs, "1.3.0"), None);
        assert_eq!(notice(&available("1.3.0"), &prefs, "1.3.0"), None);
    }
}
