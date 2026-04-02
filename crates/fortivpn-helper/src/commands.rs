//! Shared command handlers for the privileged helper.
//!
//! These functions handle route and DNS commands and are platform-independent
//! (each function uses `#[cfg]` internally where needed). They accept
//! `&mut impl Write` so they work with any stream type (Unix socket, TCP, etc.).

use std::io::Write;
use std::process::Command;

pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn send_ok(writer: &mut impl Write, extra: Option<serde_json::Value>) -> std::io::Result<()> {
    let mut resp = serde_json::json!({"ok": true});
    if let Some(extra) = extra {
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                resp[k] = v.clone();
            }
        }
    }
    let mut line = serde_json::to_string(&resp)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

pub fn send_error(writer: &mut impl Write, msg: &str) -> std::io::Result<()> {
    let resp = serde_json::json!({"ok": false, "error": msg});
    let mut line = serde_json::to_string(&resp)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

pub fn handle_add_route(msg: &serde_json::Value, writer: &mut impl Write) {
    let dest = msg["dest"].as_str().unwrap_or("");
    let gateway = msg["gateway"].as_str().unwrap_or("");

    if dest.is_empty() {
        let _ = send_error(writer, "Missing 'dest'");
        return;
    }

    match run_route("add", dest, gateway) {
        Ok(()) => {
            let _ = send_ok(writer, None);
        }
        Err(e) => {
            let _ = send_error(writer, &e);
        }
    }
}

pub fn handle_delete_route(msg: &serde_json::Value, writer: &mut impl Write) {
    let dest = msg["dest"].as_str().unwrap_or("");

    if dest.is_empty() {
        let _ = send_error(writer, "Missing 'dest'");
        return;
    }

    match run_route("delete", dest, "") {
        Ok(()) => {
            let _ = send_ok(writer, None);
        }
        Err(e) => {
            let _ = send_error(writer, &e);
        }
    }
}

pub fn handle_configure_dns(msg: &serde_json::Value, writer: &mut impl Write) {
    let servers: Vec<String> = msg["servers"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let search_domain = msg["search_domain"].as_str();

    if servers.is_empty() {
        let _ = send_ok(writer, None);
        return;
    }

    let mut script = format!("d.init\nd.add ServerAddresses * {}\n", servers.join(" "));
    if let Some(domain) = search_domain {
        script.push_str(&format!("d.add SearchDomains * {domain}\n"));
    }
    script.push_str("set State:/Network/Service/fortivpn/DNS\n");

    match run_scutil(&script) {
        Ok(()) => {
            let _ = send_ok(writer, None);
        }
        Err(e) => {
            let _ = send_error(writer, &e);
        }
    }
}

pub fn handle_restore_dns(writer: &mut impl Write) {
    let script = "remove State:/Network/Service/fortivpn/DNS\n";
    let _ = run_scutil(script);
    let _ = send_ok(writer, None);
}

pub fn run_route(action: &str, dest: &str, gateway: &str) -> Result<(), String> {
    let mut args = vec!["-n", action, dest];
    if !gateway.is_empty() {
        args.push(gateway);
    }
    let output = Command::new("/sbin/route")
        .args(&args)
        .output()
        .map_err(|e| format!("route {action}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !(action == "delete" && stderr.contains("not in table")) {
            return Err(format!("route {action} {dest}: {stderr}"));
        }
    }
    Ok(())
}

pub fn run_scutil(script: &str) -> Result<(), String> {
    let output = Command::new("scutil")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(script.as_bytes())?;
            }
            child.wait_with_output()
        })
        .map_err(|e| format!("scutil: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("scutil failed: {stderr}"));
    }
    Ok(())
}
