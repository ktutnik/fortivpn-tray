use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn socket_path() -> PathBuf {
    dirs::config_dir()
        .expect("Could not find config directory")
        .join("fortivpn-tray")
        .join("ipc.sock")
}

fn send_command(stream: &mut UnixStream, command: &str) -> String {
    writeln!(stream, "{command}").expect("Failed to send command");
    stream.flush().expect("Failed to flush");

    let reader = BufReader::new(&*stream);
    let Some(Ok(line)) = reader.lines().next() else {
        eprintln!("No response from tray app");
        std::process::exit(1);
    };
    line
}

fn connect_stream() -> UnixStream {
    let sock = socket_path();
    match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("Cannot connect to fortivpn-tray. Is the tray app running?");
            std::process::exit(1);
        }
    }
}

/// Find a profile by partial/case-insensitive name match from the profile list.
/// Returns (profile_id, profile_name) on success.
fn find_profile(profiles: &[serde_json::Value], query: &str) -> Option<(String, String)> {
    // Try exact ID match
    for p in profiles {
        if p.get("id").and_then(|v| v.as_str()) == Some(query) {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Some((query.to_string(), name));
        }
    }
    // Try case-insensitive exact name match
    let q = query.to_lowercase();
    for p in profiles {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.to_lowercase() == q {
            let id = p
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Some((id, name.to_string()));
        }
    }
    // Try partial name match
    for p in profiles {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.to_lowercase().contains(&q) {
            let id = p
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return Some((id, name.to_string()));
        }
    }
    None
}

fn handle_connect(query: &str) {
    // 1. Send `list` to get all profiles
    let mut stream = connect_stream();
    let list_resp = send_command(&mut stream, "list");
    drop(stream);

    let resp: serde_json::Value = match serde_json::from_str(&list_resp) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("Invalid response from daemon: {list_resp}");
            std::process::exit(1);
        }
    };

    if resp.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        let msg = resp
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error");
        eprintln!("Error: {msg}");
        std::process::exit(1);
    }

    let profiles = resp
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    // 2. Find matching profile
    let Some((profile_id, profile_name)) = find_profile(&profiles, query) else {
        eprintln!("Profile not found: {query}");
        std::process::exit(1);
    };

    // 3. Try reading password from keychain
    let password = match keyring::Entry::new("fortivpn-tray", &profile_id) {
        Ok(entry) => match entry.get_password() {
            Ok(pw) => pw,
            Err(_) => prompt_and_maybe_save_password(&profile_id),
        },
        Err(_) => prompt_and_maybe_save_password(&profile_id),
    };

    // 4. Send connect_with_password
    let payload = serde_json::json!({
        "name": profile_name,
        "password": password,
    });
    let mut stream = connect_stream();
    let connect_resp = send_command(&mut stream, &format!("connect_with_password {}", payload));

    // 5. Display response
    display_response(&connect_resp);
}

fn prompt_and_maybe_save_password(profile_id: &str) -> String {
    let password = rpassword::prompt_password_stderr("Password: ").unwrap_or_else(|e| {
        eprintln!("Failed to read password: {e}");
        std::process::exit(1);
    });

    // Ask if they want to save to keychain
    eprint!("Save password to keychain? [y/N] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("y") {
        match keyring::Entry::new("fortivpn-tray", profile_id) {
            Ok(entry) => {
                if let Err(e) = entry.set_password(&password) {
                    eprintln!("Failed to save password to keychain: {e}");
                } else {
                    eprintln!("Password saved to keychain.");
                }
            }
            Err(e) => eprintln!("Failed to access keychain: {e}"),
        }
    }

    password
}

fn handle_set_password(args: &[String]) {
    if args.len() < 3 {
        eprintln!("Usage: fortivpn set-password <profile-id> <password>");
        std::process::exit(1);
    }
    let profile_id = &args[1];
    let password = &args[2];

    match keyring::Entry::new("fortivpn-tray", profile_id) {
        Ok(entry) => match entry.set_password(password) {
            Ok(()) => println!("Password saved to keychain."),
            Err(e) => {
                eprintln!("Failed to save password: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Failed to access keychain: {e}");
            std::process::exit(1);
        }
    }
}

fn display_response(line: &str) {
    let resp: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            println!("{line}");
            return;
        }
    };

    if let Some(msg) = resp.get("message").and_then(|m| m.as_str()) {
        if resp.get("ok").and_then(|o| o.as_bool()) == Some(true) {
            if let Some(data) = resp.get("data") {
                if let Some(status) = data.get("status") {
                    let s = status.as_str().unwrap_or("unknown");
                    let profile = data.get("profile").and_then(|p| p.as_str()).unwrap_or("");
                    if profile.is_empty() {
                        println!("VPN: {s}");
                    } else {
                        println!("VPN: {s} ({profile})");
                    }
                } else if let Some(arr) = data.as_array() {
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
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        print_usage();
        return;
    }

    match args[0].as_str() {
        "status" | "s" => {
            let mut stream = connect_stream();
            let resp = send_command(&mut stream, "status");
            display_response(&resp);
        }
        "list" | "ls" | "l" => {
            let mut stream = connect_stream();
            let resp = send_command(&mut stream, "list");
            display_response(&resp);
        }
        "connect" | "c" => {
            if args.len() < 2 {
                eprintln!("Error: connect requires a profile name");
                eprintln!("Usage: fortivpn connect <profile-name>");
                std::process::exit(1);
            }
            let query = args[1..].join(" ");
            handle_connect(&query);
        }
        "disconnect" | "dc" | "d" => {
            let mut stream = connect_stream();
            let resp = send_command(&mut stream, "disconnect");
            display_response(&resp);
        }
        "set-password" => {
            handle_set_password(&args);
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        other => {
            // Treat as profile name shortcut: `fortivpn sg` = `fortivpn connect sg`
            let query = if args.len() > 1 {
                args.join(" ")
            } else {
                other.to_string()
            };
            handle_connect(&query);
        }
    }
}

fn print_usage() {
    println!("fortivpn - CLI for FortiVPN Tray");
    println!();
    println!("Usage:");
    println!("  fortivpn status               Show VPN connection status");
    println!("  fortivpn list                  List available profiles");
    println!("  fortivpn connect <name>        Connect to a VPN profile");
    println!("  fortivpn disconnect            Disconnect VPN");
    println!("  fortivpn set-password <id> <pw>  Save password to keychain");
    println!("  fortivpn <name>                Shortcut for connect");
    println!();
    println!("Aliases: s=status, l/ls=list, c=connect, d/dc=disconnect");
    println!();
    println!("The tray app must be running for this CLI to work.");
}
