use crate::commands::{self, send_error, send_ok, HELPER_VERSION};
use std::io::{BufRead, BufReader};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use tun2::AbstractDevice;

const IDLE_TIMEOUT_SECS: u64 = 30;

pub fn run() -> ! {
    // TODO: systemd socket activation instead of launchd
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
    std::process::exit(0);
}

fn handle_client(stream: UnixStream) {
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;
    let mut tun_fds: Vec<RawFd> = Vec::new();

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
            "create_tun" => handle_create_tun(&msg, &mut writer, &mut tun_fds),
            "destroy_tun" => handle_destroy_tun(&mut writer, &mut tun_fds),
            "add_route" => commands::handle_add_route(&msg, &mut writer),
            "delete_route" => commands::handle_delete_route(&msg, &mut writer),
            "configure_dns" => commands::handle_configure_dns(&msg, &mut writer),
            "restore_dns" => commands::handle_restore_dns(&mut writer),
            "shutdown" => {
                let _ = send_ok(&mut writer, None);
                break;
            }
            _ => {
                let _ = send_error(&mut writer, &format!("Unknown command: {cmd}"));
            }
        }
    }

    close_tun_fds(&mut tun_fds);
}

fn close_tun_fds(tun_fds: &mut Vec<RawFd>) {
    for fd in tun_fds.drain(..) {
        unsafe {
            libc::close(fd);
        }
    }
}

fn handle_destroy_tun(writer: &mut UnixStream, tun_fds: &mut Vec<RawFd>) {
    close_tun_fds(tun_fds);
    let _ = send_ok(writer, None);
}

fn handle_create_tun(msg: &serde_json::Value, writer: &mut UnixStream, tun_fds: &mut Vec<RawFd>) {
    close_tun_fds(tun_fds);
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

    let dev = match tun2::create(&config) {
        Ok(d) => d,
        Err(e) => {
            let _ = send_error(writer, &format!("Failed to create tun: {e}"));
            return;
        }
    };

    let tun_name = dev.tun_name().unwrap_or_default();
    let tun_fd = dev.as_raw_fd();

    if let Err(e) = send_fd(writer.as_raw_fd(), tun_fd) {
        let _ = send_error(writer, &format!("Failed to send tun fd: {e}"));
        return;
    }

    let fd = dev.as_raw_fd();
    std::mem::forget(dev);
    tun_fds.push(fd);

    let _ = send_ok(writer, Some(serde_json::json!({"tun_name": tun_name})));
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
        let mut buf = [0u8; 1];
        let mut iov = iovec {
            iov_base: buf.as_mut_ptr() as *mut c_void,
            iov_len: 1,
        };

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
