mod ipc_client;
mod keychain;
mod notification;
mod settings;

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

    let app = gpui::Application::new();

    app.run(|cx: &mut gpui::App| {
        // Initialize gpui-component (theme, input, button, etc.)
        gpui_component::init(cx);

        // Store AsyncApp for opening windows from menu events
        *GPUI_APP.lock().unwrap() = Some(AppHolder(cx.to_async()));
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

        // Bridge muda menu events — set_event_handler fires on the main thread
        // within GPUI's NSApplication::run(), so we can handle events directly
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
                        // dispatch_to_main ensures UI updates happen on the main thread
                        dispatch_to_main(refresh_tray);
                    }
                    Err(_) => break,
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

/// Dispatch a function to the main thread (macOS GCD)
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

#[cfg(not(target_os = "macos"))]
fn dispatch_to_main(f: fn()) {
    f();
}

/// Rebuild tray menu and update icon based on current daemon status
fn refresh_tray() {
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
                let _ = holder.0.set_icon_with_as_template(Some(icon), true);
            }
            let menu = build_tray_menu();
            holder.0.set_menu(Some(Box::new(menu)));
        }
    }
}

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
    } else if id == "settings" {
        if let Ok(guard) = GPUI_APP.lock() {
            if let Some(holder) = guard.as_ref() {
                let _ = holder.0.update(|cx| {
                    settings::open_settings(cx);
                });
            }
        }
    } else if id == "quit" {
        if let Some(s) = ipc_client::get_status() {
            if s.status == "connected" {
                ipc_client::disconnect_vpn();
            }
        }
        std::process::exit(0);
    }
}

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
            let daemon = dir.join("fortivpn-daemon");
            if daemon.exists() {
                let _ = Command::new(daemon).spawn();
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}
