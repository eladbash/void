//! Agent-facing integrations and OS hooks: usage by agent, Claude Code hooks,
//! the MCP snippet, and launch at login.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

use deepclean_core::attribution::{self, AgentUsage};
use deepclean_core::hooks::{self, HookStatus};

use crate::state::AppState;

/// Shown whenever an integration needs the `void` CLI and it cannot be found.
pub const CLI_MISSING: &str = "The void command-line tool was not found on your PATH. \
Install it with `cargo install --path crates/void-cli` from a Void checkout, or download \
it from https://github.com/eladbash/void/releases, then try again.";

/// Space per AI tool across the current results.
#[tauri::command]
pub async fn get_agent_usage(state: State<'_, AppState>) -> Result<Vec<AgentUsage>, String> {
    Ok(attribution::by_agent(&state.results()))
}

/// Hook status plus where the CLI lives, so the UI can explain a missing CLI
/// before the user clicks Install.
#[derive(Debug, Clone, Serialize)]
pub struct HookView {
    #[serde(flatten)]
    pub status: HookStatus,
    pub cli_path: Option<PathBuf>,
}

fn home() -> Result<PathBuf, String> {
    deepclean_core::paths::home_dir().ok_or_else(|| "Could not find your home folder".to_string())
}

fn cli() -> Result<PathBuf, String> {
    find_void_cli().ok_or_else(|| CLI_MISSING.to_string())
}

#[tauri::command]
pub async fn get_hook_status() -> Result<HookView, String> {
    Ok(HookView {
        status: hooks::status(&home()?),
        cli_path: find_void_cli(),
    })
}

#[tauri::command]
pub async fn install_hooks() -> Result<HookView, String> {
    let bin = cli()?;
    let status = hooks::install(&home()?, &bin)?;
    Ok(HookView {
        status,
        cli_path: Some(bin),
    })
}

#[tauri::command]
pub async fn uninstall_hooks() -> Result<HookView, String> {
    Ok(HookView {
        status: hooks::uninstall(&home()?)?,
        cli_path: find_void_cli(),
    })
}

/// What the user pastes to register Void as an MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct McpSnippet {
    /// JSON for an agent's MCP config file.
    pub json: String,
    /// One-liner for Claude Code.
    pub command: String,
    pub cli_path: PathBuf,
}

#[tauri::command]
pub async fn get_mcp_snippet() -> Result<McpSnippet, String> {
    let bin = cli()?;
    Ok(McpSnippet {
        json: hooks::mcp_config_snippet(&bin),
        command: "claude mcp add void -- void mcp".into(),
        cli_path: bin,
    })
}

/// Register or remove the login item, then remember the choice.
#[tauri::command]
pub async fn set_launch_at_login(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<bool, String> {
    apply_launch_at_login(&app, enabled)?;
    state.config().ui.launch_at_login = enabled;
    state.persist_config()?;
    Ok(enabled)
}

/// Make the OS login item match `enabled`. Only touches it when it differs,
/// so a launch with the setting off never writes a LaunchAgent.
pub fn apply_launch_at_login(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    let current = manager.is_enabled().unwrap_or(!enabled);
    if current == enabled {
        return Ok(());
    }
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|err| format!("Could not change launch at login: {err}"))
}

/// Find the `void` CLI: `PATH` first, then the places installers put it.
///
/// The GUI's `PATH` has already been widened to the login shell's by
/// `path_env::restore_login_shell_path`, so this sees what a terminal sees.
pub fn find_void_cli() -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
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
        assert!(CLI_MISSING.contains("cargo install --path crates/void-cli"));
        assert!(CLI_MISSING.contains("releases"));
    }
}
