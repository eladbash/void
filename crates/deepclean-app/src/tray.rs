use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, Manager,
};

pub fn setup_tray(app: &App) -> Result<(), Box<dyn std::error::Error>> {
    let scan_now = MenuItemBuilder::with_id("scan_now", "Scan Now").build(app)?;
    let open_dashboard = MenuItemBuilder::with_id("open_dashboard", "Open Dashboard").build(app)?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&scan_now, &open_dashboard, &separator, &quit])
        .build()?;

    // macOS menu-bar icons must be template images: pure black plus alpha, so
    // the system can tint them for a light or dark menu bar and invert them
    // when clicked. The full-colour app icon does neither.
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/trayTemplate@2x.png"))
        .unwrap_or_else(|_| {
            app.default_window_icon()
                .cloned()
                .unwrap_or_else(|| tauri::image::Image::new(&[], 0, 0))
        });

    TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(true)
        .menu(&menu)
        .tooltip("Void")
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "scan_now" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                    let _ = window.eval("window.__void_scan && window.__void_scan()");
                }
            }
            "open_dashboard" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
}
