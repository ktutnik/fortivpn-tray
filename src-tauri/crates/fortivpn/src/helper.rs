//! Client for the privileged helper binary.
//!
//! Launches `fortivpn-helper` via `osascript` (macOS admin prompt) and communicates
//! over a Unix socket. The helper creates tun devices, manages routes, and configures DNS
//! as root, while the main app remains unprivileged.

use std::io::{BufRead, BufReader, Write};
use std::net::Ipv4Addr;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use crate::{FortiError, VpnConfig};

/// A connection to the privileged helper process.
pub struct HelperClient {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
    socket_path: PathBuf,
}

impl HelperClient {
    /// Launch the helper via osascript and establish a connection.
    pub fn spawn() -> Result<Self, FortiError> {
        let socket_path = std::env::temp_dir().join(format!("fortivpn-helper-{}.sock", std::process::id()));

        // Clean up any stale socket
        let _ = std::fs::remove_file(&socket_path);

        // Create listener before launching helper
        let listener = UnixListener::bind(&socket_path)
            .map_err(|e| FortiError::TunDeviceError(format!("Create helper socket: {e}")))?;

        // Find the helper binary next to our own executable
        let helper_path = std::env::current_exe()
            .map_err(|e| FortiError::TunDeviceError(format!("Find current exe: {e}")))?
            .parent()
            .ok_or_else(|| FortiError::TunDeviceError("No parent dir".into()))?
            .join("fortivpn-helper");

        if !helper_path.exists() {
            return Err(FortiError::TunDeviceError(format!(
                "Helper binary not found at {}",
                helper_path.display()
            )));
        }

        let helper_str = helper_path.to_string_lossy();
        let socket_str = socket_path.to_string_lossy();

        // Launch via osascript for admin privileges (shows native macOS password dialog)
        let script = format!(
            "do shell script \"'{}' '{}'\" with administrator privileges",
            helper_str.replace('\'', "'\\''"),
            socket_str.replace('\'', "'\\''"),
        );

        std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| FortiError::TunDeviceError(format!("Launch osascript: {e}")))?;

        // Wait for helper to connect (with timeout)
        listener
            .set_nonblocking(false)
            .map_err(|e| FortiError::TunDeviceError(format!("Set blocking: {e}")))?;

        // Set a timeout for accept
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(60); // user has 60s to enter password

        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > timeout {
                        let _ = std::fs::remove_file(&socket_path);
                        return Err(FortiError::TunDeviceError(
                            "Timed out waiting for helper (user cancelled?)".into(),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&socket_path);
                    return Err(FortiError::TunDeviceError(format!("Accept helper: {e}")));
                }
            }
        };

        let writer = stream.try_clone()
            .map_err(|e| FortiError::TunDeviceError(format!("Clone stream: {e}")))?;
        let reader = BufReader::new(stream);

        Ok(Self {
            reader,
            writer,
            socket_path,
        })
    }

    /// Ask the helper to create a tun device. Returns (raw_fd, tun_name).
    pub fn create_tun(
        &mut self,
        ip: Ipv4Addr,
        peer_ip: Ipv4Addr,
        mtu: u16,
    ) -> Result<(RawFd, String), FortiError> {
        let cmd = serde_json::json!({
            "cmd": "create_tun",
            "ip": ip.to_string(),
            "peer_ip": peer_ip.to_string(),
            "mtu": mtu,
        });
        self.send_cmd(&cmd)?;

        // Receive the fd via SCM_RIGHTS
        let fd = recv_fd(self.reader.get_ref())
            .map_err(|e| FortiError::TunDeviceError(format!("Receive tun fd: {e}")))?;

        // Read the JSON response
        let resp = self.read_response()?;
        let tun_name = resp["tun_name"]
            .as_str()
            .unwrap_or("utun?")
            .to_string();

        Ok((fd, tun_name))
    }

    /// Ask the helper to add a route.
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

    /// Ask the helper to delete a route.
    pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
        let cmd = serde_json::json!({
            "cmd": "delete_route",
            "dest": dest,
        });
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    /// Ask the helper to configure DNS.
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

    /// Ask the helper to restore DNS.
    pub fn restore_dns(&mut self) -> Result<(), FortiError> {
        let cmd = serde_json::json!({"cmd": "restore_dns"});
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    /// Check if the helper is still alive.
    pub fn ping(&mut self) -> Result<(), FortiError> {
        let cmd = serde_json::json!({"cmd": "ping"});
        self.send_cmd(&cmd)?;
        self.read_response()?;
        Ok(())
    }

    /// Tell the helper to shut down.
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

impl Drop for HelperClient {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Receive a file descriptor over a Unix socket using SCM_RIGHTS.
fn recv_fd(stream: &std::os::unix::net::UnixStream) -> Result<RawFd, String> {
    use libc::{
        c_void, cmsghdr, iovec, msghdr, recvmsg, CMSG_DATA, CMSG_FIRSTHDR, CMSG_SPACE,
        SCM_RIGHTS, SOL_SOCKET,
    };
    use std::mem;
    use std::os::unix::io::AsRawFd;

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

/// Wrap a raw tun fd into a tokio-compatible async reader/writer.
/// On macOS, utun sockets are kernel control sockets that support read/write.
pub fn async_tun_from_fd(fd: RawFd) -> Result<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>, FortiError> {
    use std::os::fd::OwnedFd;

    // Set non-blocking
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(FortiError::TunDeviceError(format!(
            "fcntl F_GETFL: {}",
            std::io::Error::last_os_error()
        )));
    }
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if ret < 0 {
        return Err(FortiError::TunDeviceError(format!(
            "fcntl F_SETFL: {}",
            std::io::Error::last_os_error()
        )));
    }

    let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
    tokio::io::unix::AsyncFd::new(owned_fd)
        .map_err(|e| FortiError::TunDeviceError(format!("AsyncFd: {e}")))
}
