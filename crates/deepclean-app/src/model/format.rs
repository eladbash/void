//! Number, size, path and time formatting.
//!
//! One formatter, used everywhere. The old UI had two that disagreed — Rust's
//! `ByteSize` filled `size_display` for rows while JS used base 1000 for
//! totals, so a row could read 1.5 GB while contributing 1.6 GB to the sum.
//! Everything here derives from `size_bytes`; `size_display` is ignored.

use chrono::{DateTime, Local, Utc};

const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];

/// Narrow no-break space: keeps number and unit on one line.
pub const NNBSP: char = '\u{202f}';

/// Split a byte count into number and unit.
///
/// Base 1000 with `kB`/`MB`/`GB` to match Finder's Get Info, which is the
/// figure users compare against. `None` renders as an em dash.
pub fn split_bytes(bytes: Option<u64>) -> (String, &'static str) {
    let Some(bytes) = bytes else {
        return ("—".into(), "");
    };
    if bytes == 0 {
        return ("0".into(), "B");
    }
    let mut i = (((bytes as f64).log10() / 3.0).floor() as usize).min(UNITS.len() - 1);
    let mut v = bytes as f64 / 1000f64.powi(i as i32);
    // 999.97 kB should read as 1.0 MB, not 1000.0 kB.
    if v >= 999.95 && i < UNITS.len() - 1 {
        i += 1;
        v = bytes as f64 / 1000f64.powi(i as i32);
    }
    // Keep the trailing .0 below 100: a consistent decimal position down a
    // column matters more than brevity.
    let n = if i == 0 {
        format!("{v:.0}")
    } else if v < 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    };
    (n, UNITS[i])
}

/// Single-string form, for tooltips, toasts and the clipboard.
pub fn format_bytes(bytes: u64) -> String {
    let (n, u) = split_bytes(Some(bytes));
    format!("{n}{NNBSP}{u}")
}

/// Magnitude tier driving the size cell's typographic weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Magnitude {
    Unknown,
    Sm,
    Md,
    Lg,
    Xl,
}

pub fn magnitude(bytes: u64) -> Magnitude {
    match bytes {
        0 => Magnitude::Unknown,
        b if b >= 10_000_000_000 => Magnitude::Xl,
        b if b >= 1_000_000_000 => Magnitude::Lg,
        b if b >= 100_000_000 => Magnitude::Md,
        _ => Magnitude::Sm,
    }
}

/// Compact staleness for the narrow column: a sortable magnitude, not prose.
/// `None` renders as an em dash — an empty cell reads as a rendering bug.
pub fn stale_short(days: Option<u64>) -> String {
    match days {
        None => "—".into(),
        Some(0) => "today".into(),
        Some(d) if d < 365 => format!("{d}d"),
        Some(d) => format!("{:.1}y", d as f64 / 365.0),
    }
}

/// Verbose form for the drawer, where the user is deciding whether to delete.
pub fn stale_long(days: Option<u64>, last_modified: Option<DateTime<Utc>>) -> String {
    let Some(days) = days else {
        return "Modification time unavailable".into();
    };
    let relative = if days < 30 {
        match days {
            0 => "today".to_string(),
            1 => "yesterday".to_string(),
            d => format!("{d} days ago"),
        }
    } else if days < 365 {
        match (days as f64 / 30.0).round() as u64 {
            1 => "last month".to_string(),
            m => format!("{m} months ago"),
        }
    } else {
        let years = ((days as f64 / 365.0) * 10.0).round() / 10.0;
        if years == 1.0 {
            "last year".to_string()
        } else if years.fract() == 0.0 {
            format!("{years:.0} years ago")
        } else {
            format!("{years:.1} years ago")
        }
    };
    match last_modified {
        Some(at) => format!("{relative} · {}", medium_date(at)),
        None => relative,
    }
}

/// Exact counts always, with thousands separators. Abbreviating in a tool
/// that audits numbers is a lie.
pub fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "n item" / "n items".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

pub fn percent(part: u64, total: u64) -> String {
    if total == 0 {
        return "0%".into();
    }
    let p = part as f64 / total as f64 * 100.0;
    // A 40 MB ecosystem in a 30 GB scan rounds to 0%, which reads as "empty".
    if p > 0.0 && p < 1.0 {
        return "<1%".into();
    }
    if p > 99.0 && p < 100.0 {
        return ">99%".into();
    }
    format!("{}%", p.round() as u64)
}

pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    if ms < 60_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let m = ms / 60_000;
    let s = ((ms % 60_000) as f64 / 1000.0).round() as u64;
    format!("{m}m {s:02}s")
}

pub fn relative_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds();
    if secs < 60 {
        return "just now".into();
    }
    if secs < 3600 {
        return format!("{}m ago", secs / 60);
    }
    if secs < 86_400 {
        return format!("{}h ago", secs / 3600);
    }
    medium_date_time(at)
}

/// "Sep 29, 2026".
pub fn medium_date(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%b %-d, %Y").to_string()
}

/// "Sep 29, 2026, 8:25 AM".
pub fn medium_date_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%b %-d, %Y, %-I:%M %p")
        .to_string()
}

/// Replace the home prefix with `~`. Without a known home, fall back to the
/// conventional `/Users/<name>` or `/home/<name>` prefix.
pub fn tildify(path: &str, home: &str) -> String {
    let n = path.replace('\\', "/");
    let home = home.trim_end_matches('/');
    if !home.is_empty() && n.starts_with(home) {
        return format!("~{}", &n[home.len()..]);
    }
    for prefix in ["/Users/", "/home/"] {
        if let Some(rest) = n.strip_prefix(prefix) {
            return match rest.find('/') {
                Some(i) => format!("~{}", &rest[i..]),
                None => "~".into(),
            };
        }
    }
    n
}

/// Emphasis role of one path segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegRole {
    Root,
    Dim,
    Project,
    Leaf,
    Sep,
}

/// A path split into segments with four emphasis roles, so the meaningful
/// segments read as names and the separators recede.
///
/// The path is the item's identity — three `node_modules` rows are otherwise
/// indistinguishable — so it lives permanently on the row.
pub fn path_segments(full: &str, project: Option<&str>, home: &str) -> Vec<(SegRole, String)> {
    let p = tildify(full, home);
    let absolute = p.starts_with('/');
    let segments: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return vec![(SegRole::Dim, p)];
    }
    let leaf = segments.len() - 1;
    let project_idx = project
        .and_then(|name| segments.iter().rposition(|s| *s == name))
        .filter(|i| *i != leaf);

    let mut out = Vec::with_capacity(segments.len() * 2);
    if absolute && segments[0] != "~" {
        out.push((SegRole::Sep, "/".into()));
    }
    for (i, seg) in segments.iter().enumerate() {
        let role = if i == leaf {
            SegRole::Leaf
        } else if Some(i) == project_idx {
            SegRole::Project
        } else if i == 0 {
            SegRole::Root
        } else {
            SegRole::Dim
        };
        out.push((role, (*seg).to_string()));
        if i != leaf {
            out.push((SegRole::Sep, "/".into()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sb(b: Option<u64>) -> (String, &'static str) {
        split_bytes(b)
    }

    #[test]
    fn bytes_use_base_1000_with_a_consistent_decimal() {
        assert_eq!(sb(Some(0)), ("0".into(), "B"));
        assert_eq!(sb(Some(999)), ("999".into(), "B"));
        assert_eq!(sb(Some(1500)), ("1.5".into(), "kB"));
        assert_eq!(sb(Some(999_970)), ("1.0".into(), "MB"));
        assert_eq!(sb(Some(123_456_789_000)), ("123".into(), "GB"));
        assert_eq!(sb(None), ("—".into(), ""));
        // A narrow no-break space keeps number and unit together.
        assert_eq!(format_bytes(12_400_000_000), "12.4\u{202f}GB");
    }

    #[test]
    fn magnitude_tiers() {
        assert_eq!(magnitude(0), Magnitude::Unknown);
        assert_eq!(magnitude(50_000_000), Magnitude::Sm);
        assert_eq!(magnitude(200_000_000), Magnitude::Md);
        assert_eq!(magnitude(2_000_000_000), Magnitude::Lg);
        assert_eq!(magnitude(20_000_000_000), Magnitude::Xl);
    }

    #[test]
    fn staleness_percent_and_duration() {
        assert_eq!(stale_short(None), "—");
        assert_eq!(stale_short(Some(0)), "today");
        assert_eq!(stale_short(Some(12)), "12d");
        assert_eq!(stale_short(Some(730)), "2.0y");
        assert_eq!(percent(0, 0), "0%");
        assert_eq!(percent(1, 1000), "<1%");
        assert_eq!(percent(999, 1000), ">99%");
        assert_eq!(percent(50, 100), "50%");
        assert_eq!(duration(500), "500ms");
        assert_eq!(duration(1500), "1.5s");
        assert_eq!(duration(125_000), "2m 05s");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(12), "12");
        assert_eq!(plural(1, "item", "items"), "1 item");
        assert_eq!(plural(1200, "item", "items"), "1,200 items");
    }

    #[test]
    fn stale_long_reads_like_intl() {
        assert_eq!(stale_long(None, None), "Modification time unavailable");
        assert_eq!(stale_long(Some(0), None), "today");
        assert_eq!(stale_long(Some(1), None), "yesterday");
        assert_eq!(stale_long(Some(12), None), "12 days ago");
        assert_eq!(stale_long(Some(35), None), "last month");
        assert_eq!(stale_long(Some(90), None), "3 months ago");
        assert_eq!(stale_long(Some(365), None), "last year");
        assert_eq!(stale_long(Some(548), None), "1.5 years ago");
    }

    #[test]
    fn relative_time_buckets() {
        let now = Utc::now();
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(
            relative_time(now - chrono::Duration::minutes(5), now),
            "5m ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::hours(3), now),
            "3h ago"
        );
    }

    #[test]
    fn tildify_and_path_segments() {
        assert_eq!(tildify("/Users/dev/git/void", "/Users/dev/"), "~/git/void");
        assert_eq!(tildify("/home/someone/x", ""), "~/x");
        let segs = path_segments(
            "/Users/dev/git/<evil>/node_modules",
            Some("git"),
            "/Users/dev",
        );
        assert_eq!(
            segs.last().unwrap(),
            &(SegRole::Leaf, "node_modules".to_string())
        );
        assert!(segs.contains(&(SegRole::Project, "git".to_string())));
        assert!(segs.contains(&(SegRole::Dim, "<evil>".to_string())));
        assert_eq!(segs[0], (SegRole::Root, "~".to_string()));
        let abs = path_segments("/opt/x", None, "/Users/dev");
        assert_eq!(abs[0], (SegRole::Sep, "/".to_string()));
    }
}
