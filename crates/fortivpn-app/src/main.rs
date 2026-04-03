// Suppress console window on Windows
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ipc_client;
mod keychain;
mod notification;
mod settings;

#[cfg(unix)]
use std::process::Command;
use std::sync::Mutex;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

// TrayIcon is !Sync, but we only access it from the main thread
struct TrayHolder(TrayIcon);
unsafe impl Send for TrayHolder {}
unsafe impl Sync for TrayHolder {}

static TRAY: Mutex<Option<TrayHolder>> = Mutex::new(None);
struct AppHolder(gpui::AsyncApp);
unsafe impl Send for AppHolder {}
unsafe impl Sync for AppHolder {}

static GPUI_APP: Mutex<Option<AppHolder>> = Mutex::new(None);

fn main() {
    // Ensure daemon is running
    ensure_daemon();

    // Initialize Windows dispatch mechanism before GPUI takes over the event loop
    #[cfg(target_os = "windows")]
    win_dispatch::init();

    let app = gpui::Application::new();

    app.run(|cx: &mut gpui::App| {
        // Initialize gpui-component (theme, input, button, etc.)
        gpui_component::init(cx);

        // Store AsyncApp for opening windows from menu events
        *GPUI_APP.lock().unwrap() = Some(AppHolder(cx.to_async()));

        // Create a hidden window to keep GPUI alive on Windows
        // (GPUI exits when the last window closes, but we need the tray to stay active)
        #[cfg(target_os = "windows")]
        {
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

        // Hide from Dock (macOS)
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
            if let Some(mtm) = objc2::MainThreadMarker::new() {
                let ns_app = NSApplication::sharedApplication(mtm);
                ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            }
        }

        // Build tray icon
        let icon = load_icon(include_bytes!("../../../icons/vpn-disconnected.png"))
            .expect("load tray icon");
        let menu = build_tray_menu();
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_tooltip("FortiVPN Tray")
            .build()
            .expect("Failed to build tray icon");

        *TRAY.lock().unwrap() = Some(TrayHolder(tray));

        // Bridge muda menu events
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            let id = event.id().as_ref().to_string();
            handle_menu_event(&id);
        }));

        // Subscribe to daemon status events in background thread
        std::thread::spawn(|| {
            subscribe_loop();
        });
    });
}

/// Subscribe to daemon status events and refresh tray on changes
fn subscribe_loop() {
    loop {
        if let Some(reader) = ipc_client::subscribe() {
            use std::io::BufRead;
            for line in reader.lines() {
                match line {
                    Ok(_) => {
                        // Dispatch tray refresh to main thread (safe on all platforms)
                        dispatch_to_main(refresh_tray);
                    }
                    Err(_) => break,
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

// ── Platform dispatch: safely run a function on the main thread ──────────────

/// macOS: use GCD dispatch_async_f to the main queue
#[cfg(target_os = "macos")]
fn dispatch_to_main(f: fn()) {
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

/// Windows: use PostMessageW to a message-only window (same pattern as GPUI internally)
#[cfg(target_os = "windows")]
fn dispatch_to_main(f: fn()) {
    win_dispatch::post(f);
}

/// Linux: call directly (GTK tray-icon handles cross-thread updates)
#[cfg(target_os = "linux")]
fn dispatch_to_main(f: fn()) {
    f();
}

// ── Windows dispatch implementation ──────────────────────────────────────────

#[cfg(target_os = "windows")]
mod win_dispatch {
    use std::sync::Mutex;

    // Raw FFI — avoids windows-sys feature flag issues
    type HWND = isize;
    type WPARAM = usize;
    type LPARAM = isize;
    type LRESULT = isize;
    type WNDPROC = Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>;

    const WM_USER: u32 = 0x0400;
    const WM_FORTIVPN_DISPATCH: u32 = WM_USER + 100;
    const HWND_MESSAGE: HWND = -3;

    #[repr(C)]
    struct WNDCLASSW {
        style: u32,
        lpfn_wnd_proc: WNDPROC,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: isize,
        h_icon: isize,
        h_cursor: isize,
        hbr_background: isize,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    extern "system" {
        fn RegisterClassW(lpwndclass: *const WNDCLASSW) -> u16;
        fn CreateWindowExW(
            dwexstyle: u32,
            lpclassname: *const u16,
            lpwindowname: *const u16,
            dwstyle: u32,
            x: i32,
            y: i32,
            nwidth: i32,
            nheight: i32,
            hwndparent: HWND,
            hmenu: isize,
            hinstance: isize,
            lpparam: *const std::ffi::c_void,
        ) -> HWND;
        fn PostMessageW(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> i32;
        fn DefWindowProcW(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    }

    static DISPATCH_HWND: Mutex<Option<HWND>> = Mutex::new(None);
    static PENDING_FNS: Mutex<Vec<fn()>> = Mutex::new(Vec::new());

    /// Create a message-only window on the current (main) thread.
    pub fn init() {
        unsafe {
            let class_name: Vec<u16> = "FortiVPNDispatch\0".encode_utf16().collect();

            let wc = WNDCLASSW {
                style: 0,
                lpfn_wnd_proc: Some(wnd_proc),
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: 0,
                h_icon: 0,
                h_cursor: 0,
                hbr_background: 0,
                lpsz_menu_name: std::ptr::null(),
                lpsz_class_name: class_name.as_ptr(),
            };
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                0,
                0,
                std::ptr::null(),
            );

            if hwnd != 0 {
                *DISPATCH_HWND.lock().unwrap() = Some(hwnd);
            }
        }
    }

    /// Post a function to be executed on the main thread.
    pub fn post(f: fn()) {
        PENDING_FNS.lock().unwrap().push(f);
        if let Some(hwnd) = *DISPATCH_HWND.lock().unwrap() {
            unsafe {
                PostMessageW(hwnd, WM_FORTIVPN_DISPATCH, 0, 0);
            }
        }
    }

    /// Window procedure — processes dispatched messages on the main thread.
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == WM_FORTIVPN_DISPATCH {
            let fns: Vec<fn()> = PENDING_FNS.lock().unwrap().drain(..).collect();
            for f in fns {
                f();
            }
            return 0;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

// ── Tray update functions ────────────────────────────────────────────────────

/// Update tray icon based on current status.
fn refresh_icon() {
    let status = ipc_client::get_status();
    let is_connected = status
        .as_ref()
        .map(|s| s.status == "connected")
        .unwrap_or(false);

    let icon_bytes = if is_connected {
        include_bytes!("../../../icons/vpn-connected.png").as_slice()
    } else {
        include_bytes!("../../../icons/vpn-disconnected.png").as_slice()
    };

    if let Ok(guard) = TRAY.lock() {
        if let Some(holder) = guard.as_ref() {
            if let Ok(icon) = load_icon(icon_bytes) {
                #[cfg(target_os = "macos")]
                let _ = holder.0.set_icon_with_as_template(Some(icon), true);
                #[cfg(not(target_os = "macos"))]
                let _ = holder.0.set_icon(Some(icon));
            }
        }
    }
}

/// Rebuild tray menu AND update icon.
fn refresh_tray() {
    refresh_icon();
    if let Ok(guard) = TRAY.lock() {
        if let Some(holder) = guard.as_ref() {
            let menu = build_tray_menu();
            holder.0.set_menu(Some(Box::new(menu)));
        }
    }
}

// ── Menu event handling ──────────────────────────────────────────────────────

fn handle_menu_event(id: &str) {
    if let Some(profile_name) = id.strip_prefix("connect:") {
        let profiles = ipc_client::get_profiles();
        if let Some(profile) = profiles.iter().find(|p| p.name == profile_name) {
            if let Some(password) = keychain::read_password(&profile.id) {
                let resp = ipc_client::connect_with_password(&profile.name, &password);
                if let Some(r) = &resp {
                    if r.ok {
                        notification::show(
                            "FortiVPN Connected",
                            &format!("Connected to {}", profile.name),
                        );
                    } else {
                        notification::show("Connection Failed", &r.message);
                    }
                }
                refresh_tray();
            } else {
                notification::show(
                    "No Password",
                    &format!(
                        "Set password for {} using CLI: fortivpn set-password",
                        profile_name
                    ),
                );
            }
        }
    } else if id.starts_with("disconnect:") {
        ipc_client::disconnect_vpn();
        notification::show("FortiVPN Disconnected", "VPN connection closed");
        refresh_tray();
    } else if id == "settings" {
        dispatch_to_main(open_settings_window);
    } else if id == "quit" {
        if let Some(s) = ipc_client::get_status() {
            if s.status == "connected" {
                ipc_client::disconnect_vpn();
            }
        }
        std::process::exit(0);
    }
}

fn open_settings_window() {
    if let Ok(guard) = GPUI_APP.lock() {
        if let Some(holder) = guard.as_ref() {
            let _ = holder.0.update(|cx| {
                settings::open_settings(cx);
            });
        }
    }
}

/// Hidden view to keep GPUI event loop alive on Windows
#[cfg(target_os = "windows")]
struct HiddenView;

#[cfg(target_os = "windows")]
impl gpui::Render for HiddenView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

// ── Tray menu building ───────────────────────────────────────────────────────

fn build_tray_menu() -> Menu {
    let menu = Menu::new();

    let profiles = ipc_client::get_profiles();
    let status = ipc_client::get_status();
    let is_connected = status
        .as_ref()
        .map(|s| s.status == "connected")
        .unwrap_or(false);
    let is_busy = is_connected
        || status
            .as_ref()
            .map(|s| s.status == "connecting" || s.status == "disconnecting")
            .unwrap_or(false);
    let connected_name = status.as_ref().and_then(|s| s.profile.clone());

    for profile in &profiles {
        let this_connected = is_connected && connected_name.as_deref() == Some(&profile.name);
        if this_connected {
            let _ = menu.append(&MenuItem::with_id(
                format!("disconnect:{}", profile.name),
                format!("\u{25CF} {} \u{2014} Disconnect", profile.name),
                true,
                None::<muda::accelerator::Accelerator>,
            ));
        } else {
            let _ = menu.append(&MenuItem::with_id(
                format!("connect:{}", profile.name),
                format!("\u{25CB} {} \u{2014} Connect", profile.name),
                !is_busy,
                None::<muda::accelerator::Accelerator>,
            ));
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        "settings",
        "Settings...",
        true,
        None::<muda::accelerator::Accelerator>,
    ));
    let _ = menu.append(&MenuItem::with_id(
        "quit",
        "Quit",
        true,
        None::<muda::accelerator::Accelerator>,
    ));

    menu
}

fn load_icon(bytes: &[u8]) -> Result<Icon, Box<dyn std::error::Error>> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(Icon::from_rgba(rgba.into_raw(), w, h)?)
}

fn ensure_daemon() {
    if ipc_client::is_daemon_running() {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(unix)]
            let daemon_name = "fortivpn-daemon";
            #[cfg(windows)]
            let daemon_name = "fortivpn-daemon.exe";

            let daemon = dir.join(daemon_name);
            if daemon.exists() {
                #[cfg(unix)]
                {
                    let _ = Command::new(&daemon).spawn();
                }

                // On Windows, use ShellExecute with "runas" to trigger UAC elevation
                #[cfg(windows)]
                {
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

                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }
}
