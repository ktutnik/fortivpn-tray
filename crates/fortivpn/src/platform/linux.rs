use std::io;
use std::io::{BufRead, BufReader, Write};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::pin::Pin;
use std::process::Command;
use std::task::{Context, Poll};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::{FortiError, VpnConfig};

// ── TunHandle ───────────────────────────────────────────────────────────────

/// Platform-specific handle returned by the helper after creating a TUN device.
/// On Linux this is a raw file descriptor plus the device name.
pub type TunHandle = (RawFd, String);

// ── HelperClientInner ───────────────────────────────────────────────────────

const HELPER_SOCKET_PATH: &str = "/var/run/fortivpn-helper.sock";

/// Unix implementation of the privileged helper client.
pub struct HelperClientInner {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}

impl HelperClientInner {
    pub fn connect() -> Result<Self, FortiError> {
        let stream = std::os::unix::net::UnixStream::connect(HELPER_SOCKET_PATH).map_err(|e| {
            FortiError::TunDeviceError(format!(
                "Connect to helper daemon: {e}. Is the helper installed? Run the app to trigger installation."
            ))
        })?;

        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| FortiError::TunDeviceError(format!("Set timeout: {e}")))?;

        let writer = stream
            .try_clone()
            .map_err(|e| FortiError::TunDeviceError(format!("Clone stream: {e}")))?;
        let reader = BufReader::new(stream);

        Ok(Self { reader, writer })
    }

    pub fn version(&mut self) -> Result<String, FortiError> {
        let cmd = serde_json::json!({"cmd": "version"});
        self.send_cmd(&cmd)?;
        let resp = self.read_response()?;
        Ok(resp["version"].as_str().unwrap_or("unknown").to_string())
    }

    pub fn create_tun(
        &mut self,
        ip: Ipv4Addr,
        peer_ip: Ipv4Addr,
        mtu: u16,
    ) -> Result<TunHandle, FortiError> {
        let cmd = serde_json::json!({
            "cmd": "create_tun",
            "ip": ip.to_string(),
            "peer_ip": peer_ip.to_string(),
            "mtu": mtu,
        });
        self.send_cmd(&cmd)?;

        let fd = recv_fd(self.reader.get_ref())
            .map_err(|e| FortiError::TunDeviceError(format!("Receive tun fd: {e}")))?;

        let resp = self.read_response()?;
        let tun_name = resp["tun_name"].as_str().unwrap_or("utun?").to_string();

        Ok((fd, tun_name))
    }

    pub fn add_route(&mut self, dest: &str, gateway: &str) -> Result<(), FortiError> {
        let cmd = serde_json::json!({
            "cmd": "add_route",
            "dest": dest,
            "gateway": gateway,
        });
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
        let cmd = serde_json::json!({"cmd": "destroy_tun"});
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
        let cmd = serde_json::json!({
            "cmd": "delete_route",
            "dest": dest,
        });
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    pub fn configure_dns(&mut self, config: &VpnConfig) -> Result<(), FortiError> {
        let servers: Vec<String> = config.dns_servers.iter().map(|ip| ip.to_string()).collect();
        let cmd = serde_json::json!({
            "cmd": "configure_dns",
            "servers": servers,
            "search_domain": config.search_domain,
        });
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    pub fn restore_dns(&mut self) -> Result<(), FortiError> {
        let cmd = serde_json::json!({"cmd": "restore_dns"});
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    pub fn ping(&mut self) -> Result<(), FortiError> {
        let cmd = serde_json::json!({"cmd": "ping"});
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        let cmd = serde_json::json!({"cmd": "shutdown"});
        let _ = self.send_cmd(&cmd);
    }

    fn send_cmd(&mut self, cmd: &serde_json::Value) -> Result<(), FortiError> {
        let mut line = serde_json::to_string(cmd)
            .map_err(|e| FortiError::TunDeviceError(format!("Serialize cmd: {e}")))?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| FortiError::TunDeviceError(format!("Send to helper: {e}")))?;
        self.writer
            .flush()
            .map_err(|e| FortiError::TunDeviceError(format!("Flush to helper: {e}")))
    }

    fn read_response(&mut self) -> Result<serde_json::Value, FortiError> {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .map_err(|e| FortiError::TunDeviceError(format!("Read from helper: {e}")))?;

        let resp: serde_json::Value = serde_json::from_str(&line)
            .map_err(|e| FortiError::TunDeviceError(format!("Parse helper response: {e}")))?;

        if resp["ok"].as_bool() != Some(true) {
            let error = resp["error"].as_str().unwrap_or("Unknown error");
            return Err(FortiError::TunDeviceError(error.to_string()));
        }

        Ok(resp)
    }
}

/// Receive a file descriptor over a Unix socket using SCM_RIGHTS.
fn recv_fd(stream: &std::os::unix::net::UnixStream) -> Result<RawFd, String> {
    use libc::{
        c_void, cmsghdr, iovec, msghdr, recvmsg, CMSG_DATA, CMSG_FIRSTHDR, CMSG_SPACE, SCM_RIGHTS,
        SOL_SOCKET,
    };
    use std::mem;

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

        let ret = recvmsg(stream.as_raw_fd(), &mut msg, 0);
        if ret < 0 {
            return Err(format!("recvmsg: {}", std::io::Error::last_os_error()));
        }

        let cmsg: *mut cmsghdr = CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err("No control message received".into());
        }
        if (*cmsg).cmsg_level != SOL_SOCKET || (*cmsg).cmsg_type != SCM_RIGHTS {
            return Err("Unexpected control message type".into());
        }

        let mut fd: RawFd = 0;
        std::ptr::copy_nonoverlapping(
            CMSG_DATA(cmsg),
            &mut fd as *mut RawFd as *mut u8,
            mem::size_of::<RawFd>(),
        );

        Ok(fd)
    }
}

// ── AsyncTunFd ──────────────────────────────────────────────────────────────

/// An async tun device backed by a raw file descriptor.
pub struct AsyncTunFd {
    inner: AsyncFd<OwnedFd>,
}

impl AsyncTunFd {
    /// Create from a raw file descriptor (takes ownership).
    pub fn new(fd: RawFd) -> io::Result<Self> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let async_fd = AsyncFd::new(owned)?;
        Ok(Self { inner: async_fd })
    }

    /// Create from a platform-specific TUN handle.
    pub fn from_handle(handle: TunHandle) -> io::Result<Self> {
        Self::new(handle.0)
    }
}

impl AsyncRead for AsyncTunFd {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = match self.inner.poll_read_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            let fd = self.inner.as_raw_fd();
            let unfilled = buf.initialize_unfilled();
            let ret = unsafe {
                libc::read(
                    fd,
                    unfilled.as_mut_ptr() as *mut libc::c_void,
                    unfilled.len(),
                )
            };

            if ret >= 0 {
                buf.advance(ret as usize);
                return Poll::Ready(Ok(()));
            }

            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                guard.clear_ready();
                continue;
            }
            return Poll::Ready(Err(err));
        }
    }
}

impl AsyncWrite for AsyncTunFd {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = match self.inner.poll_write_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            let fd = self.inner.as_raw_fd();
            let ret = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };

            if ret >= 0 {
                return Poll::Ready(Ok(ret as usize));
            }

            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                guard.clear_ready();
                continue;
            }
            return Poll::Ready(Err(err));
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

// ── Routing ─────────────────────────────────────────────────────────────────

pub fn run_route(action: &str, dest: &str, gateway: &str) -> Result<(), FortiError> {
    let ip_action = match action {
        "add" => "add",
        "delete" => "del",
        _ => action,
    };
    let mut args = vec!["route", ip_action, dest];
    if !gateway.is_empty() {
        args.push("via");
        args.push(gateway);
    }
    let output = Command::new("ip")
        .args(&args)
        .output()
        .map_err(|e| FortiError::RoutingError(format!("ip route {ip_action}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !(action == "delete" && stderr.contains("No such process")) {
            return Err(FortiError::RoutingError(format!(
                "ip route {ip_action} {dest}: {stderr}"
            )));
        }
    }
    Ok(())
}

pub fn get_default_gateway() -> Option<Ipv4Addr> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[0] == "default" && parts[1] == "via" {
            return parts[2].parse().ok();
        }
    }
    None
}

pub fn configure_dns(_tun_name: &str, _config: &VpnConfig) -> Result<(), FortiError> {
    // TODO: Linux DNS configuration (e.g., resolvconf or systemd-resolved)
    Ok(())
}

pub fn restore_dns(_tun_name: &str) {
    // No-op on Linux for now
}

pub fn disable_ipv6() -> Vec<String> {
    Vec::new() // TODO
}

pub fn restore_ipv6(_interfaces: &[String]) {
    // No-op on Linux for now
}

// ── TUN device name ─────────────────────────────────────────────────────────

pub fn device_name(dev: &tun2::AsyncDevice) -> String {
    dev.as_ref().tun_name().unwrap_or_default()
}
