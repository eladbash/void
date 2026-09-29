mod commands;
mod guard_mode;
mod integrations;
pub mod model;
mod state;
mod tray;

use tauri::Manager;

use state::AppState;

/// `VOID_HOME=<dir>`: run the whole app against a fake home, for
/// development and demos (`void dev seed <dir>` builds one). Scans, settings,
/// history and the Trash all stay inside `<dir>`; see
/// `deepclean_core::sandbox`.
///
/// Must run before anything resolves home, so it is set as the very first
/// step of setup.
fn sandbox_home() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("VOID_HOME").filter(|v| !v.is_empty())?;
    let dir = std::path::PathBuf::from(dir);
    let dir = match dir.canonicalize() {
        Ok(dir) if dir.is_dir() => dir,
        _ => {
            eprintln!(
                "VOID_HOME={} is not a directory; run `void dev seed {}` first",
                dir.display(),
                dir.display()
            );
            std::process::exit(2);
        }
    };
    // Refuse the real home (or anything containing it): the point of a
    // sandbox is that it is not the real one.
    if let Some(real) = dirs::home_dir().and_then(|h| h.canonicalize().ok()) {
        if real.starts_with(&dir) {
            eprintln!("VOID_HOME must be a sandbox, not your real home or a folder containing it");
            std::process::exit(2);
        }
    }
    deepclean_core::paths::set_home_override(&dir);
    eprintln!("Void sandbox: {}", dir.display());
    Some(dir)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // GUI launchers hand us a bare PATH; recover the user's real one before any
    // scanner or clean action tries to shell out to cargo/go/npm/brew/docker.
    deepclean_core::path_env::restore_login_shell_path();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            commands::start_scan,
            commands::get_scan_results,
            commands::execute_clean,
            commands::cancel_clean,
            commands::get_summary,
            commands::get_config,
            commands::update_config,
            commands::get_config_path,
            commands::reveal_item,
            commands::get_disk_usage,
            commands::get_history,
            commands::clear_history,
            commands::take_startup_warnings,
            guard_mode::get_guard_status,
            guard_mode::run_guard_now,
            integrations::get_agent_usage,
            integrations::get_hook_status,
            integrations::install_hooks,
            integrations::uninstall_hooks,
            integrations::get_mcp_snippet,
            integrations::set_launch_at_login,
        ])
        .setup(|app| {
            // Settings live beside the bundle identifier, so the path resolver
            // owns the platform conventions rather than this crate:
            //   macOS   ~/Library/Application Support/com.void.app/
            //   Linux   ~/.config/com.void.app/
            //   Windows %APPDATA%\com.void.app\
            let sandbox = sandbox_home();
            let config_dir = match &sandbox {
                Some(home) => home.join(deepclean_core::sandbox::SANDBOX_CONFIG),
                None => app.path().app_config_dir().unwrap_or_else(|_| {
                    dirs::config_dir()
                        .unwrap_or_else(std::env::temp_dir)
                        .join("com.void.app")
                }),
            };
            app.manage(AppState::load_from(config_dir));
            if let (Some(home), Some(window)) = (&sandbox, app.get_webview_window("main")) {
                let _ = window.set_title(&format!("Void — SANDBOX {}", home.display()));
            }

            tray::setup_tray(app)?;

            // The login item lives in the OS, not in our config; bring it in
            // line with the saved preference in case either was changed
            // behind our back.
            // A sandbox run must not change the real login items.
            let launch_at_login = app.state::<AppState>().config().ui.launch_at_login;
            if sandbox.is_none() {
                if let Err(err) = integrations::apply_launch_at_login(app.handle(), launch_at_login)
                {
                    eprintln!("{err}");
                }
            }

            guard_mode::spawn(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!());

    // The entry point is the one place with no caller to hand an error to, so
    // this reports and exits rather than unwinding with a bare panic message.
    if let Err(err) = result {
        eprintln!("Void could not start: {err}");
        std::process::exit(1);
    }
}
