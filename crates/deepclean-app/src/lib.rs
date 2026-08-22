mod commands;
mod state;
mod tray;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // GUI launchers hand us a bare PATH; recover the user's real one before any
    // scanner or clean action tries to shell out to cargo/go/npm/brew/docker.
    deepclean_core::path_env::restore_login_shell_path();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::start_scan,
            commands::get_scan_results,
            commands::execute_clean,
            commands::get_summary,
            commands::get_config,
            commands::update_config,
        ])
        .setup(|app| {
            tray::setup_tray(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Void");
}
