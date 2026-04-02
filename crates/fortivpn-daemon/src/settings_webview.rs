//! On-demand WKWebView settings window.
//!
//! Opens a native NSWindow with an embedded WKWebView serving the settings HTML.
//! WebKit is only loaded when Settings is opened — zero battery impact when closed.
//!
//! Dock behavior (like Tailscale):
//! - Settings closed → Accessory policy (no Dock icon)
//! - Settings open → Regular policy (Dock icon with correct name/icon)
//! - Settings closed again → back to Accessory

use std::cell::RefCell;

use tao::event_loop::EventLoopWindowTarget;
use tao::window::Window;
use wry::{WebView, WebViewBuilder};

use crate::app::{AppEvent, AppState};
use crate::keychain;
use crate::profile::VpnProfile;

const SETTINGS_HTML: &str = include_str!("../resources/settings/index.html");

thread_local! {
    static SETTINGS_WINDOW: RefCell<Option<(Window, WebView)>> = const { RefCell::new(None) };
}

/// Open the settings window. If already open, focuses it.
pub fn open_settings(state: &AppState, event_loop: &EventLoopWindowTarget<AppEvent>) {
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

    // Switch to Regular policy so Dock shows proper icon + name
    activate_dock();

    let state_for_ipc = state.clone();
    let state_for_load = state.clone();

    let window = tao::window::WindowBuilder::new()
        .with_title("FortiVPN Settings")
        .with_inner_size(tao::dpi::LogicalSize::new(600.0, 560.0))
        .with_resizable(true)
        .build(event_loop)
        .expect("Failed to create settings window");

    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWindowCollectionBehavior;
        use tao::platform::macos::WindowExtMacOS;

        let ns_window: &objc2_app_kit::NSWindow =
            unsafe { &*(window.ns_window() as *const objc2_app_kit::NSWindow) };
        ns_window.setCollectionBehavior(NSWindowCollectionBehavior::MoveToActiveSpace);
    }

    window.set_focus();

    let init_script = format!(
        "setTimeout(function() {{ updateProfiles({}); }}, 50);",
        build_profiles_json(&state_for_load)
    );

    let webview = WebViewBuilder::new()
        .with_html(SETTINGS_HTML)
        .with_ipc_handler(move |msg| {
            handle_ipc_message(&state_for_ipc, msg.body());
        })
        .with_initialization_script(&init_script)
        .build(&window)
        .expect("Failed to create webview");

    SETTINGS_WINDOW.with(|cell| {
        *cell.borrow_mut() = Some((window, webview));
    });
}

/// Close the settings window and hide from Dock.
pub fn close_settings() {
    SETTINGS_WINDOW.with(|cell| {
        let was_open = cell.borrow().is_some();
        *cell.borrow_mut() = None;
        if was_open {
            // Switch back to Accessory — no Dock icon
            deactivate_dock();
        }
    });
}

/// Switch to Regular activation policy — shows Dock icon with correct name/icon.
fn activate_dock() {
    #[cfg(target_os = "macos")]
    {
        use objc2::AllocAnyThread;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSImage};
        use objc2_foundation::NSData;

        if let Some(mtm) = objc2::MainThreadMarker::new() {
            let ns_app = NSApplication::sharedApplication(mtm);

            // Set icon before switching policy
            let icon_data = include_bytes!("../icons/icon.png");
            let ns_data = NSData::with_bytes(icon_data);
            if let Some(ns_image) = NSImage::initWithData(NSImage::alloc(), &ns_data) {
                unsafe { ns_app.setApplicationIconImage(Some(&ns_image)) };
            }

            // Show in Dock
            ns_app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        }
    }
}

/// Switch back to Accessory activation policy — hides Dock icon.
fn deactivate_dock() {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

        if let Some(mtm) = objc2::MainThreadMarker::new() {
            let ns_app = NSApplication::sharedApplication(mtm);
            ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }
}

fn build_profiles_json(state: &AppState) -> String {
    let store = state.store.lock().unwrap();
    let profiles: Vec<serde_json::Value> = store
        .profiles
        .iter()
        .map(|p| {
            let has_pw = keychain::get_password(&p.id).is_ok();
            serde_json::json!({
                "id": p.id,
                "name": p.name,
                "host": p.host,
                "port": p.port,
                "username": p.username,
                "trusted_cert": p.trusted_cert,
                "has_password": has_pw,
            })
        })
        .collect();
    serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".to_string())
}

fn handle_ipc_message(state: &AppState, msg: &str) {
    let Ok(data) = serde_json::from_str::<serde_json::Value>(msg) else {
        return;
    };

    let cmd = data["cmd"].as_str().unwrap_or("");
    match cmd {
        "save_profile" => {
            let id = data["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            let profile = VpnProfile {
                id: id.clone(),
                name: data["name"].as_str().unwrap_or("").to_string(),
                host: data["host"].as_str().unwrap_or("").to_string(),
                port: data["port"].as_u64().unwrap_or(443) as u16,
                username: data["username"].as_str().unwrap_or("").to_string(),
                trusted_cert: data["trusted_cert"].as_str().unwrap_or("").to_string(),
            };

            {
                let mut store = state.store.lock().unwrap();
                if store.get(&id).is_some() {
                    store.update(profile);
                } else {
                    store.add(profile);
                }
            }

            let _ = state.proxy.send_event(AppEvent::RebuildTray);
        }
        "delete_profile" => {
            if let Some(id) = data["id"].as_str() {
                {
                    let mut store = state.store.lock().unwrap();
                    store.remove(id);
                }
                let _ = keychain::delete_password(id);
                let _ = state.proxy.send_event(AppEvent::RebuildTray);
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
