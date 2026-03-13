pub mod async_tun;
pub mod auth;
pub mod bridge;
pub mod helper;
pub mod ppp;
pub mod routing;
pub mod tun;
pub mod tunnel;

use std::net::{Ipv4Addr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// VPN configuration received from the FortiGate gateway XML response.
#[derive(Debug, Clone)]
pub struct VpnConfig {
    pub assigned_ip: Ipv4Addr,
    pub peer_ip: Ipv4Addr,
    pub dns_servers: Vec<Ipv4Addr>,
    pub search_domain: Option<String>,
    pub routes: Vec<(Ipv4Addr, Ipv4Addr)>, // (network, netmask)
}

/// Errors that can occur during FortiVPN operations.
#[derive(Debug)]
pub enum FortiError {
    GatewayUnreachable(String),
    CertificateNotTrusted(String),
    InvalidCredentials,
    OtpRequired,
    AllocationFailed(String),
    TunnelRejected(String),
    PppNegotiationFailed(String),
    TunDeviceError(String),
    RoutingError(String),
    Disconnected(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FortiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GatewayUnreachable(e) => write!(f, "Gateway unreachable: {e}"),
            Self::CertificateNotTrusted(e) => write!(f, "Certificate not trusted: {e}"),
            Self::InvalidCredentials => write!(f, "Invalid username or password"),
            Self::OtpRequired => write!(f, "OTP/two-factor authentication required"),
            Self::AllocationFailed(e) => write!(f, "VPN allocation failed: {e}"),
            Self::TunnelRejected(e) => write!(f, "Tunnel rejected: {e}"),
            Self::PppNegotiationFailed(e) => write!(f, "PPP negotiation failed: {e}"),
            Self::TunDeviceError(e) => write!(f, "Tun device error: {e}"),
            Self::RoutingError(e) => write!(f, "Routing error: {e}"),
            Self::Disconnected(e) => write!(f, "Disconnected: {e}"),
            Self::Io(e) => write!(f, "IO error: {e}"),
        }
    }
}

impl From<std::io::Error> for FortiError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

pub use auth::authenticate;

/// An active VPN session. Holds the tunnel, tun device, and routing state.
pub struct VpnSession {
    shutdown: Arc<Notify>,
    route_manager: Option<routing::RouteManager>,
    bridge_tasks: Vec<JoinHandle<()>>,
    alive: Arc<AtomicBool>,
    host: String,
    port: u16,
    cookie: String,
    trusted_cert: String,
}

impl VpnSession {
    /// Establish a full VPN connection: auth → tunnel → PPP → tun → routes.
    pub async fn connect(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        trusted_cert: &str,
        helper_client: &mut helper::HelperClient,
    ) -> Result<Self, FortiError> {
        // Phase 1: Authenticate (sync TLS in blocking task)
        let (cookie, config) = tokio::task::spawn_blocking({
            let host = host.to_string();
            let username = username.to_string();
            let password = password.to_string();
            let trusted_cert = trusted_cert.to_string();
            move || auth::authenticate(&host, port, &username, &password, &trusted_cert)
        })
        .await
        .map_err(|e| FortiError::Io(std::io::Error::other(e)))??;

        // Phase 2: Open async TLS tunnel
        let mut tls_stream = bridge::async_tls_connect(host, port, trusted_cert).await?;
        bridge::open_tunnel(&mut tls_stream, host, port, &cookie).await?;

        // Phase 3: PPP negotiation
        let (mut tls_reader, mut tls_writer) = tokio::io::split(tls_stream);
        let (assigned_ip, magic_number, _ppp_dns) =
            bridge::negotiate_ppp(&mut tls_reader, &mut tls_writer).await?;

        // Reassemble TLS stream from halves
        let tls_stream = tls_reader.unsplit(tls_writer);

        // Use PPP-negotiated IP if different from XML config
        let final_ip = if !assigned_ip.is_unspecified() {
            assigned_ip
        } else {
            config.assigned_ip
        };

        // Phase 4: Create tun device via privileged helper
        let (tun_fd, tun_name) = helper_client.create_tun(final_ip, config.peer_ip, 1354)?;
        let tun_dev = async_tun::AsyncTunFd::new(tun_fd)
            .map_err(|e| FortiError::TunDeviceError(format!("Async tun: {e}")))?;

        // Phase 5: Start bridge (tun ↔ tunnel)
        let shutdown = Arc::new(Notify::new());
        let bridge_handle =
            bridge::start_bridge(tls_stream, tun_dev, shutdown.clone(), magic_number);

        // Phase 6: Configure routes via helper
        let gateway_ip = format!("{host}:{port}")
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .map(|a| match a.ip() {
                std::net::IpAddr::V4(ip) => ip,
                _ => Ipv4Addr::UNSPECIFIED,
            })
            .unwrap_or(Ipv4Addr::UNSPECIFIED);
        let mut route_manager = routing::RouteManager::new(gateway_ip, &tun_name);
        route_manager.configure_via_helper(&config, helper_client)?;

        Ok(Self {
            shutdown,
            route_manager: Some(route_manager),
            bridge_tasks: bridge_handle.tasks,
            alive: bridge_handle.alive,
            host: host.to_string(),
            port,
            cookie,
            trusted_cert: trusted_cert.to_string(),
        })
    }

    /// Check if the session is still alive (LCP echo health).
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && self.bridge_tasks.iter().all(|t| !t.is_finished())
    }

    /// Disconnect: stop tunnel, restore routes, send logout.
    pub async fn disconnect(&mut self, helper: Option<&mut helper::HelperClient>) {
        // Signal bridge tasks to stop
        self.shutdown.notify_waiters();

        // Wait for tasks to finish
        for task in self.bridge_tasks.drain(..) {
            let _ = tokio::time::timeout(tokio::time::Duration::from_secs(3), task).await;
        }

        // Restore routes via helper
        if let Some(ref mut rm) = self.route_manager {
            rm.restore_via_helper(helper);
        }
        self.route_manager = None;

        // Send logout request (best-effort)
        let host = self.host.clone();
        let port = self.port;
        let cookie = self.cookie.clone();
        let trusted_cert = self.trusted_cert.clone();
        let _ =
            tokio::task::spawn_blocking(move || send_logout(&host, port, &cookie, &trusted_cert))
                .await;
    }
}

impl Drop for VpnSession {
    fn drop(&mut self) {
        self.shutdown.notify_waiters();
        if let Some(ref mut rm) = self.route_manager {
            rm.restore();
        }
    }
}

fn build_logout_request(host: &str, port: u16, cookie: &str) -> String {
    format!(
        "GET /remote/logout HTTP/1.1\r\nHost: {host}:{port}\r\nCookie: SVPNCOOKIE={cookie}\r\n\r\n"
    )
}

/// Send logout request to the gateway (clean session termination).
fn send_logout(host: &str, port: u16, cookie: &str, trusted_cert: &str) {
    if let Ok(mut tls) = auth::tls_connect(host, port, trusted_cert) {
        use std::io::Write;
        let req = build_logout_request(host, port, cookie);
        let _ = tls.write_all(req.as_bytes());
        let _ = tls.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forti_error_display_gateway_unreachable() {
        let err = FortiError::GatewayUnreachable("timeout".to_string());
        assert_eq!(err.to_string(), "Gateway unreachable: timeout");
    }

    #[test]
    fn test_forti_error_display_certificate_not_trusted() {
        let err = FortiError::CertificateNotTrusted("mismatch".to_string());
        assert_eq!(err.to_string(), "Certificate not trusted: mismatch");
    }

    #[test]
    fn test_forti_error_display_invalid_credentials() {
        let err = FortiError::InvalidCredentials;
        assert_eq!(err.to_string(), "Invalid username or password");
    }

    #[test]
    fn test_forti_error_display_otp_required() {
        let err = FortiError::OtpRequired;
        assert_eq!(err.to_string(), "OTP/two-factor authentication required");
    }

    #[test]
    fn test_forti_error_display_allocation_failed() {
        let err = FortiError::AllocationFailed("no resources".to_string());
        assert_eq!(err.to_string(), "VPN allocation failed: no resources");
    }

    #[test]
    fn test_forti_error_display_tunnel_rejected() {
        let err = FortiError::TunnelRejected("403".to_string());
        assert_eq!(err.to_string(), "Tunnel rejected: 403");
    }

    #[test]
    fn test_forti_error_display_ppp_negotiation_failed() {
        let err = FortiError::PppNegotiationFailed("timeout".to_string());
        assert_eq!(err.to_string(), "PPP negotiation failed: timeout");
    }

    #[test]
    fn test_forti_error_display_tun_device_error() {
        let err = FortiError::TunDeviceError("permission denied".to_string());
        assert_eq!(err.to_string(), "Tun device error: permission denied");
    }

    #[test]
    fn test_forti_error_display_routing_error() {
        let err = FortiError::RoutingError("route add failed".to_string());
        assert_eq!(err.to_string(), "Routing error: route add failed");
    }

    #[test]
    fn test_forti_error_display_disconnected() {
        let err = FortiError::Disconnected("peer closed".to_string());
        assert_eq!(err.to_string(), "Disconnected: peer closed");
    }

    #[test]
    fn test_forti_error_display_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "connection reset");
        let err = FortiError::Io(io_err);
        assert!(err.to_string().contains("IO error:"));
        assert!(err.to_string().contains("connection reset"));
    }

    #[test]
    fn test_forti_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let err: FortiError = io_err.into();
        match err {
            FortiError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
            _ => panic!("Expected FortiError::Io"),
        }
    }

    #[test]
    fn test_vpn_config_structure() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![Ipv4Addr::new(8, 8, 8, 8)],
            search_domain: Some("example.com".to_string()),
            routes: vec![(Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0))],
        };
        assert_eq!(config.assigned_ip, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(config.peer_ip, Ipv4Addr::new(169, 254, 2, 1));
        assert_eq!(config.dns_servers.len(), 1);
        assert_eq!(config.routes.len(), 1);
    }

    // build_logout_request tests
    #[test]
    fn test_build_logout_request_basic() {
        let req = build_logout_request("vpn.example.com", 443, "DEADBEEF");
        assert!(req.starts_with("GET /remote/logout HTTP/1.1\r\n"));
        assert!(req.contains("Host: vpn.example.com:443"));
        assert!(req.contains("Cookie: SVPNCOOKIE=DEADBEEF"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_build_logout_request_custom_port() {
        let req = build_logout_request("gw.corp.com", 10443, "abc123");
        assert!(req.contains("Host: gw.corp.com:10443"));
        assert!(req.contains("SVPNCOOKIE=abc123"));
    }

    #[test]
    fn test_build_logout_request_empty_cookie() {
        let req = build_logout_request("host", 443, "");
        assert!(req.contains("SVPNCOOKIE=\r\n"));
    }

    // VpnConfig edge cases
    #[test]
    fn test_vpn_config_empty_routes() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![],
            search_domain: None,
            routes: vec![],
        };
        assert!(config.routes.is_empty());
        assert!(config.dns_servers.is_empty());
        assert!(config.search_domain.is_none());
    }

    #[test]
    fn test_vpn_config_multiple_routes() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(8, 8, 4, 4)],
            search_domain: Some("corp.com".to_string()),
            routes: vec![
                (Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0)),
                (Ipv4Addr::new(172, 16, 0, 0), Ipv4Addr::new(255, 240, 0, 0)),
            ],
        };
        assert_eq!(config.routes.len(), 2);
        assert_eq!(config.dns_servers.len(), 2);
    }

    #[test]
    fn test_vpn_config_clone() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![Ipv4Addr::new(8, 8, 8, 8)],
            search_domain: Some("test.com".to_string()),
            routes: vec![(Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0))],
        };
        let cloned = config.clone();
        assert_eq!(cloned.assigned_ip, config.assigned_ip);
        assert_eq!(cloned.dns_servers, config.dns_servers);
        assert_eq!(cloned.search_domain, config.search_domain);
    }

    #[test]
    fn test_vpn_config_debug() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![],
            search_domain: None,
            routes: vec![],
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("VpnConfig"));
        assert!(debug.contains("10.0.0.1"));
    }

    // FortiError additional edge cases
    #[test]
    fn test_forti_error_io_preserves_kind() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no access");
        let forti_err = FortiError::from(io_err);
        match forti_err {
            FortiError::Io(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(e.to_string().contains("no access"));
            }
            _ => panic!("Expected FortiError::Io"),
        }
    }

    #[test]
    fn test_forti_error_display_all_string_variants() {
        // Verify all string-carrying variants format correctly
        let cases: Vec<(FortiError, &str)> = vec![
            (
                FortiError::GatewayUnreachable("dns failed".into()),
                "Gateway unreachable: dns failed",
            ),
            (
                FortiError::CertificateNotTrusted("expired".into()),
                "Certificate not trusted: expired",
            ),
            (
                FortiError::AllocationFailed("full".into()),
                "VPN allocation failed: full",
            ),
            (
                FortiError::TunnelRejected("denied".into()),
                "Tunnel rejected: denied",
            ),
            (
                FortiError::PppNegotiationFailed("nak".into()),
                "PPP negotiation failed: nak",
            ),
            (
                FortiError::TunDeviceError("busy".into()),
                "Tun device error: busy",
            ),
            (
                FortiError::RoutingError("no route".into()),
                "Routing error: no route",
            ),
            (
                FortiError::Disconnected("reset".into()),
                "Disconnected: reset",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected);
        }
    }

    #[test]
    fn test_build_logout_request_special_chars_in_cookie() {
        let req = build_logout_request("host", 443, "abc+def/ghi=");
        assert!(req.contains("SVPNCOOKIE=abc+def/ghi="));
    }

    #[test]
    fn test_build_logout_request_format_exact() {
        let req = build_logout_request("vpn.test.com", 8443, "COOKIE123");
        let expected = "GET /remote/logout HTTP/1.1\r\nHost: vpn.test.com:8443\r\nCookie: SVPNCOOKIE=COOKIE123\r\n\r\n";
        assert_eq!(req, expected);
    }

    #[test]
    fn test_vpn_config_clone_deep() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(1, 1, 1, 1)],
            search_domain: Some("example.com".to_string()),
            routes: vec![
                (Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0)),
                (Ipv4Addr::new(172, 16, 0, 0), Ipv4Addr::new(255, 240, 0, 0)),
            ],
        };
        let cloned = config.clone();
        assert_eq!(cloned.assigned_ip, Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(cloned.peer_ip, Ipv4Addr::new(169, 254, 2, 1));
        assert_eq!(cloned.dns_servers.len(), 2);
        assert_eq!(cloned.routes.len(), 2);
        assert_eq!(cloned.search_domain, Some("example.com".to_string()));
    }

    #[test]
    fn test_vpn_config_many_dns_servers() {
        let config = VpnConfig {
            assigned_ip: Ipv4Addr::new(10, 0, 0, 1),
            peer_ip: Ipv4Addr::new(169, 254, 2, 1),
            dns_servers: vec![
                Ipv4Addr::new(8, 8, 8, 8),
                Ipv4Addr::new(8, 8, 4, 4),
                Ipv4Addr::new(1, 1, 1, 1),
                Ipv4Addr::new(1, 0, 0, 1),
            ],
            search_domain: None,
            routes: vec![],
        };
        assert_eq!(config.dns_servers.len(), 4);
        assert_eq!(config.dns_servers[2], Ipv4Addr::new(1, 1, 1, 1));
    }
}
