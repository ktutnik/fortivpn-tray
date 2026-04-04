use std::net::Ipv4Addr;

use crate::helper::HelperClient;
use crate::platform;
use crate::{FortiError, VpnConfig};

/// Manages routes installed for the VPN connection.
pub struct RouteManager {
    gateway_ip: Ipv4Addr,
    original_gateway: Option<Ipv4Addr>,
    installed_routes: Vec<(Ipv4Addr, Ipv4Addr)>,
    full_tunnel: bool,
    tun_name: String,
    ipv6_disabled_interfaces: Vec<String>,
    skip_restore_on_drop: bool,
}

impl RouteManager {
    pub fn new(gateway_ip: Ipv4Addr, tun_name: &str) -> Self {
        Self {
            gateway_ip,
            original_gateway: None,
            installed_routes: Vec::new(),
            full_tunnel: false,
            tun_name: tun_name.to_string(),
            ipv6_disabled_interfaces: Vec::new(),
            skip_restore_on_drop: false,
        }
    }

    pub fn configure(&mut self, config: &VpnConfig) -> Result<(), FortiError> {
        self.original_gateway = platform::get_default_gateway();

        if let Some(orig_gw) = self.original_gateway {
            platform::run_route("add", &self.gateway_ip.to_string(), &orig_gw.to_string())?;
        }

        if !config.routes.is_empty() {
            // Split tunnel: only route specified networks through VPN
            for (network, netmask) in &config.routes {
                let cidr = netmask_to_cidr(netmask);
                let dest = format!("{network}/{cidr}");
                platform::run_route("add", &dest, &config.assigned_ip.to_string())?;
                self.installed_routes.push((*network, *netmask));
            }
        } else {
            // Full tunnel: route all traffic through VPN using 0/1 + 128/1
            // This covers all IPv4 without replacing the actual default route entry
            platform::run_route("add", "0.0.0.0/1", &config.assigned_ip.to_string())?;
            platform::run_route("add", "128.0.0.0/1", &config.assigned_ip.to_string())?;
            self.full_tunnel = true;
        }

        platform::configure_dns(&self.tun_name, config)?;

        Ok(())
    }

    pub fn restore(&mut self) {
        platform::restore_ipv6(&self.ipv6_disabled_interfaces);
        self.ipv6_disabled_interfaces.clear();

        if self.full_tunnel {
            let _ = platform::run_route("delete", "0.0.0.0/1", "");
            let _ = platform::run_route("delete", "128.0.0.0/1", "");
            self.full_tunnel = false;
        }

        for (network, netmask) in &self.installed_routes {
            let cidr = netmask_to_cidr(netmask);
            let dest = format!("{network}/{cidr}");
            let _ = platform::run_route("delete", &dest, "");
        }
        self.installed_routes.clear();

        if let Some(orig_gw) = self.original_gateway {
            let _ =
                platform::run_route("delete", &self.gateway_ip.to_string(), &orig_gw.to_string());
        }

        platform::restore_dns(&self.tun_name);
    }

    /// Configure routes via the privileged helper (no root needed in main process).
    pub fn configure_via_helper(
        &mut self,
        config: &VpnConfig,
        helper: &mut HelperClient,
    ) -> Result<(), FortiError> {
        self.original_gateway = platform::get_default_gateway();

        if let Some(orig_gw) = self.original_gateway {
            helper.add_route(&self.gateway_ip.to_string(), &orig_gw.to_string())?;
        }

        if !config.routes.is_empty() {
            for (network, netmask) in &config.routes {
                let cidr = netmask_to_cidr(netmask);
                let dest = format!("{network}/{cidr}");
                helper.add_route(&dest, &config.assigned_ip.to_string())?;
                self.installed_routes.push((*network, *netmask));
            }
        } else {
            helper.add_route("0.0.0.0/1", &config.assigned_ip.to_string())?;
            helper.add_route("128.0.0.0/1", &config.assigned_ip.to_string())?;
            self.full_tunnel = true;
        }

        helper.configure_dns(config)?;

        // Disable IPv6 on active interfaces to prevent leaks (no root needed)
        self.ipv6_disabled_interfaces = platform::disable_ipv6();

        Ok(())
    }

    /// Mark this route manager to skip restore on drop.
    /// Used when the session is dropped without helper access (e.g., panic, event monitor).
    pub fn skip_drop_restore(&mut self) {
        self.skip_restore_on_drop = true;
    }

    /// Restore routes via the privileged helper.
    pub fn restore_via_helper(&mut self, helper: Option<&mut HelperClient>) {
        // Restore IPv6 first (no root needed)
        platform::restore_ipv6(&self.ipv6_disabled_interfaces);
        self.ipv6_disabled_interfaces.clear();

        let Some(helper) = helper else {
            self.restore();
            return;
        };

        if self.full_tunnel {
            let _ = helper.delete_route("0.0.0.0/1");
            let _ = helper.delete_route("128.0.0.0/1");
            self.full_tunnel = false;
        }

        for (network, netmask) in &self.installed_routes {
            let cidr = netmask_to_cidr(netmask);
            let dest = format!("{network}/{cidr}");
            let _ = helper.delete_route(&dest);
        }
        self.installed_routes.clear();

        if let Some(orig_gw) = self.original_gateway {
            let _ = helper.delete_route(&self.gateway_ip.to_string());
            let _ = orig_gw;
        }

        let _ = helper.restore_dns();
    }
}

impl Drop for RouteManager {
    fn drop(&mut self) {
        if !self.skip_restore_on_drop {
            self.restore();
        }
    }
}

fn netmask_to_cidr(mask: &Ipv4Addr) -> u32 {
    u32::from_be_bytes(mask.octets()).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_netmask_to_cidr_class_a() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 0, 0, 0)), 8);
    }

    #[test]
    fn test_netmask_to_cidr_class_b() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 255, 0, 0)), 16);
    }

    #[test]
    fn test_netmask_to_cidr_class_c() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 255, 255, 0)), 24);
    }

    #[test]
    fn test_netmask_to_cidr_full() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 255, 255, 255)), 32);
    }

    #[test]
    fn test_netmask_to_cidr_zero() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(0, 0, 0, 0)), 0);
    }

    #[test]
    fn test_netmask_to_cidr_slash12() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 240, 0, 0)), 12);
    }

    #[test]
    fn test_netmask_to_cidr_slash20() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 255, 240, 0)), 20);
    }

    #[test]
    fn test_netmask_to_cidr_slash1() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(128, 0, 0, 0)), 1);
    }

    #[test]
    fn test_route_manager_new() {
        let rm = RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun5");
        assert_eq!(rm.gateway_ip, Ipv4Addr::new(1, 2, 3, 4));
        assert_eq!(rm.tun_name, "utun5");
        assert!(rm.original_gateway.is_none());
        assert!(rm.installed_routes.is_empty());
        assert!(!rm.full_tunnel);
    }

    // parse_gateway_output tests (macOS-only helper)
    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_typical() {
        let output = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n";
        assert_eq!(
            platform::parse_gateway_output(output),
            Some(Ipv4Addr::new(192, 168, 1, 1))
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_no_gateway() {
        let output = "   route to: default\ndestination: default\n  interface: en0\n";
        assert_eq!(platform::parse_gateway_output(output), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_empty() {
        assert_eq!(platform::parse_gateway_output(""), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_invalid_ip() {
        let output = "    gateway: not-an-ip\n";
        assert_eq!(platform::parse_gateway_output(output), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_extra_whitespace() {
        let output = "    gateway:   10.0.0.1  \n";
        assert_eq!(
            platform::parse_gateway_output(output),
            Some(Ipv4Addr::new(10, 0, 0, 1))
        );
    }

    // build_dns_script tests (macOS-only helper)
    #[test]
    #[cfg(target_os = "macos")]
    fn test_build_dns_script_single_server() {
        let servers = vec![Ipv4Addr::new(8, 8, 8, 8)];
        let script = platform::build_dns_script(&servers, &None);
        assert!(script.contains("d.init"));
        assert!(script.contains("d.add ServerAddresses * 8.8.8.8"));
        assert!(script.contains("set State:/Network/Service/fortivpn/DNS"));
        assert!(!script.contains("SearchDomains"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_build_dns_script_multiple_servers() {
        let servers = vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(8, 8, 4, 4)];
        let script = platform::build_dns_script(&servers, &None);
        assert!(script.contains("d.add ServerAddresses * 8.8.8.8 8.8.4.4"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_build_dns_script_with_search_domain() {
        let servers = vec![Ipv4Addr::new(10, 0, 0, 1)];
        let domain = Some("corp.example.com".to_string());
        let script = platform::build_dns_script(&servers, &domain);
        assert!(script.contains("d.add SearchDomains * corp.example.com"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_build_dns_script_no_search_domain() {
        let servers = vec![Ipv4Addr::new(10, 0, 0, 1)];
        let script = platform::build_dns_script(&servers, &None);
        assert!(!script.contains("SearchDomains"));
    }

    // RouteManager restore tests (no actual routes, just state tracking)
    #[test]
    fn test_route_manager_restore_clears_state() {
        let mut rm = RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun5");
        rm.installed_routes
            .push((Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0)));
        rm.full_tunnel = false;
        // restore will try to run_route which will fail, but state should still be cleared
        rm.restore();
        assert!(rm.installed_routes.is_empty());
        assert!(!rm.full_tunnel);
    }

    #[test]
    fn test_route_manager_restore_full_tunnel_clears_flag() {
        let mut rm = RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun5");
        rm.full_tunnel = true;
        rm.restore();
        assert!(!rm.full_tunnel);
    }

    #[test]
    fn test_route_manager_drop_calls_restore() {
        let mut rm = RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun5");
        rm.full_tunnel = true;
        rm.installed_routes
            .push((Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0)));
        drop(rm);
        // If Drop panicked, this test would fail
    }

    #[test]
    fn test_route_manager_restore_with_original_gateway() {
        let mut rm = RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun5");
        rm.original_gateway = Some(Ipv4Addr::new(192, 168, 1, 1));
        rm.restore();
        // Should not panic even though route commands fail
    }

    #[test]
    fn test_route_manager_restore_full_tunnel_with_routes() {
        let mut rm = RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun5");
        rm.full_tunnel = true;
        rm.original_gateway = Some(Ipv4Addr::new(192, 168, 1, 1));
        rm.installed_routes
            .push((Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0)));
        rm.installed_routes
            .push((Ipv4Addr::new(172, 16, 0, 0), Ipv4Addr::new(255, 240, 0, 0)));
        rm.restore();
        assert!(rm.installed_routes.is_empty());
        assert!(!rm.full_tunnel);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_link_address_line() {
        // Some systems output "gateway:" followed by a link address
        let output = "   route to: default\n    gateway: link#5\n  interface: en0\n";
        // link#5 is not a valid IP
        assert_eq!(platform::parse_gateway_output(output), None);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_parse_gateway_output_multiple_gateways() {
        // Should return the first gateway found
        let output = "    gateway: 10.0.0.1\n    gateway: 10.0.0.2\n";
        assert_eq!(
            platform::parse_gateway_output(output),
            Some(Ipv4Addr::new(10, 0, 0, 1))
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_build_dns_script_empty_servers() {
        let servers: Vec<Ipv4Addr> = vec![];
        let script = platform::build_dns_script(&servers, &None);
        assert!(script.contains("d.init"));
        assert!(script.contains("d.add ServerAddresses *"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_build_dns_script_with_many_servers() {
        let servers = vec![
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(8, 8, 4, 4),
            Ipv4Addr::new(1, 1, 1, 1),
        ];
        let domain = Some("corp.example.com".to_string());
        let script = platform::build_dns_script(&servers, &domain);
        assert!(script.contains("8.8.8.8 8.8.4.4 1.1.1.1"));
        assert!(script.contains("SearchDomains * corp.example.com"));
        assert!(script.ends_with("set State:/Network/Service/fortivpn/DNS\n"));
    }

    #[test]
    fn test_netmask_to_cidr_slash25() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 255, 255, 128)), 25);
    }

    #[test]
    fn test_netmask_to_cidr_slash30() {
        assert_eq!(netmask_to_cidr(&Ipv4Addr::new(255, 255, 255, 252)), 30);
    }
}
