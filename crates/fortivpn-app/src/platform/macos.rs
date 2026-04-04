use std::process::Command;

/// No-op on macOS — no pre-init needed.
pub fn init() {}

/// Spawn the daemon binary as a child process.
pub fn ensure_daemon(daemon_dir: &std::path::Path) {
    let daemon = daemon_dir.join("fortivpn-daemon");
    if daemon.exists() {
        let _ = Command::new(&daemon).spawn();
    }
}

/// Hide the app from the macOS Dock by setting Accessory activation policy.
pub fn hide_from_dock(_cx: &mut gpui::App) {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let ns_app = NSApplication::sharedApplication(mtm);
        ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Show a desktop notification via osascript (avoids "Choose Application" dialog for non-bundled binaries).
pub fn show_notification(title: &str, body: &str) {
    let safe_title = title.replace('"', "\\\"");
    let safe_body = body.replace('"', "\\\"");
    let script = format!("display notification \"{safe_body}\" with title \"{safe_title}\"");
    let _ = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .spawn();
}

/// Set the tray icon with template mode enabled (macOS renders as template image).
pub fn set_tray_icon(tray: &tray_icon::TrayIcon, icon: tray_icon::Icon) {
    let _ = tray.set_icon_with_as_template(Some(icon), true);
}

/// No-op on macOS — GPUI stays alive without a hidden window.
pub fn create_keepalive_window(_cx: &mut gpui::App) {}

/// Dispatch a function to the main thread via GCD.
pub fn dispatch_to_main(f: fn()) {
    use std::ffi::c_void;
    extern "C" {
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
        static _dispatch_main_q: c_void;
    }

    extern "C" fn trampoline(ctx: *mut c_void) {
        let f: fn() = unsafe { std::mem::transmute(ctx) };
        f();
    }

    unsafe {
        let main_q = &raw const _dispatch_main_q;
        dispatch_async_f(main_q, f as *mut c_void, trampoline);
    }
}
