//! Cross-platform tray UI for FortiVPN (Windows/Linux).
//!
//! Uses tray-icon + muda for system tray, wry for on-demand settings window.
//! Communicates with the fortivpn-daemon via Unix socket IPC.

mod ipc_client;

use std::cell::RefCell;
use std::process::Command;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, TrayIconBuilder};
use wry::{WebView, WebViewBuilder};

const SETTINGS_HTML: &str = include_str!("../resources/settings.html");

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
                // Check password
                let profiles = ipc_client::get_profiles();
                if let Some(profile) = profiles.iter().find(|p| p.name == name) {
                    if !profile.has_password {
                        // TODO: show password dialog
                        eprintln!(
                            "No password set for {}. Use CLI: fortivpn set-password",
                            name
                        );
                    } else {
                        let resp = ipc_client::connect_vpn(&name);
                        if let Some(r) = resp {
                            if !r.ok {
                                eprintln!("Connect failed: {}", r.message);
                            }
                        }
                    }
                }
                update_tray(&tray);
            } else if id.starts_with("disconnect:") {
                ipc_client::disconnect_vpn();
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
                !is_connected,
                None::<muda::accelerator::Accelerator>,
            ));
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    let status_text = if let Some(s) = &status {
        if s.status.starts_with("error") {
            format!("Status: {}", s.status)
        } else if is_connected {
            format!(
                "Status: Connected to {}",
                connected_name.as_deref().unwrap_or("?")
            )
        } else {
            format!("Status: {}", capitalize(&s.status))
        }
    } else {
        "Status: Daemon not running".to_string()
    };
    let _ = menu.append(&MenuItem::with_id(
        "status",
        &status_text,
        false,
        None::<muda::accelerator::Accelerator>,
    ));

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
    serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".to_string())
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
                let _ = ipc_client::set_password(id, pw);
            }
        }
        _ => {}
    }
}

fn load_icon(bytes: &[u8]) -> Result<Icon, Box<dyn std::error::Error>> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(Icon::from_rgba(rgba.into_raw(), w, h)?)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
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
