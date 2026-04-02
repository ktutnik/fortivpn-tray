use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const DAEMON_ADDR: &str = "127.0.0.1:9847";

use crate::profile::ProfileStore;
use crate::vpn::{VpnManager, VpnStatus};

pub type VpnState = Arc<tokio::sync::Mutex<VpnManager>>;
pub type StoreState = Arc<Mutex<ProfileStore>>;

#[derive(Clone)]
pub struct AppState {
    pub vpn: VpnState,
    pub store: StoreState,
    pub status_tx: tokio::sync::broadcast::Sender<String>,
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
    let listener = match TcpListener::bind(DAEMON_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            log::error!(target: "ipc", "Failed to bind TCP listener: {e}");
            return;
        }
    };

    log::info!(target: "ipc", "Listening on {}", DAEMON_ADDR);

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
            let (reader, writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();

            let mut writer = tokio::io::BufWriter::new(writer);
            while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                let cmd = line.trim().to_string();
                if cmd == "subscribe" {
                    handle_subscribe(&state_clone, &mut writer).await;
                    break;
                }
                let response = handle_ipc_command(&state_clone, &cmd).await;
                let json = serde_json::to_string(&response).unwrap_or_default();
                let _ = writer.write_all(format!("{json}\n").as_bytes()).await;
                let _ = writer.flush().await;
                line.clear();
            }
        });
    }
}

async fn handle_subscribe(
    state: &AppState,
    writer: &mut tokio::io::BufWriter<tokio::net::tcp::OwnedWriteHalf>,
) {
    use tokio::io::AsyncWriteExt;
    let mut rx = state.status_tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(event_json) => {
                let line = format!("{}\n", event_json);
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
        }
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
                                    let _ = st.status_tx.send(
                                        serde_json::json!({"event":"status","data":{"status":format!("error: {reason}"),"profile":null}}).to_string()
                                    );
                                });
                                vpn.monitor_handle = Some(handle);
                            }
                        }
                    }
                    log::info!(target: "vpn", "Connected successfully");
                    let _ = state.status_tx.send(
                        serde_json::json!({"event":"status","data":{"status":"connected","profile":profile.name}}).to_string()
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
                    let _ = state.status_tx.send(
                        serde_json::json!({"event":"status","data":{"status":format!("error: {e}"),"profile":null}}).to_string()
                    );
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
            let _ = state.status_tx.send(
                serde_json::json!({"event":"status","data":{"status":"disconnected","profile":null}}).to_string()
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
            IpcResponse {
                ok: true,
                message: "Deleted".into(),
                data: None,
            }
        }

        _ => IpcResponse {
            ok: false,
            message: format!(
                "Unknown command: {command}. Available: status, list, connect_with_password, disconnect, get_profiles, save_profile, delete_profile"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ProfileStore, VpnProfile};

    fn make_store() -> ProfileStore {
        ProfileStore::in_memory(vec![
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
        ])
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
    fn test_daemon_addr() {
        assert_eq!(DAEMON_ADDR, "127.0.0.1:9847");
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
        let store = ProfileStore::in_memory(vec![
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
        ]);
        // Should match by exact ID first
        let result = find_profile(&store, "office");
        assert_eq!(result, Some("office".to_string()));
    }

    #[test]
    fn test_find_profile_unicode() {
        let store = ProfileStore::in_memory(vec![VpnProfile {
            id: "id-1".to_string(),
            name: "日本語 VPN".to_string(),
            host: "h".to_string(),
            port: 443,
            username: "u".to_string(),
            trusted_cert: "".to_string(),
        }]);
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

    // -- IPC command handler tests --

    fn make_state() -> AppState {
        let store = make_store();
        let (status_tx, _) = tokio::sync::broadcast::channel::<String>(16);
        AppState {
            vpn: std::sync::Arc::new(tokio::sync::Mutex::new(crate::vpn::VpnManager::new())),
            store: std::sync::Arc::new(std::sync::Mutex::new(store)),
            status_tx,
        }
    }

    #[tokio::test]
    async fn test_ipc_status_disconnected() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "status").await;
        assert!(resp.ok);
        let data = resp.data.unwrap();
        assert_eq!(data["status"], "disconnected");
        assert!(data["profile"].is_null());
    }

    #[tokio::test]
    async fn test_ipc_list_profiles() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "list").await;
        assert!(resp.ok);
        let data = resp.data.unwrap();
        let profiles = data.as_array().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0]["name"], "Office VPN");
        assert_eq!(profiles[1]["port"], 8443);
    }

    #[tokio::test]
    async fn test_ipc_get_profiles_returns_all_fields() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "get_profiles").await;
        assert!(resp.ok);
        let data = resp.data.unwrap();
        let profiles = data.as_array().unwrap();
        assert_eq!(profiles.len(), 2);
        // Verify all fields present
        let p = &profiles[0];
        assert!(p.get("id").is_some());
        assert!(p.get("name").is_some());
        assert!(p.get("host").is_some());
        assert!(p.get("port").is_some());
        assert!(p.get("username").is_some());
        assert!(p.get("trusted_cert").is_some());
    }

    #[tokio::test]
    async fn test_ipc_save_profile_new() {
        let state = make_state();
        let json = r#"{"name":"New VPN","host":"vpn.new.com","port":443,"username":"new","trusted_cert":""}"#;
        let resp = handle_ipc_command(&state, &format!("save_profile {json}")).await;
        assert!(resp.ok);
        assert_eq!(resp.message, "Saved");
        // Verify ID was generated
        let id = resp.data.unwrap()["id"].as_str().unwrap().to_string();
        assert!(!id.is_empty());
        // Verify profile was added
        let store = state.store.lock().unwrap();
        assert_eq!(store.profiles.len(), 3);
        assert!(store.get(&id).is_some());
    }

    #[tokio::test]
    async fn test_ipc_save_profile_update_existing() {
        let state = make_state();
        let json = r#"{"id":"id-1","name":"Updated VPN","host":"vpn.updated.com","port":9443,"username":"updated","trusted_cert":"abc"}"#;
        let resp = handle_ipc_command(&state, &format!("save_profile {json}")).await;
        assert!(resp.ok);
        let store = state.store.lock().unwrap();
        assert_eq!(store.profiles.len(), 2); // No new profile added
        let p = store.get("id-1").unwrap();
        assert_eq!(p.name, "Updated VPN");
        assert_eq!(p.host, "vpn.updated.com");
        assert_eq!(p.port, 9443);
    }

    #[tokio::test]
    async fn test_ipc_save_profile_invalid_json() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "save_profile not-json").await;
        assert!(!resp.ok);
        assert!(resp.message.contains("Invalid JSON"));
    }

    #[tokio::test]
    async fn test_ipc_save_profile_missing_arg() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "save_profile").await;
        assert!(!resp.ok);
    }

    #[tokio::test]
    async fn test_ipc_delete_profile() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "delete_profile id-1").await;
        assert!(resp.ok);
        assert_eq!(resp.message, "Deleted");
        let store = state.store.lock().unwrap();
        assert_eq!(store.profiles.len(), 1);
        assert!(store.get("id-1").is_none());
    }

    #[tokio::test]
    async fn test_ipc_delete_profile_missing_arg() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "delete_profile").await;
        assert!(!resp.ok);
    }

    #[tokio::test]
    async fn test_ipc_connect_with_password_missing_arg() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "connect_with_password").await;
        assert!(!resp.ok);
    }

    #[tokio::test]
    async fn test_ipc_connect_with_password_invalid_json() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "connect_with_password not-json").await;
        assert!(!resp.ok);
        assert!(resp.message.contains("Invalid JSON"));
    }

    #[tokio::test]
    async fn test_ipc_connect_with_password_missing_name() {
        let state = make_state();
        let resp = handle_ipc_command(&state, r#"connect_with_password {"password":"x"}"#).await;
        assert!(!resp.ok);
        assert!(resp.message.contains("name"));
    }

    #[tokio::test]
    async fn test_ipc_connect_with_password_missing_password() {
        let state = make_state();
        let resp = handle_ipc_command(&state, r#"connect_with_password {"name":"Office"}"#).await;
        assert!(!resp.ok);
        assert!(resp.message.contains("password"));
    }

    #[tokio::test]
    async fn test_ipc_connect_with_password_profile_not_found() {
        let state = make_state();
        let resp = handle_ipc_command(
            &state,
            r#"connect_with_password {"name":"nonexistent","password":"x"}"#,
        )
        .await;
        assert!(!resp.ok);
        assert!(resp.message.contains("not found"));
    }

    #[tokio::test]
    async fn test_ipc_disconnect_when_not_connected() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "disconnect").await;
        assert!(resp.ok);
        assert_eq!(resp.message, "Disconnected");
    }

    #[tokio::test]
    async fn test_ipc_unknown_command() {
        let state = make_state();
        let resp = handle_ipc_command(&state, "foobar").await;
        assert!(!resp.ok);
        assert!(resp.message.contains("Unknown command"));
    }

    #[tokio::test]
    async fn test_broadcast_sends_on_status_change() {
        let state = make_state();
        let mut rx = state.status_tx.subscribe();
        let _ = state.status_tx.send(
            r#"{"event":"status","data":{"status":"disconnected","profile":null}}"#.to_string(),
        );
        let msg = rx.recv().await.unwrap();
        assert!(msg.contains("disconnected"));
    }
}
