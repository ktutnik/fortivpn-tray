//! Cross-platform tray UI for FortiVPN (Windows/Linux).
//!
//! Uses tray-icon + muda for system tray, wry for on-demand settings window.
//! Communicates with the fortivpn-daemon via Unix socket IPC.

mod ipc_client;
mod keychain;

use std::cell::RefCell;
use std::io::BufRead;
use std::process::Command;
use std::sync::mpsc as std_mpsc;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::{Icon, TrayIconBuilder};
use wry::{WebView, WebViewBuilder};

const SETTINGS_HTML: &str = include_str!("../resources/settings.html");
const PASSWORD_PROMPT_HTML: &str = include_str!("../resources/password-prompt.html");

#[derive(Debug)]
#[allow(dead_code)]
enum AppEvent {
    RebuildTray,
}

thread_local! {
    static SETTINGS_WINDOW: RefCell<Option<(tao::window::Window, WebView)>> = const { RefCell::new(None) };
}

fn main() {
    // Ensure daemon is running
    ensure_daemon();

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let menu_rx = MenuEvent::receiver();
    let proxy = event_loop.create_proxy();

    // Build tray
    let icon =
        load_icon(include_bytes!("../../../icons/vpn-disconnected.png")).expect("load tray icon");
    let menu = build_tray_menu();
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_icon_as_template(true)
        .with_tooltip("FortiVPN Tray")
        .build()
        .expect("Failed to build tray icon");

    // Start subscribe listener in background
    start_subscribe_listener(proxy);

    event_loop.run(move |event, event_loop_target, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Handle window close
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = &event
        {
            SETTINGS_WINDOW.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }

        // Handle menu clicks
        if let Ok(menu_event) = menu_rx.try_recv() {
            let id = menu_event.id().as_ref().to_string();

            if let Some(profile_name) = id.strip_prefix("connect:") {
                let name = profile_name.to_string();
                let profiles = ipc_client::get_profiles();
                if let Some(profile) = profiles.iter().find(|p| p.name == name) {
                    let password = keychain::read_password(&profile.id);
                    if let Some(pw) = password {
                        let resp = ipc_client::connect_with_password(&name, &pw);
                        if let Some(r) = &resp {
                            if r.ok {
                                show_notification(
                                    "FortiVPN Connected",
                                    &format!("Connected to {name}"),
                                );
                            } else {
                                show_notification("Connection Failed", &r.message);
                            }
                        }
                    } else {
                        // Open password prompt window
                        let profile_id = profile.id.clone();
                        let profile_name_clone = name.clone();
                        if let Some((pw, remember)) =
                            open_password_prompt(event_loop_target, &profile_name_clone)
                        {
                            if remember {
                                let _ = keychain::store_password(&profile_id, &pw);
                            }
                            let resp = ipc_client::connect_with_password(&profile_name_clone, &pw);
                            if let Some(r) = &resp {
                                if r.ok {
                                    show_notification(
                                        "FortiVPN Connected",
                                        &format!("Connected to {profile_name_clone}"),
                                    );
                                } else {
                                    show_notification("Connection Failed", &r.message);
                                }
                            }
                        }
                    }
                }
                update_tray(&tray);
            } else if id.starts_with("disconnect:") {
                ipc_client::disconnect_vpn();
                show_notification("FortiVPN Disconnected", "VPN connection closed");
                update_tray(&tray);
            } else if id == "settings" {
                open_settings(event_loop_target);
            } else if id == "quit" {
                let status = ipc_client::get_status();
                if let Some(s) = status {
                    if s.status == "connected" {
                        ipc_client::disconnect_vpn();
                    }
                }
                *control_flow = ControlFlow::Exit;
            }
        }

        // Handle custom events
        if let Event::UserEvent(AppEvent::RebuildTray) = &event {
            update_tray(&tray);
        }
    });
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

fn update_tray(tray: &tray_icon::TrayIcon) {
    let menu = build_tray_menu();
    tray.set_menu(Some(Box::new(menu)));

    let is_connected = ipc_client::get_status()
        .map(|s| s.status == "connected")
        .unwrap_or(false);
    let icon_bytes: &[u8] = if is_connected {
        include_bytes!("../../../icons/vpn-connected.png")
    } else {
        include_bytes!("../../../icons/vpn-disconnected.png")
    };
    if let Ok(icon) = load_icon(icon_bytes) {
        let _ = tray.set_icon(Some(icon));
        tray.set_icon_as_template(true);
    }
}

fn open_settings(event_loop: &tao::event_loop::EventLoopWindowTarget<AppEvent>) {
    // Focus existing window
    let already_open = SETTINGS_WINDOW.with(|cell| {
        let borrow = cell.borrow();
        if let Some((ref window, _)) = *borrow {
            window.set_focus();
            true
        } else {
            false
        }
    });
    if already_open {
        return;
    }

    let window = tao::window::WindowBuilder::new()
        .with_title("FortiVPN Settings")
        .with_inner_size(tao::dpi::LogicalSize::new(600.0, 560.0))
        .with_resizable(true)
        .build(event_loop)
        .expect("Failed to create settings window");

    let profiles_json = build_profiles_json();
    let init_script = format!(
        "setTimeout(function() {{ updateProfiles({}); }}, 50);",
        profiles_json
    );

    let webview = WebViewBuilder::new()
        .with_html(SETTINGS_HTML)
        .with_ipc_handler(handle_settings_ipc)
        .with_initialization_script(&init_script)
        .build(&window)
        .expect("Failed to create webview");

    window.set_focus();

    SETTINGS_WINDOW.with(|cell| {
        *cell.borrow_mut() = Some((window, webview));
    });
}

fn build_profiles_json() -> String {
    let profiles = ipc_client::get_profiles();
    let enriched: Vec<serde_json::Value> = profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "host": p.host,
                "port": p.port,
                "username": p.username,
                "trusted_cert": p.trusted_cert,
                "has_password": keychain::has_password(&p.id),
            })
        })
        .collect();
    serde_json::to_string(&enriched).unwrap_or_else(|_| "[]".to_string())
}

fn handle_settings_ipc(msg: wry::http::Request<String>) {
    let Ok(data) = serde_json::from_str::<serde_json::Value>(msg.body()) else {
        return;
    };

    let cmd = data["cmd"].as_str().unwrap_or("");
    match cmd {
        "save_profile" => {
            let _ = ipc_client::save_profile(&data);
        }
        "delete_profile" => {
            if let Some(id) = data["id"].as_str() {
                let _ = ipc_client::delete_profile(id);
            }
        }
        "set_password" => {
            if let (Some(id), Some(pw)) = (data["id"].as_str(), data["password"].as_str()) {
                let _ = keychain::store_password(id, pw);
            }
        }
        _ => {}
    }
}

fn open_password_prompt(
    event_loop: &tao::event_loop::EventLoopWindowTarget<AppEvent>,
    profile_name: &str,
) -> Option<(String, bool)> {
    let (tx, rx) = std_mpsc::channel::<Option<(String, bool)>>();

    let window = tao::window::WindowBuilder::new()
        .with_title("FortiVPN - Enter Password")
        .with_inner_size(tao::dpi::LogicalSize::new(320.0, 200.0))
        .with_resizable(false)
        .build(event_loop)
        .expect("Failed to create password prompt window");

    let escaped_name = profile_name.replace('\\', "\\\\").replace('\'', "\\'");
    let init_script = format!(
        "setTimeout(function() {{ setProfileName('{}'); }}, 50);",
        escaped_name
    );

    let tx_clone = tx.clone();
    let webview = WebViewBuilder::new()
        .with_html(PASSWORD_PROMPT_HTML)
        .with_ipc_handler(move |msg: wry::http::Request<String>| {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(msg.body()) {
                let action = data["action"].as_str().unwrap_or("");
                match action {
                    "submit" => {
                        let password = data["password"].as_str().unwrap_or("").to_string();
                        let remember = data["remember"].as_bool().unwrap_or(true);
                        let _ = tx_clone.send(Some((password, remember)));
                    }
                    "cancel" => {
                        let _ = tx_clone.send(None);
                    }
                    _ => {}
                }
            }
        })
        .with_initialization_script(&init_script)
        .build(&window)
        .expect("Failed to create password prompt webview");

    window.set_focus();

    // Keep the webview alive until we get a result
    let result = rx.recv().unwrap_or(None);
    drop(webview);
    drop(window);
    result
}

fn show_notification(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show();
}

fn start_subscribe_listener(proxy: EventLoopProxy<AppEvent>) {
    std::thread::spawn(move || {
        let mut last_connected_profile: Option<String> = None;
        let mut reconnect_attempts: u32 = 0;

        loop {
            let reader = match ipc_client::subscribe() {
                Some(r) => r,
                None => {
                    // Daemon not available, retry after a delay
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    continue;
                }
            };

            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break, // Connection lost, reconnect subscribe
                };

                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&line) {
                    let status = data["status"].as_str().unwrap_or("");
                    let profile = data["profile"].as_str().map(|s| s.to_string());

                    if status == "connected" {
                        last_connected_profile = profile;
                        reconnect_attempts = 0;
                    } else if status.starts_with("error") && reconnect_attempts < 3 {
                        // Auto-reconnect on error
                        if let Some(ref prof_name) = last_connected_profile {
                            reconnect_attempts += 1;
                            let name = prof_name.clone();
                            show_notification(
                                "FortiVPN Reconnecting",
                                &format!("Attempt {}/3 for {name}", reconnect_attempts),
                            );
                            std::thread::sleep(std::time::Duration::from_secs(3));

                            // Read password from keychain for reconnect
                            let profiles = ipc_client::get_profiles();
                            if let Some(p) = profiles.iter().find(|p| p.name == name) {
                                if let Some(pw) = keychain::read_password(&p.id) {
                                    let _ = ipc_client::connect_with_password(&name, &pw);
                                }
                            }
                        }
                    } else if status == "disconnected" {
                        last_connected_profile = None;
                        reconnect_attempts = 0;
                    }

                    let _ = proxy.send_event(AppEvent::RebuildTray);
                }
            }

            // If we get here, the subscribe connection dropped. Retry.
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
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
    // Try to launch daemon from next to this binary
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
