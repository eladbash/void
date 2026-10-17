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
}
