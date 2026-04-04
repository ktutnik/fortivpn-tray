use std::sync::Mutex;

/// Initialize the Windows dispatch mechanism (message-only window for PostMessageW).
pub fn init() {
    win_dispatch::init();
}

/// Post a function to the main thread via PostMessageW.
pub fn dispatch_to_main(f: fn()) {
    win_dispatch::post(f);
}

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

/// Set the tray icon (no template mode on Windows).
pub fn set_tray_icon(tray: &tray_icon::TrayIcon, icon: tray_icon::Icon) {
    let _ = tray.set_icon(Some(icon));
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

// ── Windows dispatch via PostMessageW ────────────────────────────────────────

mod win_dispatch {
    use std::sync::Mutex;

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

    pub fn post(f: fn()) {
        PENDING_FNS.lock().unwrap().push(f);
        if let Some(hwnd) = *DISPATCH_HWND.lock().unwrap() {
            unsafe {
                PostMessageW(hwnd, WM_FORTIVPN_DISPATCH, 0, 0);
            }
        }
    }

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
