//! Client for the privileged helper daemon.
//!
//! Connects to the launchd-managed helper daemon via a well-known Unix socket.
//! The helper creates tun devices, manages routes, and configures DNS
//! as root, while the main app remains unprivileged.

use crate::platform;
use crate::{FortiError, VpnConfig};

pub struct HelperClient {
    inner: platform::HelperClientInner,
}

impl HelperClient {
    pub fn connect() -> Result<Self, FortiError> {
        Ok(Self {
            inner: platform::HelperClientInner::connect()?,
        })
    }

    pub fn version(&mut self) -> Result<String, FortiError> {
        self.inner.version()
    }

    pub fn create_tun(
        &mut self,
        ip: std::net::Ipv4Addr,
        peer_ip: std::net::Ipv4Addr,
        mtu: u16,
    ) -> Result<platform::TunHandle, FortiError> {
        self.inner.create_tun(ip, peer_ip, mtu)
    }

    pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
        self.inner.destroy_tun()
    }

    pub fn add_route(&mut self, dest: &str, gateway: &str) -> Result<(), FortiError> {
        self.inner.add_route(dest, gateway)
    }

    pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
        self.inner.delete_route(dest)
    }

    pub fn configure_dns(&mut self, config: &VpnConfig) -> Result<(), FortiError> {
        self.inner.configure_dns(config)
    }

    pub fn restore_dns(&mut self) -> Result<(), FortiError> {
        self.inner.restore_dns()
    }

    pub fn ping(&mut self) -> Result<(), FortiError> {
        self.inner.ping()
    }

    pub fn shutdown(&mut self) {
        self.inner.shutdown()
    }
}
