/// Show a desktop notification. Best effort: a missing notification daemon
/// (a bare Linux session) or a denied permission must not take Guard down.
pub fn notify(title: &str, body: &str) {
    let mut n = notify_rust::Notification::new();
    n.summary(title).body(body).appname("Void");
    #[cfg(target_os = "macos")]
    {
        // Attribute the notification to the bundle when running from one, so
        // it carries Void's icon. A bare `cargo run` binary has no bundle and
        // keeps the default sender.
        let in_bundle = std::env::current_exe()
            .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
            .unwrap_or(false);
        if in_bundle {
            let _ = notify_rust::set_application("com.void.app");
        }
    }
    if let Err(err) = n.show() {
        eprintln!("notification: {err}");
    }
}
