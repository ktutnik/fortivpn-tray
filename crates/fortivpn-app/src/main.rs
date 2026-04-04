// Suppress console window on Windows
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ipc_client;
mod keychain;
mod platform;
mod settings;

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

/// Cached VPN status — updated by subscribe thread, read by tray refresh.
static CACHED_STATUS: Mutex<Option<ipc_client::StatusResponse>> = Mutex::new(None);

/// Cached profiles — refreshed periodically, avoids blocking IPC during tray updates.
static CACHED_PROFILES: Mutex<Vec<ipc_client::VpnProfile>> = Mutex::new(Vec::new());

/// Channel sender for subscribe thread → GPUI main thread status updates.
static STATUS_TX: std::sync::OnceLock<async_channel::Sender<ipc_client::StatusResponse>> =
    std::sync::OnceLock::new();

fn main() {
    // Ensure daemon is running
    ensure_daemon();

    // Platform init (Windows: hidden keepalive window setup, etc.)
    platform::init();

    let app = gpui::Application::new();

    app.run(|cx: &mut gpui::App| {
        gpui_component::init(cx);

        *GPUI_APP.lock().unwrap() = Some(AppHolder(cx.to_async()));

        platform::create_keepalive_window(cx);
        platform::hide_from_dock(cx);

        // Fetch initial status and profiles (before building tray menu)
        if let Some(status) = ipc_client::get_status() {
            *CACHED_STATUS.lock().unwrap() = Some(status);
        }
        *CACHED_PROFILES.lock().unwrap() = ipc_client::get_profiles();

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

        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            let id = event.id().as_ref().to_string();
            handle_menu_event(&id);
        }));

        // Create async channel for subscribe thread → main thread
        let (tx, rx) = async_channel::unbounded();
        STATUS_TX.set(tx).ok();

        // GPUI task: await status updates from subscribe thread, refresh tray on main thread.
        // recv().await suspends with zero CPU — no polling, no timer, no battery drain.
        cx.spawn(async move |_cx| {
            while let Ok(status) = rx.recv().await {
                *CACHED_STATUS.lock().unwrap() = Some(status);
                // Only update the icon from the async task — safe, no IPC.
                // Menu rebuild with set_menu() from a GPUI task can break
                // menu event handling on Windows.
                refresh_icon();
            }
        })
        .detach();

        // Subscribe to daemon events in background thread
        std::thread::spawn(|| {
            subscribe_loop();
        });
    });
}

/// Subscribe to daemon status events.
/// Parses status from events and sends to main thread via async channel.
fn subscribe_loop() {
    loop {
        if let Some(reader) = ipc_client::subscribe() {
            use std::io::BufRead;
            for line in reader.lines() {
                match line {
                    Ok(data) => {
                        if let Some(status) = parse_status_event(&data) {
                            if let Some(tx) = STATUS_TX.get() {
                                let _ = tx.send_blocking(status);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        // Reconnect: refresh status and profiles from daemon
        if let Some(status) = ipc_client::get_status() {
            *CACHED_PROFILES.lock().unwrap() = ipc_client::get_profiles();
            if let Some(tx) = STATUS_TX.get() {
                let _ = tx.send_blocking(status);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

/// Parse a StatusResponse from a subscribe event line.
fn parse_status_event(data: &str) -> Option<ipc_client::StatusResponse> {
    let event: serde_json::Value = serde_json::from_str(data).ok()?;
    // Try {"event":"status","data":{...}} format
    if let Some(status_data) = event.get("data") {
        return serde_json::from_value(status_data.clone()).ok();
    }
    // Try direct {"status":"connected",...} format
    serde_json::from_value(event).ok()
}

// ── Tray update functions ────────────────────────────────────────────────────

fn get_cached_connected() -> bool {
    CACHED_STATUS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.status == "connected"))
        .unwrap_or(false)
}

fn refresh_icon() {
    let is_connected = get_cached_connected();

    let icon_bytes = if is_connected {
        include_bytes!("../../../icons/vpn-connected.png").as_slice()
    } else {
        include_bytes!("../../../icons/vpn-disconnected.png").as_slice()
    };

    if let Ok(guard) = TRAY.lock() {
        if let Some(holder) = guard.as_ref() {
            if let Ok(icon) = load_icon(icon_bytes) {
                platform::set_tray_icon(&holder.0, icon);
            }
        }
    }
}

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
        let profiles = CACHED_PROFILES.lock().unwrap().clone();
        if let Some(profile) = profiles.iter().find(|p| p.name == profile_name) {
            if let Some(password) = keychain::read_password(&profile.id) {
                let resp = ipc_client::connect_with_password(&profile.name, &password);
                if let Some(r) = &resp {
                    if r.ok {
                        platform::show_notification(
                            "FortiVPN Connected",
                            &format!("Connected to {}", profile.name),
                        );
                    } else {
                        platform::show_notification("Connection Failed", &r.message);
                    }
                }
                if let Some(status) = ipc_client::get_status() {
                    *CACHED_STATUS.lock().unwrap() = Some(status);
                }
                refresh_tray();
            } else {
                platform::show_notification(
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
        platform::show_notification("FortiVPN Disconnected", "VPN connection closed");
        if let Some(status) = ipc_client::get_status() {
            *CACHED_STATUS.lock().unwrap() = Some(status);
        }
        refresh_tray();
    } else if id == "settings" {
        open_settings_window();
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

// ── Tray menu building ───────────────────────────────────────────────────────

fn build_tray_menu() -> Menu {
    let menu = Menu::new();

    let profiles = CACHED_PROFILES.lock().unwrap().clone();

    let cached = CACHED_STATUS.lock().ok().and_then(|g| g.clone());
    let is_connected = cached
        .as_ref()
        .map(|s| s.status == "connected")
        .unwrap_or(false);
    let is_busy = is_connected
        || cached
            .as_ref()
            .map(|s| s.status == "connecting" || s.status == "disconnecting")
            .unwrap_or(false);
    let connected_name = cached.as_ref().and_then(|s| s.profile.clone());

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
            platform::ensure_daemon(dir);
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
}
