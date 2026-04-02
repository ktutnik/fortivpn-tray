//! Client for the privileged helper daemon.
//!
//! Connects to the launchd-managed helper daemon via a well-known Unix socket.
//! The helper creates tun devices, manages routes, and configures DNS
//! as root, while the main app remains unprivileged.

// ── Unix implementation ──────────────────────────────────────────────────────

#[cfg(unix)]
mod platform {
    use std::io::{BufRead, BufReader, Write};
    use std::net::Ipv4Addr;
    use std::os::unix::io::RawFd;

    use crate::{FortiError, VpnConfig};

    const HELPER_SOCKET_PATH: &str = "/var/run/fortivpn-helper.sock";

    /// A connection to the privileged helper daemon.
    pub struct HelperClient {
        reader: BufReader<std::os::unix::net::UnixStream>,
        writer: std::os::unix::net::UnixStream,
    }

    impl HelperClient {
        /// Connect to the launchd-managed helper daemon via well-known socket.
        pub fn connect() -> Result<Self, FortiError> {
            let stream = std::os::unix::net::UnixStream::connect(HELPER_SOCKET_PATH)
                .map_err(|e| FortiError::TunDeviceError(format!(
                    "Connect to helper daemon: {e}. Is the helper installed? Run the app to trigger installation."
                )))?;

            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .map_err(|e| FortiError::TunDeviceError(format!("Set timeout: {e}")))?;

            let writer = stream
                .try_clone()
                .map_err(|e| FortiError::TunDeviceError(format!("Clone stream: {e}")))?;
            let reader = BufReader::new(stream);

            Ok(Self { reader, writer })
        }

        /// Check the helper version.
        pub fn version(&mut self) -> Result<String, FortiError> {
            let cmd = serde_json::json!({"cmd": "version"});
            self.send_cmd(&cmd)?;
            let resp = self.read_response()?;
            Ok(resp["version"].as_str().unwrap_or("unknown").to_string())
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
            let tun_name = resp["tun_name"].as_str().unwrap_or("utun?").to_string();

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

        /// Ask the helper to close the TUN device (prevents stale utun on reconnect).
        pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
            let cmd = serde_json::json!({"cmd": "destroy_tun"});
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
}

// ── Windows stub ─────────────────────────────────────────────────────────────

#[cfg(windows)]
mod platform {
    use std::net::Ipv4Addr;

    use crate::{FortiError, VpnConfig};

    /// Stub helper client for Windows (not yet implemented).
    pub struct HelperClient;

    impl HelperClient {
        pub fn connect() -> Result<Self, FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn version(&mut self) -> Result<String, FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn create_tun(
            &mut self,
            _ip: Ipv4Addr,
            _peer_ip: Ipv4Addr,
            _mtu: u16,
        ) -> Result<(i32, String), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn add_route(&mut self, _dest: &str, _gateway: &str) -> Result<(), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn delete_route(&mut self, _dest: &str) -> Result<(), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn configure_dns(&mut self, _config: &VpnConfig) -> Result<(), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn restore_dns(&mut self) -> Result<(), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn ping(&mut self) -> Result<(), FortiError> {
            Err(FortiError::TunDeviceError(
                "Windows helper not yet implemented".into(),
            ))
        }

        pub fn shutdown(&mut self) {}
    }
}

pub use platform::HelperClient;
