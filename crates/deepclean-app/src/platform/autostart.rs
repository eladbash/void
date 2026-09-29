//! Launch at login.
//!
//! macOS uses a LaunchAgent named after the app — the same one the Tauri
//! build registered, so an upgrade keeps the user's choice. Windows uses the
//! Run registry key, Linux an XDG autostart entry.

use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode};

fn launcher() -> Result<AutoLaunch, String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("Could not change launch at login: {err}"))?;
    AutoLaunchBuilder::new()
        .set_app_name("Void")
        .set_app_path(&exe.to_string_lossy())
        .set_macos_launch_mode(MacOSLaunchMode::LaunchAgent)
        .build()
        .map_err(|err| format!("Could not change launch at login: {err}"))
}

/// Make the OS login item match `enabled`. Only touches it when it differs,
/// so a launch with the setting off never writes a LaunchAgent.
pub fn apply_launch_at_login(enabled: bool) -> Result<(), String> {
    let manager = launcher()?;
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
