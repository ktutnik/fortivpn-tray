//! Privileged helper for fortivpn-tray.
//!
//! This binary runs as root (launched via `osascript` with administrator privileges).
//! It creates a utun device and passes the file descriptor back to the unprivileged
//! parent process via a Unix socket using SCM_RIGHTS, then executes route/DNS commands.
//!
//! Protocol (over Unix socket, newline-delimited JSON):
//!   Parent -> Helper: {"cmd":"create_tun","ip":"10.0.0.1","peer_ip":"10.0.0.2","mtu":1354}
//!   Helper -> Parent: {"ok":true,"tun_name":"utun5"}  (+ sends fd via SCM_RIGHTS)
//!   Parent -> Helper: {"cmd":"add_route","dest":"10.0.0.0/24","gateway":"10.0.0.1"}
//!   Helper -> Parent: {"ok":true}
//!   Parent -> Helper: {"cmd":"configure_dns","servers":["8.8.8.8"],"search_domain":"corp.com"}
//!   Helper -> Parent: {"ok":true}
//!   Parent -> Helper: {"cmd":"cleanup","routes":[...],"gateway_ip":"1.2.3.4","orig_gateway":"192.168.1.1"}
//!   (Helper cleans up routes/DNS and exits)

use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::Command;
use tun2::AbstractDevice;

mod launchd {
    use std::os::unix::io::RawFd;

    extern "C" {
        fn launch_activate_socket(
            name: *const libc::c_char,
            fds: *mut *mut libc::c_int,
            cnt: *mut libc::size_t,
        ) -> libc::c_int;
    }

    pub fn activate_socket(name: &str) -> Result<Vec<RawFd>, String> {
        use std::ffi::CString;
        let c_name = CString::new(name).map_err(|e| format!("CString: {e}"))?;
        let mut fds: *mut libc::c_int = std::ptr::null_mut();
        let mut cnt: libc::size_t = 0;
        let ret = unsafe { launch_activate_socket(c_name.as_ptr(), &mut fds, &mut cnt) };
        if ret != 0 {
            return Err(format!("launch_activate_socket failed: errno {ret}"));
        }
        let fd_slice = unsafe { std::slice::from_raw_parts(fds, cnt) };
        let result: Vec<RawFd> = fd_slice.to_vec();
        unsafe { libc::free(fds as *mut libc::c_void) };
        Ok(result)
    }
}

const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
const IDLE_TIMEOUT_SECS: u64 = 30;

fn main() {
    match launchd::activate_socket("Listeners") {
        Ok(fds) if !fds.is_empty() => {
            use std::os::unix::io::FromRawFd;
            let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fds[0]) };
            run_accept_loop(listener);
        }
        _ => {
            let args: Vec<String> = std::env::args().collect();
            if args.len() != 2 {
                eprintln!("Usage: fortivpn-helper <socket-path>");
                std::process::exit(1);
            }
            let stream = match UnixStream::connect(&args[1]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to connect to {}: {e}", args[1]);
                    std::process::exit(1);
                }
            };
            handle_client(stream);
        }
    }
}

fn run_accept_loop(listener: std::os::unix::net::UnixListener) {
    listener.set_nonblocking(true).expect("set nonblocking");
    let mut last_activity = std::time::Instant::now();

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if !validate_peer(&stream) {
                    eprintln!("Rejected connection from unauthorized peer");
                    continue;
                }
                handle_client(stream);
                last_activity = std::time::Instant::now();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if last_activity.elapsed() > std::time::Duration::from_secs(IDLE_TIMEOUT_SECS) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("Accept error: {e}");
                break;
            }
        }
    }
}

fn validate_peer(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let ret = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    if ret != 0 {
        return false;
    }
    gid == 20 || uid == 0
}

fn handle_client(stream: UnixStream) {
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = send_error(&mut writer, &format!("Invalid JSON: {e}"));
                continue;
            }
        };

        let cmd = msg["cmd"].as_str().unwrap_or("");
        match cmd {
            "ping" => {
                let _ = send_ok(&mut writer, None);
            }
            "version" => {
                let _ = send_ok(
                    &mut writer,
                    Some(serde_json::json!({"version": HELPER_VERSION})),
                );
            }
            "create_tun" => handle_create_tun(&msg, &mut writer),
            "add_route" => handle_add_route(&msg, &mut writer),
            "delete_route" => handle_delete_route(&msg, &mut writer),
            "configure_dns" => handle_configure_dns(&msg, &mut writer),
            "restore_dns" => handle_restore_dns(&mut writer),
            "shutdown" => {
                let _ = send_ok(&mut writer, None);
                break;
            }
            _ => {
                let _ = send_error(&mut writer, &format!("Unknown command: {cmd}"));
            }
        }
    }
}

fn send_ok(writer: &mut UnixStream, extra: Option<serde_json::Value>) -> std::io::Result<()> {
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

fn send_error(writer: &mut UnixStream, msg: &str) -> std::io::Result<()> {
    let resp = serde_json::json!({"ok": false, "error": msg});
    let mut line = serde_json::to_string(&resp)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    writer.flush()
}

fn handle_create_tun(msg: &serde_json::Value, writer: &mut UnixStream) {
    let ip: std::net::Ipv4Addr = match msg["ip"].as_str().and_then(|s| s.parse().ok()) {
        Some(ip) => ip,
        None => {
            let _ = send_error(writer, "Missing or invalid 'ip'");
            return;
        }
    };
    let peer_ip: std::net::Ipv4Addr = match msg["peer_ip"].as_str().and_then(|s| s.parse().ok()) {
        Some(ip) => ip,
        None => {
            let _ = send_error(writer, "Missing or invalid 'peer_ip'");
            return;
        }
    };
    let mtu = msg["mtu"].as_u64().unwrap_or(1354) as u16;

    let mut config = tun2::Configuration::default();
    config.address(ip);
    config.destination(peer_ip);
    config.mtu(mtu);
    config.up();

    #[cfg(target_os = "macos")]
    config.platform_config(|p| {
        p.packet_information(true);
    });

    let dev = match tun2::create(&config) {
        Ok(d) => d,
        Err(e) => {
            let _ = send_error(writer, &format!("Failed to create tun: {e}"));
            return;
        }
    };

    let tun_name = dev.tun_name().unwrap_or_default();
    let tun_fd = dev.as_raw_fd();

    // Send the fd via SCM_RIGHTS
    if let Err(e) = send_fd(writer.as_raw_fd(), tun_fd) {
        let _ = send_error(writer, &format!("Failed to send tun fd: {e}"));
        return;
    }

    // Keep the device alive by leaking it (the fd is now owned by the parent)
    std::mem::forget(dev);

    let _ = send_ok(writer, Some(serde_json::json!({"tun_name": tun_name})));
}

fn handle_add_route(msg: &serde_json::Value, writer: &mut UnixStream) {
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

fn handle_delete_route(msg: &serde_json::Value, writer: &mut UnixStream) {
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

fn handle_configure_dns(msg: &serde_json::Value, writer: &mut UnixStream) {
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

fn handle_restore_dns(writer: &mut UnixStream) {
    let script = "remove State:/Network/Service/fortivpn/DNS\n";
    let _ = run_scutil(script);
    let _ = send_ok(writer, None);
}

fn run_route(action: &str, dest: &str, gateway: &str) -> Result<(), String> {
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

fn run_scutil(script: &str) -> Result<(), String> {
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

/// Send a file descriptor over a Unix socket using SCM_RIGHTS.
fn send_fd(socket_fd: RawFd, fd_to_send: RawFd) -> Result<(), String> {
    use libc::{
        c_void, cmsghdr, iovec, msghdr, sendmsg, CMSG_DATA, CMSG_FIRSTHDR, CMSG_LEN, CMSG_SPACE,
        SCM_RIGHTS, SOL_SOCKET,
    };
    use std::mem;
    use std::ptr;

    unsafe {
        let mut buf = [0u8; 1]; // dummy data byte
        let mut iov = iovec {
            iov_base: buf.as_mut_ptr() as *mut c_void,
            iov_len: 1,
        };

        // Control message buffer
        let cmsg_space = CMSG_SPACE(mem::size_of::<RawFd>() as u32) as usize;
        let mut cmsg_buf = vec![0u8; cmsg_space];

        let mut msg: msghdr = mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut c_void;
        msg.msg_controllen = cmsg_space as _;

        let cmsg: *mut cmsghdr = CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = SOL_SOCKET;
        (*cmsg).cmsg_type = SCM_RIGHTS;
        (*cmsg).cmsg_len = CMSG_LEN(mem::size_of::<RawFd>() as u32) as _;

        ptr::copy_nonoverlapping(
            &fd_to_send as *const RawFd as *const u8,
            CMSG_DATA(cmsg),
            mem::size_of::<RawFd>(),
        );

        let ret = sendmsg(socket_fd, &msg, 0);
        if ret < 0 {
            return Err(format!("sendmsg: {}", std::io::Error::last_os_error()));
        }
    }

    Ok(())
}
