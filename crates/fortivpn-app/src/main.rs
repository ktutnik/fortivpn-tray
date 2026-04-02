#[allow(dead_code)]
mod ipc_client;
#[allow(dead_code)]
mod keychain;
mod notification;

use std::process::Command;

use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

fn main() {
    // Ensure daemon is running
    ensure_daemon();

    // Initialize GPUI app (needed for event loop)
    let app = gpui::Application::new();

    app.run(|cx: &mut gpui::App| {
        // Hide from Dock — tray-only app
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
        let _tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_icon_as_template(true)
            .with_tooltip("FortiVPN Tray")
            .build()
            .expect("Failed to build tray icon");

        // Menu event handling
        let menu_rx = MenuEvent::receiver();

        // Use GPUI's timer to poll menu events
        cx.spawn(async move |cx| loop {
            if let Ok(event) = menu_rx.try_recv() {
                let id = event.id().as_ref().to_string();
                handle_menu_event(&id);
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
        })
        .detach();
    });
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
        // TODO: Open GPUI settings window
        eprintln!("Settings window not yet implemented");
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
