mod app;
mod installer;
mod ipc;
mod keychain;
mod native_ui;
mod notification;
mod profile;
mod vpn;

use std::sync::{Arc, Mutex};

use app::{AppEvent, AppState};
use muda::MenuEvent;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::TrayIconBuilder;

fn main() {
    // Create event loop with custom AppEvent
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Initialize shared state
    let store = profile::ProfileStore::load();
    let vpn_manager = vpn::VpnManager::new();
    let state = AppState {
        vpn: Arc::new(tokio::sync::Mutex::new(vpn_manager)),
        store: Arc::new(Mutex::new(store)),
        proxy: Arc::new(proxy),
    };

    // Start tokio runtime on background thread
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime"),
    );

    // Check helper installation
    if !installer::is_helper_installed() {
        if let Err(e) = installer::install_helper() {
            eprintln!("Helper installation failed: {e}");
        }
    }

    // Start IPC server
    let ipc_state = state.clone();
    rt.spawn(async move {
        ipc::start_ipc_server(ipc_state).await;
    });

    // Build tray icon
    let menu = app::build_tray_menu(&state);
    let icon =
        app::load_icon(include_bytes!("../icons/vpn-disconnected.png")).expect("load tray icon");
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_icon_as_template(true)
        .with_tooltip("FortiVPN Tray")
        .build()
        .expect("Failed to build tray icon");

    // Hide from Dock
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            let ns_app = NSApplication::sharedApplication(mtm);
            ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    // Menu event receiver
    let menu_rx = MenuEvent::receiver();

    // Run macOS event loop (ControlFlow::Wait = sleep until event, near-zero CPU)
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Handle tray menu clicks
        if let Ok(menu_event) = menu_rx.try_recv() {
            let id = menu_event.id().as_ref().to_string();

            if let Some(profile_id) = id.strip_prefix("connect:") {
                let pid = profile_id.to_string();
                let st = state.clone();
                rt.spawn(async move {
                    app::handle_connect(&st, &pid).await;
                });
            } else if id.starts_with("disconnect:") {
                let st = state.clone();
                rt.spawn(async move {
                    app::handle_disconnect(&st).await;
                });
            } else if id == "settings" {
                #[cfg(target_os = "macos")]
                if let Some(mtm) = objc2::MainThreadMarker::new() {
                    native_ui::open_settings(mtm);
                }
            } else if id == "quit" {
                let st = state.clone();
                rt.block_on(async {
                    app::handle_quit(&st).await;
                });
                *control_flow = ControlFlow::Exit;
            }
        }

        // Handle custom events from background threads
        if let Event::UserEvent(app_event) = &event {
            match app_event {
                AppEvent::RebuildTray => {
                    app::rebuild_tray(&tray, &state);
                }
                AppEvent::ShowPasswordPrompt {
                    profile_id,
                    profile_name,
                } => {
                    #[cfg(target_os = "macos")]
                    {
                        let pid = profile_id.clone();
                        let pname = profile_name.clone();
                        if let Some(mtm) = objc2::MainThreadMarker::new() {
                            if let Some(result) = native_ui::show_password_prompt(mtm, &pid, &pname)
                            {
                                let _ =
                                    keychain::store_password(&result.profile_id, &result.password);
                                // Retrigger connect
                                let st = state.clone();
                                let pid = result.profile_id.clone();
                                rt.spawn(async move {
                                    app::handle_connect(&st, &pid).await;
                                });
                            }
                        }
                    }
                }
                AppEvent::Quit => {
                    *control_flow = ControlFlow::Exit;
                }
            }
        }
    });
}
