use std::io;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tun2::AsyncDevice;

use crate::{silent_cmd, tun, FortiError, VpnConfig};

// ── TunHandle ───────────────────────────────────────────────────────────────

/// Platform-specific handle returned by the helper after creating a TUN device.
/// On Windows this is a tun2 AsyncDevice plus the device name.
pub type TunHandle = (AsyncDevice, String);

// ── HelperClientInner ───────────────────────────────────────────────────────

/// Windows helper client — creates TUN directly (no privilege daemon).
pub struct HelperClientInner {
    /// Name of the active TUN interface (for DNS restore).
    tun_name: Option<String>,
}

impl HelperClientInner {
    pub fn connect() -> Result<Self, FortiError> {
        Ok(Self { tun_name: None })
    }

    pub fn version(&mut self) -> Result<String, FortiError> {
        Ok(env!("CARGO_PKG_VERSION").to_string())
    }

    pub fn create_tun(
        &mut self,
        ip: Ipv4Addr,
        peer_ip: Ipv4Addr,
        mtu: u16,
    ) -> Result<TunHandle, FortiError> {
        let dev = tun::create_tun(ip, peer_ip, mtu)?;
        let name = tun::device_name(&dev);
        self.tun_name = Some(name.clone());
        Ok((dev, name))
    }

    pub fn add_route(&mut self, dest: &str, gateway: &str) -> Result<(), FortiError> {
        let output = silent_cmd("route")
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

    pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
        self.tun_name = None;
        Ok(())
    }

    pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
        let output = silent_cmd("route")
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

    pub fn configure_dns(&mut self, config: &VpnConfig) -> Result<(), FortiError> {
        let iface = self.tun_name.as_deref().unwrap_or("FortiVPN");

        for (i, server) in config.dns_servers.iter().enumerate() {
            let iface_owned = iface.to_string();
            let server_str = server.to_string();
            let index_str = (i + 1).to_string();

            let mut cmd = silent_cmd("netsh");
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

    pub fn restore_dns(&mut self) -> Result<(), FortiError> {
        let iface = self.tun_name.as_deref().unwrap_or("FortiVPN");

        let output = silent_cmd("netsh")
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

    pub fn ping(&mut self) -> Result<(), FortiError> {
        Ok(())
    }

    pub fn shutdown(&mut self) {}
}

// ── AsyncTunFd ──────────────────────────────────────────────────────────────

/// An async tun device backed by a `tun2::AsyncDevice`.
pub struct AsyncTunFd {
    inner: AsyncDevice,
}

impl AsyncTunFd {
    /// Create from a `tun2::AsyncDevice` (takes ownership).
    pub fn from_device(dev: AsyncDevice) -> io::Result<Self> {
        Ok(Self { inner: dev })
    }

    /// Create from a platform-specific TUN handle.
    pub fn from_handle(handle: TunHandle) -> io::Result<Self> {
        Self::from_device(handle.0)
    }
}

impl AsyncRead for AsyncTunFd {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for AsyncTunFd {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ── Routing ─────────────────────────────────────────────────────────────────

pub fn run_route(action: &str, dest: &str, gateway: &str) -> Result<(), FortiError> {
    let action_upper = action.to_uppercase();
    let mut args = vec![action_upper.as_str(), dest];
    if !gateway.is_empty() {
        args.push(gateway);
    }
    let output = silent_cmd("route")
        .args(&args)
        .output()
        .map_err(|e| FortiError::RoutingError(format!("route {action}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(FortiError::RoutingError(format!(
            "route {action} {dest}: {stderr}"
        )));
    }
    Ok(())
}

pub fn get_default_gateway() -> Option<Ipv4Addr> {
    let output = silent_cmd("route")
        .args(["print", "0.0.0.0"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 && parts[0] == "0.0.0.0" && parts[1] == "0.0.0.0" {
            return parts[2].parse().ok();
        }
    }
    None
}

pub fn configure_dns(tun_name: &str, config: &VpnConfig) -> Result<(), FortiError> {
    if config.dns_servers.is_empty() {
        return Ok(());
    }

    for dns in &config.dns_servers {
        let _ = silent_cmd("netsh")
            .args([
                "interface",
                "ip",
                "set",
                "dns",
                tun_name,
                "static",
                &dns.to_string(),
            ])
            .output();
    }
    Ok(())
}

pub fn restore_dns(_tun_name: &str) {
    // DNS cleaned up when adapter is removed
}

pub fn disable_ipv6() -> Vec<String> {
    Vec::new() // IPv6 disable via netsh is optional for now
}

pub fn restore_ipv6(_interfaces: &[String]) {
    // No-op on Windows
}

// ── TUN device name ─────────────────────────────────────────────────────────

pub fn device_name(dev: &tun2::AsyncDevice) -> String {
    use tun2::AbstractDevice;
    dev.tun_name().unwrap_or_default()
}
