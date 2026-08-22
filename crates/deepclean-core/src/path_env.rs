//! Restores the user's real `PATH` for GUI-launched processes.
//!
//! macOS starts bundled apps through `launchd`, which hands them a bare
//! `PATH=/usr/bin:/bin:/usr/sbin:/sbin`. Toolchains installed by rustup,
//! Homebrew, nvm and friends live outside that set, so every program we shell
//! out to (`cargo`, `go`, `npm`, `brew`, `docker`, ...) fails to spawn with
//! `NotFound`, which reaches the user as the bare and unhelpful
//! "IO error: No such file or directory (os error 2)".
//!
//! Linux desktop launchers have the same problem. We fix it once at startup by
//! asking the user's login shell what `PATH` it would have given us.

use std::collections::HashSet;
use std::path::PathBuf;

const SENTINEL_START: &str = "__VOID_PATH_START__";
const SENTINEL_END: &str = "__VOID_PATH_END__";

/// Merge the login shell's `PATH` (plus well-known tool directories) into this
/// process's `PATH`.
///
/// Call once, early, before any scanning or cleaning happens. A failed probe is
/// not an error: we fall back to the well-known directories and carry on.
pub fn restore_login_shell_path() {
    let current = std::env::var("PATH").unwrap_or_default();

    let discovered = probe_login_shell_path().unwrap_or_default();
    if discovered.is_empty() {
        tracing::debug!("Login shell PATH probe returned nothing; using fallbacks only");
    }

    let merged = merge_paths(&current, &discovered, &existing_tool_dirs());
    if merged != current {
        tracing::info!(path = %merged, "Restored login shell PATH");
        std::env::set_var("PATH", &merged);
    }
}

/// Ask the user's login shell for its `PATH`.
///
/// The shell runs as interactive *and* login (`-ilc`) because users commonly
/// extend `PATH` from `~/.zshrc` / `~/.bashrc`, which a non-interactive shell
/// never reads. Interactive rc files also print banners, so the value is
/// wrapped in sentinels rather than parsed as "whatever came out".
#[cfg(unix)]
fn probe_login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let script = format!(r#"printf '{SENTINEL_START}%s{SENTINEL_END}' "$PATH""#);

    let output = std::process::Command::new(&shell)
        .args(["-ilc", &script])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;

    parse_probe_output(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(unix))]
fn probe_login_shell_path() -> Option<String> {
    // Windows processes inherit the user's PATH regardless of how they start.
    None
}

/// Pull the `PATH` value out of shell output that may also contain rc-file noise.
fn parse_probe_output(stdout: &str) -> Option<String> {
    let start = stdout.find(SENTINEL_START)? + SENTINEL_START.len();
    let rest = &stdout[start..];
    let end = rest.find(SENTINEL_END)?;
    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Build a `PATH` from the login shell's entries, then the current process's,
/// then any well-known tool directories, dropping duplicates and empties.
///
/// The login shell goes first so a user's chosen toolchain (a rustup shim, a
/// Homebrew python) wins over the system copy, exactly as it does in a terminal.
fn merge_paths(current: &str, discovered: &str, fallbacks: &[PathBuf]) -> String {
    let mut seen = HashSet::new();
    let mut entries: Vec<PathBuf> = Vec::new();

    let ordered = std::env::split_paths(discovered)
        .chain(std::env::split_paths(current))
        .chain(fallbacks.iter().cloned());

    for entry in ordered {
        if entry.as_os_str().is_empty() {
            continue;
        }
        if seen.insert(entry.clone()) {
            entries.push(entry);
        }
    }

    match std::env::join_paths(&entries) {
        Ok(joined) => joined.to_string_lossy().into_owned(),
        // An entry containing the separator can't be joined; keep what we had.
        Err(err) => {
            tracing::warn!("Could not join PATH entries: {err}");
            current.to_string()
        }
    }
}

/// Well-known per-user tool directories, filtered to the ones that exist.
///
/// A safety net for when the shell probe fails (no `$SHELL`, a shell that
/// refuses `-i`, a login script that hangs and gets killed).
fn existing_tool_dirs() -> Vec<PathBuf> {
    candidate_tool_dirs()
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect()
}

fn candidate_tool_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/local/go/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join("go/bin"));
        dirs.push(home.join(".dotnet/tools"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(parts: &[&str]) -> String {
        std::env::join_paths(parts.iter().map(PathBuf::from))
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn parse_probe_output_extracts_value_between_sentinels() {
        let stdout = format!("{SENTINEL_START}/usr/bin:/opt/homebrew/bin{SENTINEL_END}");
        assert_eq!(
            parse_probe_output(&stdout).unwrap(),
            "/usr/bin:/opt/homebrew/bin"
        );
    }

    #[test]
    fn parse_probe_output_ignores_rc_file_noise() {
        let stdout = format!(
            "Welcome to zsh!\n[oh-my-zsh] update available\n{SENTINEL_START}/home/me/.cargo/bin:/usr/bin{SENTINEL_END}\ntrailing banner\n"
        );
        assert_eq!(
            parse_probe_output(&stdout).unwrap(),
            "/home/me/.cargo/bin:/usr/bin"
        );
    }

    #[test]
    fn parse_probe_output_none_without_sentinels() {
        assert!(parse_probe_output("/usr/bin:/bin").is_none());
        assert!(parse_probe_output("").is_none());
    }

    #[test]
    fn parse_probe_output_none_when_path_is_empty() {
        let stdout = format!("{SENTINEL_START}{SENTINEL_END}");
        assert!(parse_probe_output(&stdout).is_none());
    }

    #[test]
    fn merge_puts_login_shell_entries_first() {
        // The launchd default, plus the PATH a terminal would have given us.
        let current = join(&["/usr/bin", "/bin", "/usr/sbin", "/sbin"]);
        let discovered = join(&["/Users/me/.cargo/bin", "/opt/homebrew/bin", "/usr/bin"]);

        let merged = merge_paths(&current, &discovered, &[]);
        let entries: Vec<PathBuf> = std::env::split_paths(&merged).collect();

        assert_eq!(
            entries,
            [
                "/Users/me/.cargo/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin"
            ]
            .map(PathBuf::from)
        );
    }

    #[test]
    fn merge_drops_duplicates_and_empty_entries() {
        let current = join(&["/usr/bin", "/bin"]);
        let discovered = "/usr/bin::/usr/bin:/opt/homebrew/bin";

        let merged = merge_paths(current.as_str(), discovered, &[]);
        let entries: Vec<PathBuf> = std::env::split_paths(&merged).collect();

        assert_eq!(
            entries,
            ["/usr/bin", "/opt/homebrew/bin", "/bin"].map(PathBuf::from)
        );
    }

    #[test]
    fn merge_keeps_current_when_probe_found_nothing() {
        let current = join(&["/usr/bin", "/bin"]);
        assert_eq!(merge_paths(&current, "", &[]), current);
    }

    #[test]
    fn merge_appends_fallback_dirs_that_are_not_already_present() {
        let current = join(&["/usr/bin"]);
        let fallbacks = vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/Users/me/.cargo/bin"),
        ];

        let merged = merge_paths(&current, "", &fallbacks);
        let entries: Vec<PathBuf> = std::env::split_paths(&merged).collect();

        assert_eq!(
            entries,
            ["/usr/bin", "/Users/me/.cargo/bin"].map(PathBuf::from)
        );
    }

    #[test]
    fn candidate_tool_dirs_cover_the_usual_toolchain_installs() {
        let dirs = candidate_tool_dirs();
        let home = dirs::home_dir().unwrap();

        assert!(dirs.contains(&home.join(".cargo/bin")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
    }
}
