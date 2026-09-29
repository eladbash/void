/// Show a desktop notification. Best effort: a missing notification daemon
/// (a bare Linux session) or a denied permission must not take Guard down.
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    set_sender();
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body).appname("Void");
    if let Err(err) = n.show() {
        eprintln!("notification: {err}");
    }
}

/// Name the app that posts notifications on macOS, once.
///
/// Left unset, the notification library resolves a placeholder app name
/// through AppleScript, which opens a "Choose Application" dialog on the
/// user's screen. Inside the bundle Void posts as itself; a bare `cargo run`
/// binary has no bundle and posts as Finder, the library's own fallback.
#[cfg(target_os = "macos")]
fn set_sender() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let in_bundle = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
            .unwrap_or(false);
        let sender = if in_bundle {
            "com.void.app"
        } else {
            "com.apple.Finder"
        };
        let _ = notify_rust::set_application(sender);
    });
}
