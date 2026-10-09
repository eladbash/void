//! The menu-bar (macOS) / system-tray (Windows, Linux) icon.
//!
//! Built on the main thread once GPUI's event loop is running: macOS and
//! Windows need that loop on the thread that owns the icon. On Linux the
//! StatusNotifierItem backend runs its own D-Bus worker. Menu clicks are
//! forwarded as [`TrayCommand`]s on a channel the window drains.

use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the user picked in the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ScanNow,
    OpenDashboard,
    TrimIdleWorktrees,
    Quit,
}

/// Handles kept so the tray can be updated after it is built: the status
/// line changes after every scan and guard check, and the whole icon is
/// hidden when the user turns off "Show menu bar icon".
pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
}

impl Tray {
    pub fn new(visible: bool) -> Result<(Self, UnboundedReceiver<TrayCommand>), String> {
        // A disabled item is the only way to put plain text in a native
        // menu; it makes "how much is free" answerable without the window.
        let status = MenuItem::with_id("status", "Checking disk…", false, None);
        let scan_now = MenuItem::with_id("scan_now", "Scan Now", true, None);
        let open = MenuItem::with_id("open_dashboard", "Open Dashboard", true, None);
        let trim = MenuItem::with_id("trim_worktrees", "Trim idle worktrees…", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &scan_now,
            &open,
            &trim,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;

        let (tx, rx) = unbounded();
        let menu_tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let cmd = match event.id().as_ref() {
                "scan_now" => TrayCommand::ScanNow,
                "open_dashboard" => TrayCommand::OpenDashboard,
                "trim_worktrees" => TrayCommand::TrimIdleWorktrees,
                "quit" => TrayCommand::Quit,
                _ => return,
            };
            let _ = menu_tx.unbounded_send(cmd);
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = tx.unbounded_send(TrayCommand::OpenDashboard);
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_icon(template_icon()?)
            // macOS menu-bar icons must be template images: black plus alpha,
            // so the system tints them for a light or dark menu bar.
            .with_icon_as_template(true)
            .with_menu(Box::new(menu))
            .with_tooltip("Void")
            .build()
            .map_err(|e| e.to_string())?;
        let _ = icon.set_visible(visible);
        Ok((Self { icon, status }, rx))
    }

    pub fn set_visible(&self, visible: bool) {
        let _ = self.icon.set_visible(visible);
    }

    /// Update the status line and tooltip.
    pub fn refresh(&self, free_bytes: Option<u64>, idle_worktrees: Option<usize>) {
        self.status
            .set_text(status_line(free_bytes, idle_worktrees));
        let tooltip = match free_bytes {
            Some(bytes) => format!("Void — {} free", format_bytes(bytes)),
            None => "Void".to_string(),
        };
        let _ = self.icon.set_tooltip(Some(tooltip));
    }
}

/// Decode the bundled template PNG into RGBA for the tray.
fn template_icon() -> Result<Icon, String> {
    let bytes: &[u8] = include_bytes!("../../icons/trayTemplate@2x.png");
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed tray icon after expansion".into()),
    };
    Icon::from_rgba(rgba, info.width, info.height).map_err(|e| e.to_string())
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

/// Base-1000 sizes with one decimal and a plain space — for native menus and
/// notifications, which render the narrow no-break space inconsistently.
pub fn format_bytes(bytes: u64) -> String {
    let (n, u) = crate::model::format::split_bytes(Some(bytes));
    format!("{n} {u}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_window() {
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

    #[test]
    fn the_bundled_template_icon_decodes() {
        template_icon().unwrap();
    }
}
