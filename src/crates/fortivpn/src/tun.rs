use std::net::Ipv4Addr;
use tun2::{AsyncDevice, Configuration};

use crate::FortiError;

/// Create and configure a tun device.
pub fn create_tun(ip: Ipv4Addr, peer_ip: Ipv4Addr, mtu: u16) -> Result<AsyncDevice, FortiError> {
    let mut config = Configuration::default();
    config.address(ip);
    config.destination(peer_ip);
    config.mtu(mtu);
    config.up();

    #[cfg(target_os = "macos")]
    config.platform_config(|p| {
        p.packet_information(true);
    });

    tun2::create_as_async(&config).map_err(|e| FortiError::TunDeviceError(format!("{e}")))
}

/// Get the tun device name.
pub fn device_name(dev: &AsyncDevice) -> String {
    dev.as_ref().tun_name().unwrap_or_default()
}
