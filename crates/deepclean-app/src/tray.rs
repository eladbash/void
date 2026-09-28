use tauri::{
    menu::{MenuBuilder, MenuItem, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, Manager,
};

use deepclean_core::model::ArtifactKind;

use crate::state::AppState;

/// Handles kept so the tray can be updated after it is built: the status line
/// changes after every scan and guard check, and the whole icon is hidden when
/// the user turns off "Show menu bar icon".
pub struct TrayHandles {
    tray: TrayIcon,
    status: MenuItem<tauri::Wry>,
}

pub fn setup_tray(app: &App) -> Result<(), Box<dyn std::error::Error>> {
    // A disabled item is the only way to put plain text in a native menu; it
    // is what makes "how much is free" answerable without opening the window.
    let status = MenuItemBuilder::with_id("status", "Checking disk…")
        .enabled(false)
        .build(app)?;
    let scan_now = MenuItemBuilder::with_id("scan_now", "Scan Now").build(app)?;
    let open_dashboard = MenuItemBuilder::with_id("open_dashboard", "Open Dashboard").build(app)?;
    let trim_worktrees =
        MenuItemBuilder::with_id("trim_worktrees", "Trim idle worktrees…").build(app)?;
    let top_separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let separator = tauri::menu::PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[
            &status,
            &top_separator,
            &scan_now,
            &open_dashboard,
            &trim_worktrees,
            &separator,
            &quit,
        ])
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

    let tray = TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(true)
        .menu(&menu)
        .tooltip("Void")
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "scan_now" => {
                if let Some(window) = show_main(app) {
                    let _ = window.eval("window.__void_scan && window.__void_scan()");
                }
            }
            "open_dashboard" => {
                show_main(app);
            }
            "trim_worktrees" => {
                // The frontend owns filtering; the tray only says what the
                // user asked to see.
                if show_main(app).is_some() {
                    let _ = app.emit("tray-action", "idle-worktrees");
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
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    let visible = app
        .try_state::<AppState>()
        .map(|s| s.config().ui.show_menu_bar_icon)
        .unwrap_or(true);
    let _ = tray.set_visible(visible);

    app.manage(TrayHandles { tray, status });
    Ok(())
}

fn show_main(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    let window = app.get_webview_window("main")?;
    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    Some(window)
}

/// Show or hide the menu bar icon.
pub fn set_visible(app: &AppHandle, visible: bool) {
    if let Some(handles) = app.try_state::<TrayHandles>() {
        let _ = handles.tray.set_visible(visible);
    }
}

/// Recompute the status line and tooltip from current state.
///
/// Cheap and idempotent, so it is called after anything that could change
/// the numbers: a scan, a clean, a guard tick, a settings change.
pub fn refresh(app: &AppHandle) {
    let (Some(handles), Some(state)) =
        (app.try_state::<TrayHandles>(), app.try_state::<AppState>())
    else {
        return;
    };

    let free = state.guard_view().status.as_ref().map(|s| s.free_bytes);
    let idle_days = state.config().ai.worktree_idle_days;
    let idle = {
        let results = state.results();
        let scanned = state.summary().total_items > 0 || !results.is_empty();
        scanned.then(|| {
            results
                .iter()
                .filter(|i| i.kind == ArtifactKind::AgentWorktree)
                .filter(|i| i.days_stale.is_some_and(|d| d >= idle_days))
                .count()
        })
    };

    let line = status_line(free, idle);
    let _ = handles.status.set_text(&line);
    let tooltip = match free {
        Some(bytes) => format!("Void — {} free", format_bytes(bytes)),
        None => "Void".to_string(),
    };
    let _ = handles.tray.set_tooltip(Some(tooltip));
}

/// "12.4 GB free · 4 agent worktrees idle".
///
/// The worktree half is left out until a scan has run: "0 idle" before
/// anyone looked would be a claim, not a measurement.
pub fn status_line(free_bytes: Option<u64>, idle_worktrees: Option<usize>) -> String {
    let free = match free_bytes {
        Some(bytes) => format!("{} free", format_bytes(bytes)),
        None => "Checking disk…".to_string(),
    };
    match idle_worktrees {
        Some(0) => format!("{free} · no idle worktrees"),
        Some(1) => format!("{free} · 1 agent worktree idle"),
        Some(n) => format!("{free} · {n} agent worktrees idle"),
        None => free,
    }
}

/// Base-1000 sizes with one decimal, matching the frontend's formatter and
/// Finder's Get Info — the numbers the user will compare against.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value < 100.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_frontend() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_500), "1.5 kB");
        assert_eq!(format_bytes(12_400_000_000), "12.4 GB");
        assert_eq!(format_bytes(999_970), "1.0 MB");
        assert_eq!(format_bytes(512_000_000_000), "512 GB");
    }

    #[test]
    fn status_line_reads_naturally() {
        assert_eq!(
            status_line(Some(12_400_000_000), Some(4)),
            "12.4 GB free · 4 agent worktrees idle"
        );
        assert_eq!(
            status_line(Some(12_400_000_000), Some(1)),
            "12.4 GB free · 1 agent worktree idle"
        );
        assert_eq!(
            status_line(Some(1_000_000_000), Some(0)),
            "1.0 GB free · no idle worktrees"
        );
    }

    #[test]
    fn status_line_makes_no_claims_before_measuring() {
        assert_eq!(status_line(None, None), "Checking disk…");
        assert_eq!(status_line(Some(2_000_000_000), None), "2.0 GB free");
    }
}
