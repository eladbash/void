mod backend;
pub mod model;
mod platform;
mod state;
pub mod ui;

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    px, size, App, AppContext, Bounds, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions,
};

pub use backend::{Backend, UiEvent};
pub use state::AppState;
pub use ui::app_view::{native_folder_picker, AppView, FolderPicker};

/// `VOID_HOME=<dir>`: run the whole app against a fake home, for
/// development and demos (`void dev seed <dir>` builds one). Scans, settings,
/// history and the Trash all stay inside `<dir>`; see
/// `deepclean_core::sandbox`.
///
/// Must run before anything resolves home, so it is the first step.
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

/// Where settings and history live. Same folder the Tauri build used for the
/// `com.void.app` identifier, so an upgrade keeps both:
///   macOS   ~/Library/Application Support/com.void.app/
///   Linux   ~/.config/com.void.app/
///   Windows %APPDATA%\com.void.app\
pub fn config_dir(sandbox: Option<&std::path::Path>) -> std::path::PathBuf {
    match sandbox {
        Some(home) => home.join(deepclean_core::sandbox::SANDBOX_CONFIG),
        None => dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("com.void.app"),
    }
}

pub fn run() {
    // GUI launchers hand us a bare PATH; recover the user's real one before
    // any scanner or clean action tries to shell out to cargo/go/npm/brew.
    deepclean_core::path_env::restore_login_shell_path();

    let sandbox = sandbox_home();
    let state = Arc::new(AppState::load_from(config_dir(sandbox.as_deref())));
    let (backend, events) = Backend::new(state.clone());

    // The login item lives in the OS, not in our config; bring it in line
    // with the saved preference in case either changed behind our back. A
    // sandbox run must not touch the real login items.
    if sandbox.is_none() {
        let wanted = state.config().ui.launch_at_login;
        if let Err(err) = platform::autostart::apply_launch_at_login(wanted) {
            eprintln!("{err}");
        }
    }

    gpui_kit::application()
        .with_assets(ui::icons::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            ui::bind_keys(cx);
            cx.on_action(|_: &ui::Quit, cx| cx.quit());
            cx.set_menus([Menu {
                name: "Void".into(),
                items: vec![
                    MenuItem::action("Settings…", ui::GoSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Void", ui::Quit),
                ],
                disabled: false,
            }]);

            // The tray needs the platform event loop running, which it is by
            // the time this callback runs.
            let tray = match platform::tray::Tray::new(state.config().ui.show_menu_bar_icon) {
                Ok((tray, commands)) => Some((Rc::new(tray), commands)),
                Err(err) => {
                    eprintln!("tray: {err}");
                    None
                }
            };

            let title = match &sandbox {
                Some(home) => format!("Void — SANDBOX {}", home.display()),
                None => "Void".to_string(),
            };
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(900.), px(700.)),
                    cx,
                ))),
                window_min_size: Some(size(px(720.), px(560.))),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                app_id: Some("com.void.app".into()),
                ..Default::default()
            };
            backend.spawn_guard_loop();
            let backend = backend.clone();
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                cx.new(|cx| AppView::new(backend, events, tray, native_folder_picker(), window, cx))
            });
            if let Err(err) = opened {
                // The entry point has no caller to hand an error to.
                eprintln!("Void could not open its window: {err}");
                std::process::exit(1);
            }
            // Closing the window quits, as the Tauri build did.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
