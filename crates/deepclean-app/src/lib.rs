mod commands;
mod state;
mod tray;

use tauri::Manager;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // GUI launchers hand us a bare PATH; recover the user's real one before any
    // scanner or clean action tries to shell out to cargo/go/npm/brew/docker.
    deepclean_core::path_env::restore_login_shell_path();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
        ])
        .setup(|app| {
            // Settings live beside the bundle identifier, so the path resolver
            // owns the platform conventions rather than this crate:
            //   macOS   ~/Library/Application Support/com.void.app/
            //   Linux   ~/.config/com.void.app/
            //   Windows %APPDATA%\com.void.app\
            let config_dir = app.path().app_config_dir().unwrap_or_else(|_| {
                dirs::config_dir()
                    .unwrap_or_else(std::env::temp_dir)
                    .join("com.void.app")
            });
            app.manage(AppState::load_from(config_dir));

            tray::setup_tray(app)?;
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
