// Suppress console window on Windows
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod ipc_client;
mod keychain;
mod platform;
mod settings;

use std::sync::Mutex;

use trayicon::{Icon, MenuBuilder, TrayIcon, TrayIconBuilder};

/// Tray menu events — must be Send + Sync + Clone + PartialEq
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayEvent {
    Connect(usize),    // profile index
    Disconnect(usize), // profile index
    Settings,
    Quit,
}

static TRAY: Mutex<Option<TrayIcon<TrayEvent>>> = Mutex::new(None);

struct AppHolder(gpui::AsyncApp);
unsafe impl Send for AppHolder {}
unsafe impl Sync for AppHolder {}

static GPUI_APP: Mutex<Option<AppHolder>> = Mutex::new(None);

/// Cached VPN status — updated by subscribe thread + async channel.
static CACHED_STATUS: Mutex<Option<ipc_client::StatusResponse>> = Mutex::new(None);

/// Cached profiles.
static CACHED_PROFILES: Mutex<Vec<ipc_client::VpnProfile>> = Mutex::new(Vec::new());

/// Channel sender for subscribe thread → GPUI main thread.
static STATUS_TX: std::sync::OnceLock<async_channel::Sender<ipc_client::StatusResponse>> =
    std::sync::OnceLock::new();

fn main() {
    init_logging();
    log("app", "FortiVPN app starting");

    std::panic::set_hook(Box::new(|info| {
        let msg = format!("PANIC: {info}");
        log("panic", &msg);
        if let Some(dir) = dirs::config_dir() {
            let crash_file = dir.join("fortivpn-tray").join("crash.log");
            let _ = std::fs::write(&crash_file, &msg);
        }
    }));

    ensure_daemon();
    platform::init();

    let app = gpui::Application::new();

    app.run(|cx: &mut gpui::App| {
        gpui_component::init(cx);

        *GPUI_APP.lock().unwrap() = Some(AppHolder(cx.to_async()));

        platform::create_keepalive_window(cx);
        platform::hide_from_dock(cx);

        // Fetch initial status and profiles
        if let Some(status) = ipc_client::get_status() {
            *CACHED_STATUS.lock().unwrap() = Some(status);
        }
        *CACHED_PROFILES.lock().unwrap() = ipc_client::get_profiles();

        // Build tray icon with trayicon crate (Send + Sync, no RefCell)
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let menu = build_tray_menu();

        let icon = make_tray_icon(include_bytes!("../../../icons/vpn-disconnected.png"));
        let mut builder = TrayIconBuilder::new()
            .tooltip("FortiVPN Tray")
            .menu(menu)
            .sender(move |event: &TrayEvent| {
                let _ = event_tx.send(*event);
            });
        if let Ok(icon) = icon {
            builder = builder.icon(icon);
        }

        let tray = builder.build().expect("Failed to build tray icon");
        *TRAY.lock().unwrap() = Some(tray);

        // Async channel: subscribe thread → GPUI spawn → update cache + tray
        let (status_tx, status_rx) = async_channel::unbounded();
        STATUS_TX.set(status_tx).ok();

        // GPUI spawn: receive status updates and refresh tray.
        // trayicon is Send+Sync so set_icon/set_menu is safe from any context.
        cx.spawn(async move |_cx| {
            log("spawn", "Status listener started");
            while let Ok(status) = status_rx.recv().await {
                log("spawn", &format!("Status: {}", status.status));
                *CACHED_STATUS.lock().unwrap() = Some(status);
                refresh_tray();
                log("spawn", "Tray refreshed");
            }
        })
        .detach();

        // Poll menu events from trayicon's mpsc channel
        // (trayicon uses callback → mpsc, we poll from GPUI timer)
        cx.spawn(async move |cx| loop {
            while let Ok(event) = event_rx.try_recv() {
                handle_tray_event(event);
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
        })
        .detach();

        // Subscribe to daemon events
        std::thread::spawn(|| {
            subscribe_loop();
        });

        log("app", "App initialized");
    });
}

/// Subscribe to daemon status events.
fn subscribe_loop() {
    log("subscribe", "Subscribe loop started");
    loop {
        if let Some(reader) = ipc_client::subscribe() {
            log("subscribe", "Connected");
            use std::io::BufRead;
            for line in reader.lines() {
                match line {
                    Ok(data) => {
                        if let Some(status) = parse_status_event(&data) {
                            log("subscribe", &format!("Status: {}", status.status));
                            if let Some(tx) = STATUS_TX.get() {
                                let _ = tx.send_blocking(status);
                            }
                        }
                    }
                    Err(e) => {
                        log("subscribe", &format!("Error: {e}"));
                        break;
                    }
                }
            }
        }
        // Reconnect: refresh profiles
        if let Some(status) = ipc_client::get_status() {
            *CACHED_PROFILES.lock().unwrap() = ipc_client::get_profiles();
            if let Some(tx) = STATUS_TX.get() {
                let _ = tx.send_blocking(status);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

fn parse_status_event(data: &str) -> Option<ipc_client::StatusResponse> {
    let event: serde_json::Value = serde_json::from_str(data).ok()?;
    if let Some(status_data) = event.get("data") {
        return serde_json::from_value(status_data.clone()).ok();
    }
    serde_json::from_value(event).ok()
}

// ── Tray update ──────────────────────────────────────────────────────────────

fn refresh_tray() {
    let is_connected = CACHED_STATUS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.status == "connected"))
        .unwrap_or(false);

    if let Ok(mut guard) = TRAY.lock() {
        if let Some(tray) = guard.as_mut() {
            // Update icon
            let icon_bytes: &'static [u8] = if is_connected {
                include_bytes!("../../../icons/vpn-connected.png")
            } else {
                include_bytes!("../../../icons/vpn-disconnected.png")
            };

            if let Ok(icon) = make_tray_icon(icon_bytes) {
                let _ = tray.set_icon(&icon);
            }

            // Update menu
            let menu = build_tray_menu();
            let _ = tray.set_menu(&menu);
        }
    }
}

// ── Event handling ───────────────────────────────────────────────────────────

fn handle_tray_event(event: TrayEvent) {
    match event {
        TrayEvent::Connect(index) => {
            let profiles = CACHED_PROFILES.lock().unwrap().clone();
            if let Some(profile) = profiles.get(index) {
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
                            profile.name
                        ),
                    );
                }
            }
        }
        TrayEvent::Disconnect(_) => {
            ipc_client::disconnect_vpn();
            platform::show_notification("FortiVPN Disconnected", "VPN connection closed");
            if let Some(status) = ipc_client::get_status() {
                *CACHED_STATUS.lock().unwrap() = Some(status);
            }
            refresh_tray();
        }
        TrayEvent::Settings => {
            if let Ok(guard) = GPUI_APP.lock() {
                if let Some(holder) = guard.as_ref() {
                    let _ = holder.0.update(|cx| {
                        settings::open_settings(cx);
                    });
                }
            }
        }
        TrayEvent::Quit => {
            if let Some(s) = ipc_client::get_status() {
                if s.status == "connected" {
                    ipc_client::disconnect_vpn();
                }
            }
            std::process::exit(0);
        }
    }
}

// ── Menu building ────────────────────────────────────────────────────────────

fn build_tray_menu() -> MenuBuilder<TrayEvent> {
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

    let mut menu = MenuBuilder::new();

    for (i, profile) in profiles.iter().enumerate() {
        let this_connected = is_connected && connected_name.as_deref() == Some(&profile.name);
        if this_connected {
            menu = menu.item(
                &format!("\u{25CF} {} \u{2014} Disconnect", profile.name),
                TrayEvent::Disconnect(i),
            );
        } else {
            // trayicon doesn't support disabled items directly via .item()
            // Use MenuItem::Item with disabled flag for busy state
            menu = menu.with(trayicon::MenuItem::Item {
                id: TrayEvent::Connect(i),
                name: format!("\u{25CB} {} \u{2014} Connect", profile.name),
                disabled: is_busy,
                icon: None,
            });
        }
    }

    menu = menu
        .separator()
        .item("Settings...", TrayEvent::Settings)
        .item("Quit", TrayEvent::Quit);

    menu
}

/// Create a tray icon with correct sizing for each platform.
/// macOS: 22x22 points (44px Retina), template mode for menu bar.
/// Windows/Linux: use native size.
fn make_tray_icon(buffer: &'static [u8]) -> Result<Icon, trayicon::Error> {
    #[cfg(target_os = "macos")]
    {
        let mut icon = Icon::from_buffer(buffer, Some(22), Some(22))?;
        icon.set_template(true);
        Ok(icon)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Icon::from_buffer(buffer, None, None)
    }
}

// ── Logging ──────────────────────────────────────────────────────────────────

fn init_logging() {
    if let Some(dir) = dirs::config_dir() {
        let log_dir = dir.join("fortivpn-tray");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_file = log_dir.join("app.log");
        let _ = std::fs::write(
            &log_file,
            format!(
                "=== FortiVPN App started at {:?} ===\n",
                std::time::SystemTime::now()
            ),
        );
    }
}

fn log(tag: &str, msg: &str) {
    if let Some(dir) = dirs::config_dir() {
        let log_file = dir.join("fortivpn-tray").join("app.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
        {
            let _ = writeln!(f, "[{tag}] {msg}");
        }
    }
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
