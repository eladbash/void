//! The UI driver behind `scripts/drive.py`: debug builds only, and only for a
//! sandboxed run.
//!
//! With the `drive` feature compiled in and both `VOID_HOME` (the sandbox)
//! and `VOID_DRIVE=<socket>` set, the app listens on that Unix socket for
//! JSON-lines commands and feeds them straight into GPUI: keystrokes through
//! `Window::dispatch_keystroke`, clicks as synthetic mouse events through
//! `Window::dispatch_event`. Nothing goes through the OS input system, so the
//! window never needs focus, the app never activates itself, and the user's
//! keyboard, mouse and frontmost app are left alone. The window is opened
//! behind every other window, and effects that would reach outside it —
//! clipboard, notifications, Finder, the folder picker — are captured here
//! instead.
//!
//! AppKit stops drawing a window that is fully covered, so every command ends
//! by forcing a frame; screenshots by window id (`screencapture -l`) are then
//! current even with the window at the back.
//!
//! Without the feature, everything here is a constant `false` / no-op.

#[cfg(all(feature = "drive", not(unix)))]
compile_error!("the `drive` feature needs Unix sockets (macOS or Linux)");

/// Whether this run is driven. Constant `false` without the feature.
pub fn active() -> bool {
    #[cfg(feature = "drive")]
    {
        imp::socket().is_some()
    }
    #[cfg(not(feature = "drive"))]
    {
        false
    }
}

/// Record something a driven run does instead of reaching outside its
/// window. Call it only when [`active`]; the caller then skips the real thing.
pub fn capture(effect: Effect) {
    #[cfg(feature = "drive")]
    imp::capture(effect);
    #[cfg(not(feature = "drive"))]
    let _ = effect;
}

#[cfg_attr(not(feature = "drive"), allow(dead_code))]
pub enum Effect {
    Clipboard(String),
    Notify { title: String, body: String },
    Reveal(std::path::PathBuf),
    FolderPicker,
    OpenUrl(String),
}

#[cfg(feature = "drive")]
pub use imp::start;

#[cfg(feature = "drive")]
mod imp {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use futures::channel::{mpsc, oneshot};
    use futures::StreamExt;
    use gpui_kit::{
        point, px, size, AnyWindowHandle, App, AppContext, AsyncApp, Entity, Keystroke, Modifiers,
        MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Window,
    };
    use serde_json::{json, Value};

    use super::Effect;
    use crate::ui::app_view::AppView;

    /// Effects the app handed to the driver instead of the OS, oldest first.
    #[derive(Default)]
    struct Outbox {
        clipboard: Option<String>,
        notifications: Vec<Value>,
        revealed: Vec<String>,
        folder_pickers: usize,
        opened_urls: Vec<String>,
    }

    fn outbox() -> &'static Mutex<Outbox> {
        static OUTBOX: OnceLock<Mutex<Outbox>> = OnceLock::new();
        OUTBOX.get_or_init(Default::default)
    }

    pub fn socket() -> Option<PathBuf> {
        static SOCKET: OnceLock<Option<PathBuf>> = OnceLock::new();
        SOCKET
            .get_or_init(|| {
                let path = std::env::var_os("VOID_DRIVE").filter(|v| !v.is_empty())?;
                // Driving is for sandboxed runs only: a driven app clicks
                // Clean on whatever is in front of it.
                if deepclean_core::paths::home_override().is_none() {
                    eprintln!("VOID_DRIVE needs VOID_HOME (a sandbox); not driving");
                    return None;
                }
                Some(PathBuf::from(path))
            })
            .clone()
    }

    pub fn capture(effect: Effect) {
        let mut out = outbox().lock().unwrap_or_else(|e| e.into_inner());
        match effect {
            Effect::Clipboard(text) => out.clipboard = Some(text),
            Effect::Notify { title, body } => out
                .notifications
                .push(json!({ "title": title, "body": body })),
            Effect::Reveal(path) => out.revealed.push(path.display().to_string()),
            Effect::FolderPicker => out.folder_pickers += 1,
            Effect::OpenUrl(url) => out.opened_urls.push(url),
        }
    }

    type Request = (Value, oneshot::Sender<Value>);

    /// How long a forced frame waits for pending updates, before and after.
    const SETTLE: Duration = Duration::from_millis(30);

    /// Listen on the driver socket. Connections are served on background
    /// threads; every command runs on the UI thread, one at a time.
    pub fn start(window: AnyWindowHandle, view: Entity<AppView>, cx: &mut App) {
        let Some(path) = socket() else { return };
        let _ = std::fs::remove_file(&path);
        let listener = match std::os::unix::net::UnixListener::bind(&path) {
            Ok(l) => l,
            Err(err) => {
                eprintln!("drive: cannot listen on {}: {err}", path.display());
                return;
            }
        };
        eprintln!("drive: listening on {}", path.display());

        let (tx, mut rx) = mpsc::unbounded::<Request>();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || serve(conn, tx));
            }
        });

        place_at_back(window, cx);
        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Some((req, reply)) = rx.next().await {
                let resp = match run(&req, window, &view, cx).await {
                    Ok(v) => json!({ "ok": true, "result": v }),
                    Err(e) => json!({ "ok": false, "error": e }),
                };
                let _ = reply.send(resp);
            }
        })
        .detach();
    }

    fn serve(conn: std::os::unix::net::UnixStream, tx: mpsc::UnboundedSender<Request>) {
        let Ok(mut writer) = conn.try_clone() else {
            return;
        };
        for line in BufReader::new(conn).lines() {
            let Ok(line) = line else { return };
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Value>(&line) {
                Ok(req) => {
                    let (reply, rx) = oneshot::channel();
                    if tx.unbounded_send((req, reply)).is_err() {
                        return;
                    }
                    futures::executor::block_on(rx)
                        .unwrap_or_else(|_| json!({ "ok": false, "error": "app is gone" }))
                }
                Err(e) => json!({ "ok": false, "error": format!("bad JSON: {e}") }),
            };
            if writeln!(writer, "{resp}").is_err() {
                return;
            }
        }
    }

    async fn run(
        req: &Value,
        window: AnyWindowHandle,
        view: &Entity<AppView>,
        cx: &mut AsyncApp,
    ) -> Result<Value, String> {
        let cmd = req["cmd"].as_str().ok_or("missing \"cmd\"")?;
        let num = |k: &str| {
            req[k]
                .as_f64()
                .map(|v| v as f32)
                .ok_or_else(|| format!("{cmd}: missing number \"{k}\""))
        };
        let result = match cmd {
            "key" => {
                let keys = req["keys"].as_str().ok_or("key: missing \"keys\"")?;
                let mut handled = false;
                for chord in keys.split_whitespace() {
                    let stroke = Keystroke::parse(chord).map_err(|e| format!("{chord}: {e}"))?;
                    handled |= with_window(window, cx, |w, cx| w.dispatch_keystroke(stroke, cx))?;
                    frame(window, cx).await?;
                }
                json!({ "handled": handled })
            }
            "type" => {
                let text = req["text"].as_str().ok_or("type: missing \"text\"")?;
                for ch in text.chars() {
                    let key = match ch {
                        ' ' => "space".to_string(),
                        c => c.to_lowercase().to_string(),
                    };
                    let stroke = Keystroke {
                        modifiers: Modifiers {
                            shift: ch.is_uppercase(),
                            ..Default::default()
                        },
                        key,
                        key_char: Some(ch.to_string()),
                    };
                    with_window(window, cx, |w, cx| w.dispatch_keystroke(stroke, cx))?;
                }
                frame(window, cx).await?;
                Value::Null
            }
            "click" => {
                let position = point(px(num("x")?), px(num("y")?));
                let count = req["count"].as_u64().unwrap_or(1) as usize;
                let button = match req["button"].as_str() {
                    Some("right") => MouseButton::Right,
                    _ => MouseButton::Left,
                };
                let modifiers = Modifiers::default();
                // Hit-testing reads the last drawn frame, so draw after the
                // move (hover) and again after the click.
                with_window(window, cx, |w, cx| {
                    w.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position,
                            pressed_button: None,
                            modifiers,
                        }),
                        cx,
                    );
                })?;
                frame(window, cx).await?;
                with_window(window, cx, |w, cx| {
                    w.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position,
                            modifiers,
                            click_count: count,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    w.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position,
                            modifiers,
                            click_count: count,
                        }),
                        cx,
                    );
                })?;
                frame(window, cx).await?;
                Value::Null
            }
            "resize" => {
                let (w, h) = (num("width")?, num("height")?);
                with_window(window, cx, |win, _| win.resize(size(px(w), px(h))))?;
                // The platform applies the size on its next turn.
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let now = with_window(window, cx, |win, _| win.viewport_size())?;
                    if (now.width.as_f32() - w).abs() < 1. && (now.height.as_f32() - h).abs() < 1.
                        || Instant::now() > deadline
                    {
                        break;
                    }
                }
                frame(window, cx).await?;
                window_info(window, cx)?
            }
            "wait_idle" => {
                let timeout = req["timeout"].as_f64().unwrap_or(30.);
                let deadline = Instant::now() + Duration::from_secs_f64(timeout);
                // Let a command that just started work register as busy.
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                while cx.read_entity(view, |v, _| v.drive_busy()) {
                    if Instant::now() > deadline {
                        return Err(format!("still busy after {timeout}s"));
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
                frame(window, cx).await?;
                Value::Null
            }
            "sleep" => {
                let secs = num("seconds")?;
                cx.background_executor()
                    .timer(Duration::from_secs_f32(secs))
                    .await;
                frame(window, cx).await?;
                Value::Null
            }
            "frame" => {
                frame(window, cx).await?;
                Value::Null
            }
            "window_info" => window_info(window, cx)?,
            "state" => cx.read_entity(view, |v, _| v.drive_state()),
            "outbox" => {
                let out = outbox().lock().unwrap_or_else(|e| e.into_inner());
                json!({
                    "clipboard": out.clipboard,
                    "notifications": out.notifications,
                    "revealed": out.revealed,
                    "folder_pickers": out.folder_pickers,
                    "opened_urls": out.opened_urls,
                })
            }
            "quit" => {
                cx.update(|cx| cx.quit());
                Value::Null
            }
            other => return Err(format!("unknown command {other:?}")),
        };
        Ok(result)
    }

    fn with_window<R>(
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
        f: impl FnOnce(&mut Window, &mut App) -> R,
    ) -> Result<R, String> {
        cx.update_window(window, |_, w, cx| f(w, cx))
            .map_err(|e| e.to_string())
    }

    fn window_info(window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<Value, String> {
        let (content, scale, id) = with_window(window, cx, |w, _| {
            (
                w.viewport_size(),
                w.scale_factor(),
                native::window_number(w),
            )
        })?;
        Ok(json!({
            "window_id": id,
            "content_width": content.width.as_f32(),
            "content_height": content.height.as_f32(),
            "scale": scale,
        }))
    }

    /// Draw and present a frame now, even if AppKit thinks the window is
    /// covered and has stopped asking for frames.
    async fn frame(window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<(), String> {
        // Let deferred updates from the last event (input changes, spawned
        // follow-ups) land first, or the frame shows the state before them.
        cx.background_executor().timer(SETTLE).await;
        // Mark the whole window dirty and leave the update: the frame
        // callback takes its own borrow of the window.
        let view = with_window(window, cx, |w, _| {
            w.refresh();
            native::view(w)
        })?;
        native::display(view);
        // One more runloop turn so the presented frame is committed before
        // the caller takes a screenshot.
        cx.background_executor().timer(SETTLE).await;
        Ok(())
    }

    fn place_at_back(window: AnyWindowHandle, cx: &mut App) {
        let _ = window.update(cx, |_, w, _| native::order_back(w));
    }

    #[cfg(target_os = "macos")]
    mod native {
        use gpui_kit::Window;
        use objc2::msg_send;
        use objc2::runtime::AnyObject as Object;
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        /// The window's NSView, as an address so it can leave the update.
        pub fn view(window: &Window) -> Option<usize> {
            match HasWindowHandle::window_handle(window).ok()?.as_raw() {
                RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr() as usize),
                _ => None,
            }
        }

        fn ns_window(window: &Window) -> Option<*mut Object> {
            let view = view(window)? as *mut Object;
            let win: *mut Object = unsafe { msg_send![view, window] };
            (!win.is_null()).then_some(win)
        }

        pub fn window_number(window: &Window) -> Option<i64> {
            let win = ns_window(window)?;
            let n: isize = unsafe { msg_send![win, windowNumber] };
            Some(n as i64)
        }

        /// Behind every other window, without activating the app.
        pub fn order_back(window: &Window) {
            if let Some(win) = ns_window(window) {
                let nil: *mut Object = std::ptr::null_mut();
                let _: () = unsafe { msg_send![win, orderBack: nil] };
            }
        }

        /// GPUI's view draws in `displayLayer:`; call it directly, which
        /// works whether or not the display link is running.
        pub fn display(view: Option<usize>) {
            let Some(view) = view else { return };
            let view = view as *mut Object;
            unsafe {
                let layer: *mut Object = msg_send![view, layer];
                let _: () = msg_send![view, displayLayer: layer];
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    mod native {
        use gpui_kit::Window;

        pub fn view(_: &Window) -> Option<usize> {
            None
        }
        pub fn window_number(_: &Window) -> Option<i64> {
            None
        }
        pub fn order_back(_: &Window) {}
        pub fn display(_: Option<usize>) {}
    }
}
