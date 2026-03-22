//! IPC client for communicating with the fortivpn-daemon.
//! Connects to the Unix domain socket and sends JSON commands.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub trusted_cert: String,
    pub has_password: bool,
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    pub status: String,
    pub profile: Option<String>,
}

fn socket_path() -> PathBuf {
    dirs::config_dir()
        .expect("Could not find config directory")
        .join("fortivpn-tray")
        .join("ipc.sock")
}

/// Send a command to the daemon and return the response.
pub fn send_command(command: &str) -> Option<IpcResponse> {
    let stream = UnixStream::connect(socket_path()).ok()?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok()?;

    let mut writer = stream.try_clone().ok()?;
    let reader = BufReader::new(stream);

    writeln!(writer, "{}", command).ok()?;
    writer.flush().ok()?;

    let mut line = String::new();
    let mut reader = reader;
    reader.read_line(&mut line).ok()?;

    serde_json::from_str(&line).ok()
}

pub fn get_status() -> Option<StatusResponse> {
    let resp = send_command("status")?;
    if !resp.ok {
        return None;
    }
    serde_json::from_value(resp.data?).ok()
}

pub fn get_profiles() -> Vec<VpnProfile> {
    let resp = match send_command("get_profiles") {
        Some(r) if r.ok => r,
        _ => return vec![],
    };
    match resp.data {
        Some(data) => serde_json::from_value(data).unwrap_or_default(),
        None => vec![],
    }
}

pub fn connect_vpn(name: &str) -> Option<IpcResponse> {
    send_command(&format!("connect {name}"))
}

pub fn disconnect_vpn() -> Option<IpcResponse> {
    send_command("disconnect")
}

pub fn save_profile(json: &serde_json::Value) -> Option<IpcResponse> {
    send_command(&format!(
        "save_profile {}",
        serde_json::to_string(json).ok()?
    ))
}

pub fn delete_profile(id: &str) -> Option<IpcResponse> {
    send_command(&format!("delete_profile {id}"))
}

pub fn set_password(id: &str, password: &str) -> Option<IpcResponse> {
    send_command(&format!("set_password {id} {password}"))
}

#[allow(dead_code)]
pub fn has_password(id: &str) -> bool {
    send_command(&format!("has_password {id}"))
        .and_then(|r| r.data)
        .and_then(|d| d.get("has_password")?.as_bool())
        .unwrap_or(false)
}

pub fn is_daemon_running() -> bool {
    get_status().is_some()
}
