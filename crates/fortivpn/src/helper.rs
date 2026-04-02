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

// ── Windows implementation ───────────────────────────────────────────────────
//
// On Windows there is no separate helper daemon. TUN devices are created
// directly via `tun2`, routes via `route.exe`, DNS via `netsh`.

#[cfg(windows)]
mod platform {
    use std::net::Ipv4Addr;
    use std::process::Command;

    use tun2::AsyncDevice;

    use crate::{tun, FortiError, VpnConfig};

    /// Windows helper client — creates TUN directly (no privilege daemon).
    pub struct HelperClient {
        /// Name of the active TUN interface (for DNS restore).
        tun_name: Option<String>,
    }

    impl HelperClient {
        /// No daemon to connect to on Windows; just construct the client.
        pub fn connect() -> Result<Self, FortiError> {
            Ok(Self { tun_name: None })
        }

        pub fn version(&mut self) -> Result<String, FortiError> {
            Ok(env!("CARGO_PKG_VERSION").to_string())
        }

        /// Create a TUN device via `tun2`. Returns `(AsyncDevice, tun_name)`.
        pub fn create_tun(
            &mut self,
            ip: Ipv4Addr,
            peer_ip: Ipv4Addr,
            mtu: u16,
        ) -> Result<(AsyncDevice, String), FortiError> {
            let dev = tun::create_tun(ip, peer_ip, mtu)?;
            let name = tun::device_name(&dev);
            self.tun_name = Some(name.clone());
            Ok((dev, name))
        }

        /// Add a route via `route.exe ADD`.
        pub fn add_route(&mut self, dest: &str, gateway: &str) -> Result<(), FortiError> {
            let output = Command::new("route")
                .args(["ADD", dest, "MASK", "255.255.255.255", gateway])
                .output()
                .map_err(|e| FortiError::RoutingError(format!("route ADD: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(FortiError::RoutingError(format!(
                    "route ADD {dest}: {stderr}"
                )));
            }
            Ok(())
        }

        /// No-op on Windows — tun2 cleans up on drop.
        pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
            self.tun_name = None;
            Ok(())
        }

        /// Delete a route via `route.exe DELETE`.
        pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
            let output = Command::new("route")
                .args(["DELETE", dest])
                .output()
                .map_err(|e| FortiError::RoutingError(format!("route DELETE: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(FortiError::RoutingError(format!(
                    "route DELETE {dest}: {stderr}"
                )));
            }
            Ok(())
        }

        /// Configure DNS servers via `netsh`.
        pub fn configure_dns(&mut self, config: &VpnConfig) -> Result<(), FortiError> {
            let iface = self.tun_name.as_deref().unwrap_or("FortiVPN");

            for (i, server) in config.dns_servers.iter().enumerate() {
                let iface_owned = iface.to_string();
                let server_str = server.to_string();
                let index_str = (i + 1).to_string();

                let mut cmd = Command::new("netsh");
                if i == 0 {
                    cmd.args(["interface", "ip", "set", "dns"])
                        .arg(format!("name={iface_owned}"))
                        .arg("static")
                        .arg(&server_str);
                } else {
                    cmd.args(["interface", "ip", "add", "dns"])
                        .arg(format!("name={iface_owned}"))
                        .arg(&server_str)
                        .arg(format!("index={index_str}"));
                }

                let output = cmd
                    .output()
                    .map_err(|e| FortiError::TunDeviceError(format!("netsh dns: {e}")))?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(FortiError::TunDeviceError(format!(
                        "netsh dns configure: {stderr}"
                    )));
                }
            }
            Ok(())
        }

        /// Restore DNS to DHCP via `netsh`.
        pub fn restore_dns(&mut self) -> Result<(), FortiError> {
            let iface = self.tun_name.as_deref().unwrap_or("FortiVPN");

            let output = Command::new("netsh")
                .args(["interface", "ip", "set", "dns"])
                .arg(format!("name={iface}"))
                .arg("dhcp")
                .output()
                .map_err(|e| FortiError::TunDeviceError(format!("netsh dns restore: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(FortiError::TunDeviceError(format!(
                    "netsh dns restore: {stderr}"
                )));
            }
            Ok(())
        }

        /// No-op ping — no daemon to check on Windows.
        pub fn ping(&mut self) -> Result<(), FortiError> {
            Ok(())
        }

        /// No-op shutdown — no daemon to stop on Windows.
        pub fn shutdown(&mut self) {}
    }
}

pub use platform::HelperClient;
