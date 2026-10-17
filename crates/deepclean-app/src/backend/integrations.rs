//! Agent-facing integrations: usage by agent, Claude Code hooks and the MCP
//! snippet. Launch at login lives in `platform::autostart`.

use std::path::{Path, PathBuf};

use deepclean_core::attribution::{self, AgentUsage};
use deepclean_core::hooks::{self, HookStatus};

use super::Backend;

/// Shown whenever an integration needs the `void` CLI and it cannot be found.
pub const CLI_MISSING: &str = "The void command-line tool was not found. Use Install void \
command on the Agents screen, or download it from https://github.com/eladbash/void/releases \
(or `cargo install --path crates/void-cli` from a Void checkout), then try again.";

/// Hook status plus where the CLI lives, so the UI can explain a missing CLI
/// before the user clicks Install.
#[derive(Debug, Clone)]
pub struct HookView {
    pub status: HookStatus,
    pub cli_path: Option<PathBuf>,
}

/// What the user pastes to register Void as an MCP server.
#[derive(Debug, Clone)]
pub struct McpSnippet {
    /// JSON for an agent's MCP config file, pretty-printed.
    pub json: String,
    /// One-liner for Claude Code.
    pub command: String,
}

pub const MCP_COMMAND: &str = "claude mcp add void -- void mcp";

fn home() -> Result<PathBuf, String> {
    deepclean_core::paths::home_dir().ok_or_else(|| "Could not find your home folder".to_string())
}

fn cli() -> Result<PathBuf, String> {
    find_void_cli().ok_or_else(|| CLI_MISSING.to_string())
}

impl Backend {
    /// Space per AI tool across the current results.
    pub fn agent_usage(&self) -> Vec<AgentUsage> {
        attribution::by_agent(&self.state.results())
    }
}

pub fn hook_status() -> Result<HookView, String> {
    Ok(HookView {
        status: hooks::status(&home()?),
        cli_path: find_void_cli(),
    })
}

pub fn install_hooks() -> Result<HookView, String> {
    let bin = cli()?;
    let status = hooks::install(&home()?, &bin)?;
    Ok(HookView {
        status,
        cli_path: Some(bin),
    })
}

pub fn uninstall_hooks() -> Result<HookView, String> {
    Ok(HookView {
        status: hooks::uninstall(&home()?)?,
        cli_path: find_void_cli(),
    })
}

pub fn mcp_snippet() -> Result<McpSnippet, String> {
    let bin = cli()?;
    let raw = hooks::mcp_config_snippet(&bin);
    let json = serde_json::from_str::<serde_json::Value>(&raw)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or(raw);
    // `void` alone only works once it is on the PATH; until then, the full
    // path to the copy that ships with the app.
    let command = if find_executable("void", path_dirs()).is_some() {
        MCP_COMMAND.to_string()
    } else {
        format!(
            "claude mcp add void -- {} mcp",
            shell_quote(&bin.to_string_lossy())
        )
    };
    Ok(McpSnippet { json, command })
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
}

const CLI_FILE: &str = if cfg!(windows) { "void.exe" } else { "void" };

/// The `void` that ships next to the app's own executable: in
/// `Void.app/Contents/MacOS`, the Windows install folder, or `/usr/bin` (the
/// `.deb`) — and, during development, `target/debug`.
pub fn bundled_cli() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?.join(CLI_FILE);
    is_executable(&bin).then_some(bin)
}

/// The bundled CLI, unless its path changes between launches: an AppImage
/// runs from a fresh temporary mount each time, so hooks must not point
/// into it.
fn stable_bundled_cli() -> Option<PathBuf> {
    if std::env::var_os("APPIMAGE").is_some() {
        return None;
    }
    bundled_cli()
}

/// Find the `void` CLI: the copy that ships with the app first, then `PATH`,
/// then the places installers put it.
///
/// The GUI's `PATH` has already been widened to the login shell's by
/// `path_env::restore_login_shell_path`, so this sees what a terminal sees.
pub fn find_void_cli() -> Option<PathBuf> {
    if let Some(bin) = stable_bundled_cli() {
        return Some(bin);
    }
    let path_dirs = path_dirs();
    let fallbacks = deepclean_core::paths::home_dir()
        .map(|h| vec![h.join(".cargo/bin"), h.join(".local/bin")])
        .unwrap_or_default()
        .into_iter()
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    find_executable("void", path_dirs.into_iter().chain(fallbacks))
}

/// First `dir/name` (or `name.exe` on Windows) that is an executable file.
pub fn find_executable(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    dirs.into_iter()
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

// ── The `void` command on the PATH ──────────────────────────────────────

/// Whether `void` works in a terminal, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliView {
    /// The copy that ships with the app, if there is one.
    pub bundled: Option<PathBuf>,
    /// `void` as a terminal finds it.
    pub on_path: Option<PathBuf>,
}

pub fn cli_view() -> CliView {
    CliView {
        bundled: bundled_cli(),
        on_path: find_executable("void", path_dirs()),
    }
}

/// What "Install void command" did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInstalled {
    /// The `void` that terminals now run.
    pub path: PathBuf,
    /// Set when its folder isn't on the PATH yet: what to do about it.
    pub hint: Option<String>,
}

/// How the command reaches the bundled CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    /// A symlink, which follows the app through updates.
    Symlink,
    /// A copy, for an AppImage whose own path changes every launch.
    Copy,
}

/// Put `void` in the first usable folder of `dirs`, pointing at `bundled`.
/// Folders on the PATH win; one that doesn't exist yet is created. An
/// existing `void` there is replaced only if it is a symlink or (for a copy)
/// an older copy of Void's CLI; anyone else's `void` is left alone.
pub fn link_cli(
    bundled: &Path,
    dirs: &[PathBuf],
    path: &[PathBuf],
    mode: LinkMode,
) -> Result<CliInstalled, String> {
    let on_path = |d: &Path| path.iter().any(|p| p == d);
    let mut ordered: Vec<&PathBuf> = dirs.iter().filter(|d| on_path(d)).collect();
    ordered.extend(dirs.iter().filter(|d| !on_path(d)));
    let mut last_err = "no folder to put the void command in".to_string();
    for dir in ordered {
        if std::fs::create_dir_all(dir).is_err() {
            continue;
        }
        let dest = dir.join(CLI_FILE);
        if let Ok(meta) = std::fs::symlink_metadata(&dest) {
            if meta.file_type().is_symlink() || mode == LinkMode::Copy && same_file(&dest, bundled)
            {
                if let Err(e) = std::fs::remove_file(&dest) {
                    last_err = format!("couldn't replace {}: {e}", dest.display());
                    continue;
                }
            } else {
                last_err = format!(
                    "{} already exists and isn't Void's; remove it or use that one",
                    dest.display()
                );
                continue;
            }
        }
        let made = match mode {
            LinkMode::Symlink => symlink(bundled, &dest),
            LinkMode::Copy => copy_executable(bundled, &dest),
        };
        match made {
            Ok(()) => {
                let hint = (!on_path(dir)).then(|| {
                    format!(
                        "{} isn't on your PATH yet. Add it to your shell profile: export PATH=\"{}:$PATH\"",
                        dir.display(),
                        dir.display()
                    )
                });
                return Ok(CliInstalled { path: dest, hint });
            }
            Err(e) => last_err = format!("couldn't create {}: {e}", dest.display()),
        }
    }
    Err(last_err)
}

/// `existing` is a copy of Void's CLI (any version): the same bytes, or a
/// binary built from the same crates.
fn same_file(existing: &Path, bundled: &Path) -> bool {
    let Ok(old) = std::fs::read(existing) else {
        return false;
    };
    std::fs::read(bundled).is_ok_and(|new| new == old)
        || old
            .windows(b"deepclean_core".len())
            .any(|w| w == b"deepclean_core")
}

fn copy_executable(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::copy(from, to)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    copy_executable(target, link)
}

/// "Install void command": make `void` work in a new terminal.
pub fn install_cli_command() -> Result<CliInstalled, String> {
    let bundled = bundled_cli().ok_or(
        "This build of Void has no void command bundled with it; download it from the releases page",
    )?;
    let home = home()?;
    let path = path_dirs();
    // A sandbox only ever writes inside its fake home.
    if deepclean_core::paths::home_override().is_some() {
        return link_cli(
            &bundled,
            &[home.join(".local/bin")],
            &path,
            LinkMode::Symlink,
        );
    }
    if cfg!(windows) {
        return add_to_user_path(&bundled);
    }
    let appimage = std::env::var_os("APPIMAGE").is_some();
    let mode = if appimage {
        LinkMode::Copy
    } else {
        LinkMode::Symlink
    };
    let local = home.join(".local/bin");
    let usr_local = PathBuf::from("/usr/local/bin");
    let writable = |d: &Path| {
        d.is_dir()
            && std::fs::metadata(d).is_ok_and(|m| !m.permissions().readonly())
            && tempfile_probe(d)
    };
    let mut dirs = vec![];
    if writable(&usr_local) {
        dirs.push(usr_local.clone());
    }
    dirs.push(local.clone());
    let linked = link_cli(&bundled, &dirs, &path, mode)?;
    // macOS doesn't put ~/.local/bin on the PATH. Rather than leave the user
    // to edit their shell profile, offer /usr/local/bin with an admin
    // prompt, as other apps' "install command" actions do.
    if cfg!(target_os = "macos") && linked.hint.is_some() && !appimage {
        if let Ok(path) = link_with_admin(&bundled, &usr_local) {
            let _ = std::fs::remove_file(&linked.path);
            return Ok(CliInstalled { path, hint: None });
        }
    }
    Ok(linked)
}

/// Can we create a file in `dir`? (Permissions bits lie about ACLs.)
fn tempfile_probe(dir: &Path) -> bool {
    let probe = dir.join(format!(".void-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Symlink `bundled` into `dir` with the macOS admin password prompt.
fn link_with_admin(bundled: &Path, dir: &Path) -> Result<PathBuf, String> {
    let dest = dir.join("void");
    let q = |p: &Path| shell_quote(&p.to_string_lossy());
    let script = format!("mkdir -p {} && ln -sfn {} {}", q(dir), q(bundled), q(&dest));
    let apple = format!(
        "do shell script \"{}\" with prompt \"Void wants to add the void command to {}.\" with administrator privileges",
        script.replace('\\', "\\\\").replace('"', "\\\""),
        dir.display()
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &apple])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(dest)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Windows: add the install folder to the user's PATH.
fn add_to_user_path(bundled: &Path) -> Result<CliInstalled, String> {
    let dir = bundled
        .parent()
        .ok_or("the void command has no folder")?
        .to_path_buf();
    let script = format!(
        "$d = '{}'; $p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
         if (-not $p) {{ $p = '' }}; \
         if (($p -split ';') -notcontains $d) {{ \
           [Environment]::SetEnvironmentVariable('Path', (($p.TrimEnd(';') + ';' + $d).TrimStart(';')), 'User') }}",
        dir.display().to_string().replace('\'', "''")
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("couldn't run PowerShell: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(CliInstalled {
        path: bundled.to_path_buf(),
        hint: Some("Open a new terminal to use it.".into()),
    })
}

/// An AppImage's copied `void` goes stale when the AppImage updates; bring
/// it up to date with the one inside.
pub fn refresh_appimage_cli_copy() {
    if std::env::var_os("APPIMAGE").is_none() || deepclean_core::paths::home_override().is_some() {
        return;
    }
    let (Some(bundled), Ok(home)) = (bundled_cli(), home()) else {
        return;
    };
    let copy = home.join(".local/bin").join(CLI_FILE);
    if copy.is_file() && std::fs::read(&copy).ok() != std::fs::read(&bundled).ok() {
        let _ = copy_executable(&bundled, &copy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_matching_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let name = if cfg!(windows) { "void.exe" } else { "void" };
        let bin = b.join(name);
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        assert_eq!(find_executable("void", [a, b]), Some(bin));
    }

    #[test]
    fn ignores_directories_and_missing_entries() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("void")).unwrap();
        assert_eq!(
            find_executable("void", [tmp.path().to_path_buf(), tmp.path().join("nope")]),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignores_files_that_are_not_executable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("void");
        std::fs::write(&bin, "data").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find_executable("void", [tmp.path().to_path_buf()]), None);
    }

    #[test]
    fn missing_cli_message_says_how_to_install() {
        assert!(CLI_MISSING.contains("Install void command"));
        assert!(CLI_MISSING.contains("cargo install --path crates/void-cli"));
        assert!(CLI_MISSING.contains("releases"));
    }

    #[cfg(unix)]
    fn exe(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_into_a_folder_on_the_path_first() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("Void.app/Contents/MacOS/void");
        exe(&bundled, "#!/bin/sh\necho void\n");
        let (off, on) = (tmp.path().join("off"), tmp.path().join("on"));
        let got = link_cli(
            &bundled,
            &[off.clone(), on.clone()],
            std::slice::from_ref(&on),
            LinkMode::Symlink,
        )
        .unwrap();
        assert_eq!(got.path, on.join("void"));
        assert_eq!(got.hint, None);
        assert_eq!(std::fs::read_link(&got.path).unwrap(), bundled);
        assert!(!off.join("void").exists());
    }

    #[cfg(unix)]
    #[test]
    fn says_how_to_reach_a_folder_off_the_path_and_replaces_old_links() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("app/void");
        exe(&bundled, "new");
        let dir = tmp.path().join(".local/bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("old-app/void"), dir.join("void")).unwrap();
        let got = link_cli(&bundled, std::slice::from_ref(&dir), &[], LinkMode::Symlink).unwrap();
        assert_eq!(std::fs::read_link(&got.path).unwrap(), bundled);
        assert!(got.hint.unwrap().contains("export PATH="));
    }

    #[cfg(unix)]
    #[test]
    fn never_overwrites_someone_elses_void() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("app/void");
        exe(&bundled, "ours");
        let dir = tmp.path().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("void"), "a homebrew binary").unwrap();
        let err = link_cli(
            &bundled,
            std::slice::from_ref(&dir),
            std::slice::from_ref(&dir),
            LinkMode::Symlink,
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.join("void")).unwrap(),
            "a homebrew binary"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_appimage_gets_a_copy_it_can_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("mount/usr/bin/void");
        exe(&bundled, "#!/bin/sh\necho v2\n");
        let dir = tmp.path().join(".local/bin");
        // An older copy: built from the same crates.
        exe(&dir.join("void"), "#!/bin/sh\n# deepclean_core\necho v1\n");
        let got = link_cli(
            &bundled,
            std::slice::from_ref(&dir),
            std::slice::from_ref(&dir),
            LinkMode::Copy,
        )
        .unwrap();
        assert!(!std::fs::symlink_metadata(&got.path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&got.path).unwrap(),
            "#!/bin/sh\necho v2\n"
        );
    }
}
