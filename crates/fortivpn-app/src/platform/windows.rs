/// No-op on Windows — dispatch replaced by async-channel.
pub fn init() {}

/// Launch the daemon binary elevated via ShellExecuteW with "runas".
pub fn ensure_daemon(daemon_dir: &std::path::Path) {
    let daemon = daemon_dir.join("fortivpn-daemon.exe");
    if daemon.exists() {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = daemon
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let verb: Vec<u16> = "runas\0".encode_utf16().collect();
        unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0, // SW_HIDE
            );
        }
    }
}

/// No-op on Windows — no Dock to hide from.
pub fn hide_from_dock(_cx: &mut gpui::App) {}

/// Show a desktop notification via notify-rust.
pub fn show_notification(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show();
}
/// Create a hidden window to keep the GPUI event loop alive on Windows.
pub fn create_keepalive_window(cx: &mut gpui::App) {
    use gpui::{px, AppContext};
    let _ = cx.open_window(
        gpui::WindowOptions {
            show: false,
            focus: false,
            window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                gpui::point(px(0.), px(0.)),
                gpui::size(px(1.), px(1.)),
            ))),
            ..Default::default()
        },
        |_window, cx| cx.new(|_| HiddenView),
    );
}

/// Hidden view to keep GPUI event loop alive on Windows.
struct HiddenView;

impl gpui::Render for HiddenView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}
