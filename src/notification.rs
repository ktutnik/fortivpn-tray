/// Send a desktop notification.
/// On macOS, the Swift UI app handles notifications — this is a no-op.
/// On Linux/Windows, uses notify-rust.
pub fn send_notification(_title: &str, _body: &str) {
    // notify-rust hangs on macOS in headless daemons (no RunLoop).
    // The Swift UI app receives distributed notifications and shows alerts via osascript.
    #[cfg(not(target_os = "macos"))]
    {
        let _ = notify_rust::Notification::new()
            .summary(_title)
            .body(_body)
            .show();
    }
}

/// Post a macOS distributed notification so the Swift UI app can react instantly.
/// Spawns a swift process because CFNotificationCenter requires a CFRunLoop,
/// which tokio worker threads don't have.
pub fn post_distributed_notification(name: &str) {
    let code = format!(
        "import Foundation; DistributedNotificationCenter.default().postNotificationName(NSNotification.Name(\"{name}\"), object: nil, userInfo: nil, deliverImmediately: true)"
    );
    let _ = std::process::Command::new("/usr/bin/swift")
        .args(["-e", &code])
        .spawn();
}
