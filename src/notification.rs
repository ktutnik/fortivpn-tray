/// No-op — desktop notifications are handled by the UI clients, not the daemon.
pub fn send_notification(_title: &str, _body: &str) {}

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
