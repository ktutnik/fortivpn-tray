use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn socket_path() -> PathBuf {
    dirs::config_dir()
        .expect("Could not find config directory")
        .join("fortivpn-tray")
        .join("ipc.sock")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_usage();
        return;
    }

    let command = match args[0].as_str() {
        "status" | "s" => "status".to_string(),
        "list" | "ls" | "l" => "list".to_string(),
        "connect" | "c" => {
            if args.len() < 2 {
                eprintln!("Error: connect requires a profile name");
                eprintln!("Usage: fortivpn connect <profile-name>");
                std::process::exit(1);
            }
            format!("connect {}", args[1..].join(" "))
        }
        "disconnect" | "dc" | "d" => "disconnect".to_string(),
        "help" | "--help" | "-h" => {
            print_usage();
            return;
        }
        other => {
            // Treat as profile name shortcut: `fortivpn sg` = `fortivpn connect sg`
            format!("connect {other}")
        }
    };

    let sock = socket_path();
    let mut stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("Cannot connect to fortivpn-tray. Is the tray app running?");
            std::process::exit(1);
        }
    };

    // Send command
    writeln!(stream, "{command}").expect("Failed to send command");
    stream.flush().expect("Failed to flush");

    // Read response
    let reader = BufReader::new(&stream);
    for line in reader.lines() {
        let line = line.expect("Failed to read response");
        let resp: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                println!("{line}");
                break;
            }
        };

        if let Some(msg) = resp.get("message").and_then(|m| m.as_str()) {
            if resp.get("ok").and_then(|o| o.as_bool()) == Some(true) {
                // Format output based on command
                if let Some(data) = resp.get("data") {
                    if let Some(status) = data.get("status") {
                        // Status response
                        let s = status.as_str().unwrap_or("unknown");
                        let profile = data
                            .get("profile")
                            .and_then(|p| p.as_str())
                            .unwrap_or("");
                        if profile.is_empty() {
                            println!("VPN: {s}");
                        } else {
                            println!("VPN: {s} ({profile})");
                        }
                    } else if let Some(arr) = data.as_array() {
                        // List response
                        if arr.is_empty() {
                            println!("No profiles configured.");
                        } else {
                            println!("Profiles:");
                            for p in arr {
                                let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                let host = p.get("host").and_then(|h| h.as_str()).unwrap_or("?");
                                let port = p.get("port").and_then(|p| p.as_u64()).unwrap_or(0);
                                println!("  {name} ({host}:{port})");
                            }
                        }
                    } else {
                        println!("{msg}");
                    }
                } else {
                    println!("{msg}");
                }
            } else {
                eprintln!("Error: {msg}");
                std::process::exit(1);
            }
        }
        break; // One response per command
    }
}

fn print_usage() {
    println!("fortivpn - CLI for FortiVPN Tray");
    println!();
    println!("Usage:");
    println!("  fortivpn status          Show VPN connection status");
    println!("  fortivpn list            List available profiles");
    println!("  fortivpn connect <name>  Connect to a VPN profile");
    println!("  fortivpn disconnect      Disconnect VPN");
    println!("  fortivpn <name>          Shortcut for connect");
    println!();
    println!("Aliases: s=status, l/ls=list, c=connect, d/dc=disconnect");
    println!();
    println!("The tray app must be running for this CLI to work.");
}
