use std::sync::{Arc, Mutex};

use image::GenericImageView;
use muda::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon};

use crate::notification;
use crate::profile::ProfileStore;
use crate::vpn::{VpnManager, VpnStatus};

pub type VpnState = Arc<tokio::sync::Mutex<VpnManager>>;
pub type StoreState = Arc<Mutex<ProfileStore>>;

/// Custom events sent to the main thread via EventLoopProxy.
#[derive(Debug)]
pub enum AppEvent {
    RebuildTray,
    ShowPasswordPrompt {
        profile_id: String,
        profile_name: String,
    },
    Quit,
}

#[derive(Clone)]
pub struct AppState {
    pub vpn: VpnState,
    pub store: StoreState,
    pub proxy: Arc<tao::event_loop::EventLoopProxy<AppEvent>>,
}

/// Word-wrap text for tray menu error display.
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

pub fn build_tray_menu(state: &AppState) -> Menu {
    let menu = Menu::new();
    let vpn = state.vpn.blocking_lock();
    let store = state.store.lock().unwrap();

    for p in &store.profiles {
        let is_connected = vpn.connected_profile_id() == Some(p.id.as_str())
            && vpn.status == VpnStatus::Connected;
        if is_connected {
            let _ = menu.append(&MenuItem::with_id(
                format!("disconnect:{}", p.id),
                format!("\u{25CF} {} \u{2014} Disconnect", p.name),
                true,
                None,
            ));
        } else {
            let enabled = matches!(vpn.status, VpnStatus::Disconnected | VpnStatus::Error(_));
            let _ = menu.append(&MenuItem::with_id(
                format!("connect:{}", p.id),
                format!("\u{25CB} {} \u{2014} Connect", p.name),
                enabled,
                None,
            ));
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());

    match &vpn.status {
        VpnStatus::Error(e) => {
            let _ = menu.append(&MenuItem::with_id("status", "Status: Error", false, None));
            for (i, line) in wrap_text(e, 50).iter().enumerate() {
                let _ = menu.append(&MenuItem::with_id(
                    format!("error_detail:{i}"),
                    format!("  {line}"),
                    false,
                    None,
                ));
            }
        }
        other => {
            let text = match other {
                VpnStatus::Disconnected => "Status: Disconnected".to_string(),
                VpnStatus::Connecting => "Status: Connecting...".to_string(),
                VpnStatus::Connected => vpn
                    .connected_profile_id()
                    .and_then(|id| store.get(id))
                    .map(|p| format!("Status: Connected to {}", p.name))
                    .unwrap_or_else(|| "Status: Connected".to_string()),
                VpnStatus::Disconnecting => "Status: Disconnecting...".to_string(),
                _ => unreachable!(),
            };
            let _ = menu.append(&MenuItem::with_id("status", &text, false, None));
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("settings", "Settings...", true, None));
    let _ = menu.append(&MenuItem::with_id("quit", "Quit", true, None));
    menu
}

pub fn rebuild_tray(tray: &TrayIcon, state: &AppState) {
    let menu = build_tray_menu(state);
    let _ = tray.set_menu(Some(Box::new(menu)));

    let vpn = state.vpn.blocking_lock();
    let icon_bytes: &[u8] = if vpn.status == VpnStatus::Connected {
        include_bytes!("../icons/vpn-connected.png")
    } else {
        include_bytes!("../icons/vpn-disconnected.png")
    };
    if let Ok(icon) = load_icon(icon_bytes) {
        let _ = tray.set_icon(Some(icon));
        tray.set_icon_as_template(true);
    }
}

pub fn load_icon(bytes: &[u8]) -> Result<Icon, Box<dyn std::error::Error>> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(Icon::from_rgba(rgba.into_raw(), w, h)?)
}

pub async fn handle_connect(state: &AppState, profile_id: &str) {
    let profile = {
        let store = state.store.lock().unwrap();
        store.get(profile_id).cloned()
    };
    let Some(profile) = profile else { return };

    let has_pw = {
        let vpn = state.vpn.lock().await;
        vpn.get_password(&profile.id).is_ok()
    };

    if !has_pw {
        let _ = state.proxy.send_event(AppEvent::ShowPasswordPrompt {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        });
        return;
    }

    let result = {
        let mut vpn = state.vpn.lock().await;
        vpn.connect(&profile).await
    };

    if let Err(ref e) = result {
        notification::send_notification("FortiVPN Connection Failed", e);
    }

    let _ = state.proxy.send_event(AppEvent::RebuildTray);

    // Spawn event-driven monitor
    if result.is_ok() {
        let mut vpn = state.vpn.lock().await;
        if let Some(ref mut session) = vpn.session {
            if let Some(event_rx) = session.take_event_rx() {
                let st = state.clone();
                let handle = tokio::spawn(async move {
                    let mut rx = event_rx;
                    loop {
                        if rx.changed().await.is_err() {
                            break;
                        }
                        let event = rx.borrow().clone();
                        if let fortivpn::VpnEvent::Died(ref reason) = event {
                            let reason = reason.clone();
                            {
                                let mut vpn = st.vpn.lock().await;
                                vpn.handle_session_death(reason.clone()).await;
                            }
                            notification::send_notification("FortiVPN Disconnected", &reason);
                            let _ = st.proxy.send_event(AppEvent::RebuildTray);
                            break;
                        }
                    }
                });
                vpn.monitor_handle = Some(handle);
            }
        }
    }
}

pub async fn handle_disconnect(state: &AppState) {
    {
        let mut vpn = state.vpn.lock().await;
        let _ = vpn.disconnect().await;
    }
    let _ = state.proxy.send_event(AppEvent::RebuildTray);
}

pub async fn handle_quit(state: &AppState) {
    {
        let mut vpn = state.vpn.lock().await;
        if vpn.status == VpnStatus::Connected {
            let _ = vpn.disconnect().await;
        }
    }
    crate::ipc::cleanup_socket();
    let _ = state.proxy.send_event(AppEvent::Quit);
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
    fn test_wrap_text_long() {
        let lines = wrap_text("this is a very long error message that should be wrapped", 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.len() <= 25); // some tolerance for word boundaries
        }
    }

    #[test]
    fn test_wrap_text_empty() {
        let lines = wrap_text("", 50);
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn test_wrap_text_multiline_source() {
        let lines = wrap_text("line one\nline two", 50);
        assert_eq!(lines, vec!["line one", "line two"]);
    }

    #[test]
    fn test_load_icon_invalid_bytes() {
        let result = load_icon(&[0, 1, 2, 3]);
        assert!(result.is_err());
    }
}
