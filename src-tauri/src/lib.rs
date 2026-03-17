mod ipc;
mod keychain;
mod profile;
mod vpn;

use std::sync::{Arc, Mutex};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

use profile::{ProfileStore, VpnProfile};
use vpn::{VpnManager, VpnStatus};

type VpnState = Arc<tokio::sync::Mutex<VpnManager>>;
type StoreState = Arc<Mutex<ProfileStore>>;

/// Word-wrap text to fit within `max_width` characters per line.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for source_line in text.lines() {
        let mut current = String::new();
        for word in source_line.split_whitespace() {
            if current.is_empty() {
                current = word.to_string();
            } else if current.len() + 1 + word.len() > max_width {
                lines.push(current);
                current = word.to_string();
            } else {
                current.push(' ');
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    if lines.is_empty() {
        lines.push(text.to_string());
    }
    lines
}

/// Build an AppleScript notification command string, escaping special characters.
fn build_notification_script(title: &str, message: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        message.replace('\\', "\\\\").replace('"', "\\\""),
        title.replace('\\', "\\\\").replace('"', "\\\""),
    )
}

/// Send a macOS notification with the error details.
fn send_error_notification(title: &str, message: &str) {
    let script = build_notification_script(title, message);
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn();
}

fn build_tray_menu(
    app: &tauri::AppHandle,
    vpn: &VpnManager,
    store: &ProfileStore,
) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;

    // Add profile items
    for p in &store.profiles {
        let is_connected =
            vpn.connected_profile_id() == Some(p.id.as_str()) && vpn.status == VpnStatus::Connected;

        if is_connected {
            let item = MenuItem::with_id(
                app,
                format!("disconnect:{}", p.id),
                format!("\u{25CF} {} \u{2014} Disconnect", p.name),
                true,
                None::<&str>,
            )?;
            menu.append(&item)?;
        } else {
            let enabled = matches!(vpn.status, VpnStatus::Disconnected | VpnStatus::Error(_));
            let item = MenuItem::with_id(
                app,
                format!("connect:{}", p.id),
                format!("\u{25CB} {} \u{2014} Connect", p.name),
                enabled,
                None::<&str>,
            )?;
            menu.append(&item)?;
        }
    }

    // Separator
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    // Status line(s)
    match &vpn.status {
        VpnStatus::Error(e) => {
            let header = MenuItem::with_id(app, "status", "Status: Error", false, None::<&str>)?;
            menu.append(&header)?;
            // Wrap error message into multiple lines (~50 chars each)
            for (i, line) in wrap_text(e, 50).iter().enumerate() {
                let item = MenuItem::with_id(
                    app,
                    format!("error_detail:{i}"),
                    format!("  {line}"),
                    false,
                    None::<&str>,
                )?;
                menu.append(&item)?;
            }
        }
        other => {
            let status_text = match other {
                VpnStatus::Disconnected => "Status: Disconnected".to_string(),
                VpnStatus::Connecting => "Status: Connecting...".to_string(),
                VpnStatus::Connected => {
                    if let Some(pid) = vpn.connected_profile_id() {
                        if let Some(p) = store.get(pid) {
                            format!("Status: Connected to {}", p.name)
                        } else {
                            "Status: Connected".to_string()
                        }
                    } else {
                        "Status: Connected".to_string()
                    }
                }
                VpnStatus::Disconnecting => "Status: Disconnecting...".to_string(),
                _ => unreachable!(),
            };
            let status_item = MenuItem::with_id(app, "status", &status_text, false, None::<&str>)?;
            menu.append(&status_item)?;
        }
    }

    // Separator
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    // Settings
    let settings_item = MenuItem::with_id(app, "settings", "Settings...", true, None::<&str>)?;
    menu.append(&settings_item)?;

    // Quit
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    menu.append(&quit_item)?;

    Ok(menu)
}

pub(crate) fn rebuild_tray(app: &tauri::AppHandle, vpn: &VpnManager, store: &ProfileStore) {
    if let Ok(menu) = build_tray_menu(app, vpn, store) {
        if let Some(tray) = app.tray_by_id("main") {
            let _ = tray.set_menu(Some(menu));
            // Update icon based on connection status
            let icon_bytes: &[u8] = if vpn.status == VpnStatus::Connected {
                include_bytes!("../icons/vpn-connected.png")
            } else {
                include_bytes!("../icons/vpn-disconnected.png")
            };
            if let Ok(icon) = Image::from_bytes(icon_bytes) {
                let _ = tray.set_icon(Some(icon));
                let _ = tray.set_icon_as_template(true);
            }
        }
    }
}

// -- Tauri commands for Settings UI --

#[derive(serde::Serialize)]
struct ProfileView {
    id: String,
    name: String,
    host: String,
    port: u16,
    username: String,
    trusted_cert: String,
}

#[derive(serde::Deserialize)]
struct ProfileInput {
    id: Option<String>,
    name: String,
    host: String,
    port: u16,
    username: String,
    trusted_cert: String,
}

#[tauri::command]
fn get_profiles(store: tauri::State<'_, StoreState>) -> Vec<ProfileView> {
    let store_lock = store.lock().unwrap();
    store_lock
        .profiles
        .iter()
        .map(|p| ProfileView {
            id: p.id.clone(),
            name: p.name.clone(),
            host: p.host.clone(),
            port: p.port,
            username: p.username.clone(),
            trusted_cert: p.trusted_cert.clone(),
        })
        .collect()
}

#[tauri::command]
fn save_profile(
    app: tauri::AppHandle,
    store: tauri::State<'_, StoreState>,
    vpn: tauri::State<'_, VpnState>,
    profile: ProfileInput,
) -> Result<String, String> {
    let id = profile
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let vp = VpnProfile {
        id: id.clone(),
        name: profile.name,
        host: profile.host,
        port: profile.port,
        username: profile.username,
        trusted_cert: profile.trusted_cert,
    };

    let mut store_lock = store.lock().unwrap();
    if store_lock.get(&id).is_some() {
        store_lock.update(vp);
    } else {
        store_lock.add(vp);
    }

    // Rebuild tray (need to block on async lock briefly)
    let vpn_lock = vpn.try_lock().ok();
    if let Some(vpn_guard) = vpn_lock {
        rebuild_tray(&app, &vpn_guard, &store_lock);
    }

    Ok(id)
}

#[tauri::command]
fn delete_profile(
    app: tauri::AppHandle,
    store: tauri::State<'_, StoreState>,
    vpn: tauri::State<'_, VpnState>,
    id: String,
) -> Result<(), String> {
    let mut store_lock = store.lock().unwrap();
    store_lock.remove(&id);
    let _ = keychain::delete_password(&id);

    let vpn_lock = vpn.try_lock().ok();
    if let Some(vpn_guard) = vpn_lock {
        rebuild_tray(&app, &vpn_guard, &store_lock);
    }

    Ok(())
}

#[tauri::command]
fn cmd_set_password(id: String, password: String) -> Result<(), String> {
    keychain::store_password(&id, &password)
}

#[tauri::command]
fn has_password(id: String) -> bool {
    keychain::get_password(&id).is_ok()
}

fn open_settings_window(app: &tauri::AppHandle) {
    // Focus existing window if open
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.set_focus();
        return;
    }

    // Create new settings window
    let _window = tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("/index.html".into()),
    )
    .title("FortiVPN Settings")
    .inner_size(480.0, 520.0)
    .resizable(false)
    .center()
    .build();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            get_profiles,
            save_profile,
            delete_profile,
            cmd_set_password,
            has_password,
        ])
        .setup(|app| {
            let store = ProfileStore::load();

            // Create state
            let vpn_manager = VpnManager::new();

            // Build initial tray menu before wrapping in Arc/Mutex
            let menu = build_tray_menu(app.handle(), &vpn_manager, &store)?;

            // Register state
            let vpn_state: VpnState = Arc::new(tokio::sync::Mutex::new(vpn_manager));
            let store_state: StoreState = Arc::new(Mutex::new(store));
            app.manage(vpn_state);
            app.manage(store_state);

            // Start IPC server for CLI companion
            ipc::start_ipc_server(app.handle().clone());

            // Build tray icon
            let disconnected_icon =
                Image::from_bytes(include_bytes!("../icons/vpn-disconnected.png"))?;
            let _tray = TrayIconBuilder::with_id("main")
                .icon(disconnected_icon)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(true)
                .tooltip("FortiVPN Tray")
                .on_menu_event(|app, event| {
                    let id = event.id().as_ref().to_string();

                    if id.starts_with("connect:") {
                        let profile_id = id.strip_prefix("connect:").unwrap().to_string();
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            handle_connect(&app_handle, &profile_id).await;
                        });
                    } else if id.starts_with("disconnect:") {
                        let _profile_id = id.strip_prefix("disconnect:").unwrap().to_string();
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            handle_disconnect(&app_handle).await;
                        });
                    } else if id == "settings" {
                        open_settings_window(app);
                    } else if id == "quit" {
                        let app_handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            handle_quit(&app_handle).await;
                        });
                    }
                })
                .build(app)?;

            // Hide from dock — must be done AFTER Tauri init (it overrides activation policy)
            #[cfg(target_os = "macos")]
            {
                use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
                if let Some(mtm) = objc2::MainThreadMarker::new() {
                    let ns_app = NSApplication::sharedApplication(mtm);
                    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    // Only prevent auto-exit (no windows), not explicit app.exit()
                    api.prevent_exit();
                }
            }
        });
}

pub(crate) async fn handle_connect(app: &tauri::AppHandle, profile_id: &str) {
    // Get profile data (short lock on store)
    let profile = {
        let store = app.state::<StoreState>();
        let store_lock = store.lock().unwrap();
        store_lock.get(profile_id).cloned()
    };

    let Some(profile) = profile else {
        eprintln!("Profile not found: {profile_id}");
        return;
    };

    // Connect (holds async lock across await — that's fine with tokio::sync::Mutex)
    let result = {
        let vpn = app.state::<VpnState>();
        let mut vpn_lock = vpn.lock().await;
        vpn_lock.connect(&profile).await
    };

    if let Err(e) = &result {
        eprintln!("VPN connect error: {e}");
        send_error_notification("FortiVPN Connection Failed", e);
    }

    // Rebuild menu with updated state
    {
        let vpn = app.state::<VpnState>();
        let vpn_lock = vpn.lock().await;
        let store = app.state::<StoreState>();
        let store_lock = store.lock().unwrap();
        rebuild_tray(app, &vpn_lock, &store_lock);
    }

    // Spawn event-driven monitor for this connection
    {
        let vpn = app.state::<VpnState>();
        let mut vpn_lock = vpn.lock().await;
        if let Some(ref mut session) = vpn_lock.session {
            if let Some(event_rx) = session.take_event_rx() {
                let app_handle = app.clone();
                let handle = tauri::async_runtime::spawn(async move {
                    let mut rx = event_rx;
                    loop {
                        if rx.changed().await.is_err() {
                            break; // sender dropped
                        }
                        let event = rx.borrow().clone();
                        if let fortivpn::VpnEvent::Died(ref reason) = event {
                            let reason = reason.clone();
                            // Update VPN state (short lock scope)
                            {
                                let vpn = app_handle.state::<VpnState>();
                                let mut vpn_lock = vpn.lock().await;
                                vpn_lock.session = None;
                                vpn_lock.connected_profile_id = None;
                                vpn_lock.status = VpnStatus::Error(reason.clone());
                                vpn_lock.monitor_handle = None;
                            }
                            // Send notification and rebuild tray (outside VPN lock)
                            send_error_notification("FortiVPN Disconnected", &reason);
                            {
                                let vpn = app_handle.state::<VpnState>();
                                let vpn_lock = vpn.lock().await;
                                let store = app_handle.state::<StoreState>();
                                let store_lock = store.lock().unwrap();
                                rebuild_tray(&app_handle, &vpn_lock, &store_lock);
                            }
                            break;
                        }
                    }
                });
                vpn_lock.monitor_handle = Some(handle);
            }
        }
    }
}

pub(crate) async fn handle_disconnect(app: &tauri::AppHandle) {
    {
        let vpn = app.state::<VpnState>();
        let mut vpn_lock = vpn.lock().await;
        let _ = vpn_lock.disconnect().await;
    }

    // Rebuild menu
    {
        let vpn = app.state::<VpnState>();
        let vpn_lock = vpn.lock().await;
        let store = app.state::<StoreState>();
        let store_lock = store.lock().unwrap();
        rebuild_tray(app, &vpn_lock, &store_lock);
    }
}

async fn handle_quit(app: &tauri::AppHandle) {
    // Disconnect if connected
    {
        let vpn = app.state::<VpnState>();
        let mut vpn_lock = vpn.lock().await;
        if vpn_lock.status == VpnStatus::Connected {
            let _ = vpn_lock.disconnect().await;
        }
    }
    ipc::cleanup_socket();
    app.exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_text_short() {
        let lines = wrap_text("hello world", 50);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn test_wrap_text_exact_width() {
        let lines = wrap_text("hello world", 11);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn test_wrap_text_wraps() {
        let lines = wrap_text("the quick brown fox jumps over", 15);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "the quick brown");
        assert_eq!(lines[1], "fox jumps over");
    }

    #[test]
    fn test_wrap_text_single_long_word() {
        let lines = wrap_text("superlongwordthatcannotbreak", 10);
        assert_eq!(lines, vec!["superlongwordthatcannotbreak"]);
    }

    #[test]
    fn test_wrap_text_empty() {
        let lines = wrap_text("", 50);
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn test_wrap_text_multiline_input() {
        let lines = wrap_text("line one\nline two", 50);
        assert_eq!(lines, vec!["line one", "line two"]);
    }

    #[test]
    fn test_wrap_text_width_1() {
        let lines = wrap_text("a b c", 1);
        assert_eq!(lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_wrap_text_preserves_words() {
        let text = "Gateway unreachable: Connection timed out after 10 seconds";
        let lines = wrap_text(text, 30);
        for line in &lines {
            assert!(line.len() <= 35); // some tolerance for word boundaries
        }
        let joined = lines.join(" ");
        assert_eq!(joined, text);
    }

    // build_notification_script tests
    #[test]
    fn test_notification_script_basic() {
        let script = build_notification_script("Title", "Message");
        assert_eq!(
            script,
            "display notification \"Message\" with title \"Title\""
        );
    }

    #[test]
    fn test_notification_script_escapes_quotes() {
        let script = build_notification_script("My \"App\"", "He said \"hello\"");
        assert!(script.contains(r#"He said \"hello\""#));
        assert!(script.contains(r#"My \"App\""#));
    }

    #[test]
    fn test_notification_script_escapes_backslash() {
        let script = build_notification_script("Title", "path\\to\\file");
        assert!(script.contains(r"path\\to\\file"));
    }

    #[test]
    fn test_notification_script_empty_strings() {
        let script = build_notification_script("", "");
        assert_eq!(script, "display notification \"\" with title \"\"");
    }

    #[test]
    fn test_notification_script_special_chars() {
        let script = build_notification_script("VPN Error", "Connection failed: timeout (10s)");
        assert!(script.contains("Connection failed: timeout (10s)"));
    }

    // ProfileView serialization tests
    #[test]
    fn test_profile_view_serialization() {
        let pv = ProfileView {
            id: "abc".to_string(),
            name: "Test".to_string(),
            host: "vpn.example.com".to_string(),
            port: 443,
            username: "user".to_string(),
            trusted_cert: "deadbeef".to_string(),
        };
        let json = serde_json::to_string(&pv).unwrap();
        assert!(json.contains("\"id\":\"abc\""));
        assert!(json.contains("\"port\":443"));
        assert!(json.contains("\"trusted_cert\":\"deadbeef\""));
    }

    // ProfileInput deserialization tests
    #[test]
    fn test_profile_input_deserialization_with_id() {
        let json =
            r#"{"id":"x1","name":"VPN","host":"h","port":443,"username":"u","trusted_cert":"c"}"#;
        let input: ProfileInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.id, Some("x1".to_string()));
        assert_eq!(input.name, "VPN");
    }

    #[test]
    fn test_profile_input_deserialization_without_id() {
        let json = r#"{"name":"VPN","host":"h","port":443,"username":"u","trusted_cert":"c"}"#;
        let input: ProfileInput = serde_json::from_str(json).unwrap();
        assert!(input.id.is_none());
        assert_eq!(input.port, 443);
    }

    #[test]
    fn test_profile_input_null_id() {
        let json =
            r#"{"id":null,"name":"VPN","host":"h","port":443,"username":"u","trusted_cert":"c"}"#;
        let input: ProfileInput = serde_json::from_str(json).unwrap();
        assert!(input.id.is_none());
    }
}
