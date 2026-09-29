//! Launch at login.
//!
//! macOS uses a LaunchAgent named after the app — the same one the Tauri
//! build registered, so an upgrade keeps the user's choice. Windows uses the
//! Run registry key, Linux an XDG autostart entry.

use auto_launch::{AutoLaunch, AutoLaunchBuilder, MacOSLaunchMode};

/// What the login item should start.
///
/// - An AppImage runs from a temporary mount that is gone after exit, so the
///   login item must name the `.AppImage` file itself (`$APPIMAGE`).
/// - On macOS the LaunchAgent opens the `.app` bundle, not the binary in it.
/// - Otherwise the running executable.
fn launch_target(
    exe: &std::path::Path,
    appimage: Option<std::ffi::OsString>,
) -> std::path::PathBuf {
    if let Some(appimage) = appimage.filter(|v| !v.is_empty()) {
        return appimage.into();
    }
    if cfg!(target_os = "macos") {
        let s = exe.to_string_lossy();
        if let Some(i) = s.find(".app/") {
            return std::path::PathBuf::from(&s[..i + 4]);
        }
    }
    exe.to_path_buf()
}

fn launcher() -> Result<AutoLaunch, String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("Could not change launch at login: {err}"))?;
    let target = launch_target(&exe, std::env::var_os("APPIMAGE"));
    AutoLaunchBuilder::new()
        .set_app_name("Void")
        .set_app_path(&target.to_string_lossy())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn an_appimage_registers_the_image_not_its_temporary_mount() {
        let exe = Path::new("/tmp/.mount_VoidAbc/usr/bin/void-app");
        let t = launch_target(exe, Some("/home/u/Apps/Void.AppImage".into()));
        assert_eq!(t, PathBuf::from("/home/u/Apps/Void.AppImage"));
        assert_eq!(launch_target(exe, Some("".into())), exe);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_the_bundle_is_registered() {
        let exe = Path::new("/Applications/Void.app/Contents/MacOS/void-app");
        assert_eq!(
            launch_target(exe, None),
            PathBuf::from("/Applications/Void.app")
        );
        let bare = Path::new("/Users/u/void/target/debug/void-app");
        assert_eq!(launch_target(bare, None), bare);
    }
}
