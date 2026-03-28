use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use crate::profile::ProfileStore;
use crate::vpn::{VpnManager, VpnStatus};

pub type VpnState = Arc<tokio::sync::Mutex<VpnManager>>;
pub type StoreState = Arc<Mutex<ProfileStore>>;

#[derive(Clone)]
pub struct AppState {
    pub vpn: VpnState,
    pub store: StoreState,
}

pub fn socket_path() -> PathBuf {
    dirs::config_dir()
        .expect("Could not find config directory")
        .join("fortivpn-tray")
        .join("ipc.sock")
}

#[derive(Serialize, Deserialize)]
pub struct StatusResponse {
    pub status: String,
    pub profile: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ProfileInfo {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

pub async fn start_ipc_server(state: AppState) {
    let sock = socket_path();
    log::info!(target: "ipc", "Socket path: {}", sock.display());

    // Remove stale socket
    let _ = std::fs::remove_file(&sock);

    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            log::error!(target: "ipc", "Failed to bind socket: {e}");
            return;
        }
    };

    // Make socket accessible
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock, PermissionsExt::from_mode(0o600));
    }

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("IPC: accept error: {e}");
                continue;
            }
        };

        let state_clone = state.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let cmd = line.trim().to_string();
                let response = handle_ipc_command(&state_clone, &cmd).await;
                let json = serde_json::to_string(&response).unwrap_or_default();
                let _ = writer.write_all(format!("{json}\n").as_bytes()).await;
                let _ = writer.flush().await;
                line.clear();
            }
        });
    }
}

async fn handle_ipc_command(state: &AppState, cmd: &str) -> IpcResponse {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).map(|s| s.trim());

    log::debug!(target: "ipc", "Command: {command}");

    match command {
        "status" => {
            let vpn_lock = state.vpn.lock().await;
            let store_lock = state.store.lock().unwrap();

            let (status_str, profile_name) = match &vpn_lock.status {
                VpnStatus::Disconnected => ("disconnected".to_string(), None),
                VpnStatus::Connecting => ("connecting".to_string(), None),
                VpnStatus::Connected => {
                    let name = vpn_lock
                        .connected_profile_id()
                        .and_then(|id| store_lock.get(id))
                        .map(|p| p.name.clone());
                    ("connected".to_string(), name)
                }
                VpnStatus::Disconnecting => ("disconnecting".to_string(), None),
                VpnStatus::Error(e) => (format!("error: {e}"), None),
            };

            let data = serde_json::to_value(StatusResponse {
                status: status_str,
                profile: profile_name,
            })
            .ok();

            IpcResponse {
                ok: true,
                message: "ok".into(),
                data,
            }
        }

        "list" => {
            let store_lock = state.store.lock().unwrap();
            let profiles: Vec<ProfileInfo> = store_lock
                .profiles
                .iter()
                .map(|p| ProfileInfo {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    host: p.host.clone(),
                    port: p.port,
                })
                .collect();

            IpcResponse {
                ok: true,
                message: "ok".into(),
                data: serde_json::to_value(profiles).ok(),
            }
        }

        "connect" => {
            let Some(arg) = arg else {
                return IpcResponse {
                    ok: false,
                    message: "Usage: connect <profile-name-or-id>".into(),
                    data: None,
                };
            };

            // Find profile by name (case-insensitive) or ID
            let profile_id = {
                let store_lock = state.store.lock().unwrap();
                find_profile(&store_lock, arg)
            };

            let Some(profile_id) = profile_id else {
                return IpcResponse {
                    ok: false,
                    message: format!("Profile not found: {arg}"),
                    data: None,
                };
            };

            let profile = {
                let store = state.store.lock().unwrap();
                store.get(&profile_id).cloned()
            };

            let Some(profile) = profile else {
                return IpcResponse {
                    ok: false,
                    message: "Profile not found".into(),
                    data: None,
                };
            };

            // Check password
            {
                let vpn = state.vpn.lock().await;
                if vpn.get_password(&profile.id).is_err() {
                    return IpcResponse {
                        ok: false,
                        message: "No password set for this profile".into(),
                        data: None,
                    };
                }
            }

            // Connect
            let result = {
                let mut vpn = state.vpn.lock().await;
                vpn.connect(&profile).await
            };

            match result {
                Ok(()) => {
                    // Spawn event monitor for session death
                    let st = state.clone();
                    {
                        let mut vpn = state.vpn.lock().await;
                        if let Some(ref mut session) = vpn.session {
                            if let Some(event_rx) = session.take_event_rx() {
                                let handle = tokio::spawn(async move {
                                    let mut rx = event_rx;
                                    let reason = loop {
                                        match rx.changed().await {
                                            Ok(()) => {
                                                let event = rx.borrow().clone();
                                                if let fortivpn::VpnEvent::Died(ref r) = event {
                                                    break r.clone();
                                                }
                                            }
                                            Err(_) => break "Connection lost".to_string(),
                                        }
                                    };
                                    log::warn!(target: "vpn", "Session died: {reason}");
                                    {
                                        let mut vpn = st.vpn.lock().await;
                                        vpn.handle_session_death(reason.clone()).await;
                                    }
                                    crate::notification::send_notification(
                                        "FortiVPN Disconnected",
                                        &reason,
                                    );
                                    crate::notification::post_distributed_notification(
                                        "com.fortivpn-tray.status-changed",
                                    );
                                });
                                vpn.monitor_handle = Some(handle);
                            }
                        }
                    }
                    log::info!(target: "vpn", "Connected successfully");
                    crate::notification::post_distributed_notification(
                        "com.fortivpn-tray.status-changed",
                    );
                    IpcResponse {
                        ok: true,
                        message: "Connected".into(),
                        data: None,
                    }
                }
                Err(e) => {
                    log::error!(target: "vpn", "Connection failed: {e}");
                    crate::notification::send_notification("FortiVPN Connection Failed", &e);
                    IpcResponse {
                        ok: false,
                        message: format!("Failed: {e}"),
                        data: None,
                    }
                }
            }
        }

        "connect_with_password" => {
            let Some(args_str) = arg else {
                return IpcResponse {
                    ok: false,
                    message: "Usage: connect_with_password <json>".into(),
                    data: None,
                };
            };
            let Ok(data) = serde_json::from_str::<serde_json::Value>(args_str) else {
                return IpcResponse {
                    ok: false,
                    message: "Invalid JSON. Usage: connect_with_password {\"name\":\"...\",\"password\":\"...\"}".into(),
                    data: None,
                };
            };
            let Some(name) = data["name"].as_str() else {
                return IpcResponse {
                    ok: false,
                    message: "Missing 'name' field".into(),
                    data: None,
                };
            };
            let Some(password) = data["password"].as_str() else {
                return IpcResponse {
                    ok: false,
                    message: "Missing 'password' field".into(),
                    data: None,
                };
            };

            let profile_id = {
                let store_lock = state.store.lock().unwrap();
                find_profile(&store_lock, name)
            };
            let Some(profile_id) = profile_id else {
                return IpcResponse {
                    ok: false,
                    message: format!("Profile not found: {name}"),
                    data: None,
                };
            };
            let profile = {
                let store = state.store.lock().unwrap();
                store.get(&profile_id).cloned()
            };
            let Some(profile) = profile else {
                return IpcResponse {
                    ok: false,
                    message: "Profile not found".into(),
                    data: None,
                };
            };

            let result = {
                let mut vpn = state.vpn.lock().await;
                vpn.connect_with_password(&profile, password).await
            };

            match result {
                Ok(()) => {
                    let st = state.clone();
                    {
                        let mut vpn = state.vpn.lock().await;
                        if let Some(ref mut session) = vpn.session {
                            if let Some(event_rx) = session.take_event_rx() {
                                let handle = tokio::spawn(async move {
                                    let mut rx = event_rx;
                                    let reason = loop {
                                        match rx.changed().await {
                                            Ok(()) => {
                                                let event = rx.borrow().clone();
                                                if let fortivpn::VpnEvent::Died(ref r) = event {
                                                    break r.clone();
                                                }
                                            }
                                            Err(_) => break "Connection lost".to_string(),
                                        }
                                    };
                                    log::warn!(target: "vpn", "Session died: {reason}");
                                    {
                                        let mut vpn = st.vpn.lock().await;
                                        vpn.handle_session_death(reason.clone()).await;
                                    }
                                    crate::notification::send_notification(
                                        "FortiVPN Disconnected",
                                        &reason,
                                    );
                                    crate::notification::post_distributed_notification(
                                        "com.fortivpn-tray.status-changed",
                                    );
                                });
                                vpn.monitor_handle = Some(handle);
                            }
                        }
                    }
                    log::info!(target: "vpn", "Connected successfully");
                    crate::notification::post_distributed_notification(
                        "com.fortivpn-tray.status-changed",
                    );
                    IpcResponse {
                        ok: true,
                        message: "Connected".into(),
                        data: None,
                    }
                }
                Err(e) => {
                    log::error!(target: "vpn", "Connection failed: {e}");
                    crate::notification::send_notification("FortiVPN Connection Failed", &e);
                    IpcResponse {
                        ok: false,
                        message: format!("Failed: {e}"),
                        data: None,
                    }
                }
            }
        }

        "disconnect" => {
            let mut vpn = state.vpn.lock().await;
            let _ = vpn.disconnect().await;
            crate::notification::post_distributed_notification(
                "com.fortivpn-tray.status-changed",
            );
            IpcResponse {
                ok: true,
                message: "Disconnected".into(),
                data: None,
            }
        }

        "get_profiles" => {
            let store = state.store.lock().unwrap();
            let profiles: Vec<serde_json::Value> = store
                .profiles
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "name": p.name,
                        "host": p.host,
                        "port": p.port,
                        "username": p.username,
                        "trusted_cert": p.trusted_cert,
                    })
                })
                .collect();
            IpcResponse {
                ok: true,
                message: "ok".into(),
                data: Some(serde_json::to_value(profiles).unwrap()),
            }
        }

        "save_profile" => {
            let Some(json_str) = arg else {
                return IpcResponse {
                    ok: false,
                    message: "Usage: save_profile <json>".into(),
                    data: None,
                };
            };
            let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) else {
                return IpcResponse {
                    ok: false,
                    message: "Invalid JSON".into(),
                    data: None,
                };
            };
            let id = data["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let profile = crate::profile::VpnProfile {
                id: id.clone(),
                name: data["name"].as_str().unwrap_or("").to_string(),
                host: data["host"].as_str().unwrap_or("").to_string(),
                port: data["port"].as_u64().unwrap_or(443) as u16,
                username: data["username"].as_str().unwrap_or("").to_string(),
                trusted_cert: data["trusted_cert"].as_str().unwrap_or("").to_string(),
            };
            let mut store = state.store.lock().unwrap();
            if store.get(&id).is_some() {
                store.update(profile);
            } else {
                store.add(profile);
            }
            IpcResponse {
                ok: true,
                message: "Saved".into(),
                data: Some(serde_json::json!({"id": id})),
            }
        }

        "delete_profile" => {
            let Some(id) = arg else {
                return IpcResponse {
                    ok: false,
                    message: "Usage: delete_profile <id>".into(),
                    data: None,
                };
            };
            let mut store = state.store.lock().unwrap();
            store.remove(id);
            let _ = crate::keychain::delete_password(id);
            IpcResponse {
                ok: true,
                message: "Deleted".into(),
                data: None,
            }
        }

        "set_password" => {
            let Some(args_str) = arg else {
                return IpcResponse {
                    ok: false,
                    message: "Usage: set_password <id> <password>".into(),
                    data: None,
                };
            };
            let parts: Vec<&str> = args_str.splitn(2, ' ').collect();
            if parts.len() != 2 {
                return IpcResponse {
                    ok: false,
                    message: "Usage: set_password <id> <password>".into(),
                    data: None,
                };
            }
            match crate::keychain::store_password(parts[0], parts[1]) {
                Ok(()) => IpcResponse {
                    ok: true,
                    message: "Password saved".into(),
                    data: None,
                },
                Err(e) => IpcResponse {
                    ok: false,
                    message: format!("Error: {e}"),
                    data: None,
                },
            }
        }

        "has_password" => {
            let Some(id) = arg else {
                return IpcResponse {
                    ok: false,
                    message: "Usage: has_password <id>".into(),
                    data: None,
                };
            };
            // On macOS, Swift app checks Keychain directly to avoid daemon auth prompts.
            // This fallback is for CLI and Linux/Windows where daemon Keychain access works fine.
            let has = crate::keychain::get_password(id).is_ok();
            IpcResponse {
                ok: true,
                message: "ok".into(),
                data: Some(serde_json::json!({"has_password": has})),
            }
        }

        _ => IpcResponse {
            ok: false,
            message: format!(
                "Unknown command: {command}. Available: status, list, connect, disconnect, get_profiles, save_profile, delete_profile, set_password, has_password"
            ),
            data: None,
        },
    }
}

fn find_profile(store: &ProfileStore, query: &str) -> Option<String> {
    // Try exact ID match
    if store.get(query).is_some() {
        return Some(query.to_string());
    }
    // Try case-insensitive name match
    let q = query.to_lowercase();
    store
        .profiles
        .iter()
        .find(|p| p.name.to_lowercase() == q)
        .or_else(|| {
            // Partial name match
            store
                .profiles
                .iter()
                .find(|p| p.name.to_lowercase().contains(&q))
        })
        .map(|p| p.id.clone())
}

#[allow(dead_code)]
pub fn cleanup_socket() {
    let _ = std::fs::remove_file(socket_path());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ProfileStore, VpnProfile};

    fn make_store() -> ProfileStore {
        ProfileStore {
            profiles: vec![
                VpnProfile {
                    id: "id-1".to_string(),
                    name: "Office VPN".to_string(),
                    host: "vpn.office.com".to_string(),
                    port: 443,
                    username: "admin".to_string(),
                    trusted_cert: "".to_string(),
                },
                VpnProfile {
                    id: "id-2".to_string(),
                    name: "Home VPN".to_string(),
                    host: "vpn.home.com".to_string(),
                    port: 8443,
                    username: "user".to_string(),
                    trusted_cert: "".to_string(),
                },
            ],
        }
    }

    #[test]
    fn test_find_profile_by_exact_id() {
        let store = make_store();
        let result = find_profile(&store, "id-1");
        assert_eq!(result, Some("id-1".to_string()));
    }

    #[test]
    fn test_find_profile_by_exact_name() {
        let store = make_store();
        let result = find_profile(&store, "Office VPN");
        assert_eq!(result, Some("id-1".to_string()));
    }

    #[test]
    fn test_find_profile_case_insensitive() {
        let store = make_store();
        let result = find_profile(&store, "office vpn");
        assert_eq!(result, Some("id-1".to_string()));
    }

    #[test]
    fn test_find_profile_partial_match() {
        let store = make_store();
        let result = find_profile(&store, "office");
        assert_eq!(result, Some("id-1".to_string()));
    }

    #[test]
    fn test_find_profile_partial_match_home() {
        let store = make_store();
        let result = find_profile(&store, "home");
        assert_eq!(result, Some("id-2".to_string()));
    }

    #[test]
    fn test_find_profile_not_found() {
        let store = make_store();
        let result = find_profile(&store, "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_profile_empty_store() {
        let store = ProfileStore::default();
        let result = find_profile(&store, "anything");
        assert!(result.is_none());
    }

    #[test]
    fn test_socket_path_contains_ipc_sock() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains("ipc.sock"));
        assert!(path.to_string_lossy().contains("fortivpn-tray"));
    }

    #[test]
    fn test_ipc_response_serialization() {
        let resp = IpcResponse {
            ok: true,
            message: "success".to_string(),
            data: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"message\":\"success\""));
        assert!(!json.contains("\"data\"")); // skip_serializing_if None
    }

    #[test]
    fn test_ipc_response_with_data() {
        let resp = IpcResponse {
            ok: true,
            message: "ok".to_string(),
            data: Some(serde_json::json!({"status": "connected"})),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"data\""));
        assert!(json.contains("connected"));
    }

    #[test]
    fn test_status_response_serialization() {
        let resp = StatusResponse {
            status: "connected".to_string(),
            profile: Some("Office VPN".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("connected"));
        assert!(json.contains("Office VPN"));
    }

    #[test]
    fn test_cleanup_socket_no_panic() {
        // Should not panic even if socket doesn't exist
        cleanup_socket();
    }

    #[test]
    fn test_status_response_disconnected() {
        let resp = StatusResponse {
            status: "disconnected".to_string(),
            profile: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("disconnected"));
        let deserialized: StatusResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.status, "disconnected");
        assert!(deserialized.profile.is_none());
    }

    #[test]
    fn test_ipc_response_deserialization() {
        let json = r#"{"ok":false,"message":"Unknown command: foo. Available: status, list, connect <name>, disconnect"}"#;
        let resp: IpcResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert!(resp.message.contains("Unknown command"));
        assert!(resp.data.is_none());
    }

    #[test]
    fn test_profile_info_deserialization() {
        let json = r#"{"id":"abc","name":"VPN","host":"h.com","port":443}"#;
        let info: ProfileInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.id, "abc");
        assert_eq!(info.port, 443);
    }

    #[test]
    fn test_find_profile_prefers_exact_id_over_name() {
        let store = ProfileStore {
            profiles: vec![
                VpnProfile {
                    id: "office".to_string(),
                    name: "Different Name".to_string(),
                    host: "h".to_string(),
                    port: 443,
                    username: "u".to_string(),
                    trusted_cert: "".to_string(),
                },
                VpnProfile {
                    id: "id-2".to_string(),
                    name: "office".to_string(),
                    host: "h2".to_string(),
                    port: 443,
                    username: "u2".to_string(),
                    trusted_cert: "".to_string(),
                },
            ],
        };
        // Should match by exact ID first
        let result = find_profile(&store, "office");
        assert_eq!(result, Some("office".to_string()));
    }

    #[test]
    fn test_find_profile_unicode() {
        let store = ProfileStore {
            profiles: vec![VpnProfile {
                id: "id-1".to_string(),
                name: "日本語 VPN".to_string(),
                host: "h".to_string(),
                port: 443,
                username: "u".to_string(),
                trusted_cert: "".to_string(),
            }],
        };
        let result = find_profile(&store, "日本語");
        assert_eq!(result, Some("id-1".to_string()));
    }

    #[test]
    fn test_profile_info_serialization() {
        let info = ProfileInfo {
            id: "123".to_string(),
            name: "Test".to_string(),
            host: "host.com".to_string(),
            port: 443,
        };
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: ProfileInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "123");
        assert_eq!(deserialized.port, 443);
    }
}
