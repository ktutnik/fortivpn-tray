use std::process::Command;

/// No-op on Linux — no pre-init needed.
pub fn init() {}

/// Spawn the daemon binary as a child process.
pub fn ensure_daemon(daemon_dir: &std::path::Path) {
    let daemon = daemon_dir.join("fortivpn-daemon");
    if daemon.exists() {
        let _ = Command::new(&daemon).spawn();
    }
}

/// No-op on Linux — no Dock to hide from.
pub fn hide_from_dock(_cx: &mut gpui::App) {}

/// Show a desktop notification via notify-rust.
pub fn show_notification(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show();
}

/// Set the tray icon (no template mode on Linux).
pub fn set_tray_icon(tray: &tray_icon::TrayIcon, icon: tray_icon::Icon) {
    let _ = tray.set_icon(Some(icon));
}

/// No-op on Linux — GPUI stays alive without a hidden window.
pub fn create_keepalive_window(_cx: &mut gpui::App) {}
