pub mod async_tun;
pub mod auth;
pub mod bridge;
pub mod helper;
pub mod platform;
pub mod ppp;
pub mod routing;
pub mod tun;
pub mod tunnel;

use std::net::{Ipv4Addr, ToSocketAddrs};
use std::process::Command;

/// Create a Command that doesn't show a console window on Windows.
pub fn silent_cmd(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (0x08000000) + DETACHED_PROCESS (0x00000008)
        cmd.creation_flags(0x08000008);
    }
    cmd
}
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

/// Events emitted by the VPN session for status monitoring.
#[derive(Debug, Clone, PartialEq)]
pub enum VpnEvent {
    Alive,
    Died(String),
}

/// Network state kept installed across a reconnect.
///
/// Tearing routes and DNS down on every dropped tunnel is what turns a flap into
/// a machine-wide outage: the default route disappears, every in-flight TCP
/// connection on the host dies, and DNS churns — then it is all reinstalled a
/// few seconds later. Holding this state instead makes a flap a brief stall.
///
/// The TUN descriptor is part of it because it has to be: the kernel deletes
/// every route bound to a tun interface the moment its last descriptor closes,
/// so keeping the routes means keeping the interface open.
pub struct PreservedNetwork {
    tun: platform::TunKeeper,
    /// Gateway this state was built for. Adopting it against a different gateway
    /// would inherit a host route pointing at the old one — and could route the
    /// new tunnel's own traffic into the tunnel.
    host: String,
    port: u16,
    assigned_ip: Ipv4Addr,
    peer_ip: Ipv4Addr,
    route_manager: routing::RouteManager,
}

impl PreservedNetwork {
    /// Whether this state can be adopted by a session to `host:port` that was
    /// just assigned `assigned_ip`/`peer_ip`.
    ///
    /// The installed routes are tied to all four: they point at that local IP and
    /// carry a host route to that gateway. Anything else has to be reinstalled
    /// from scratch.
    fn matches(&self, host: &str, port: u16, assigned_ip: Ipv4Addr, peer_ip: Ipv4Addr) -> bool {
        self.host == host
            && self.port == port
            && self.assigned_ip == assigned_ip
            && self.peer_ip == peer_ip
    }

    /// Tear the preserved state down — restore routes and DNS, close the TUN.
    ///
    /// Must be called once the reconnect is abandoned, otherwise the host is
    /// left with its default route pointing into a tunnel that no longer exists.
    pub fn release(mut self, mut helper: Option<&mut helper::HelperClient>) {
        self.route_manager.restore_via_helper(helper.as_deref_mut());
        if let Some(h) = helper {
            let _ = h.destroy_tun();
        }
    }
}

/// An active VPN session. Holds the tunnel, tun device, and routing state.
pub struct VpnSession {
    shutdown: Arc<Notify>,
    route_manager: Option<routing::RouteManager>,
    bridge_tasks: Vec<JoinHandle<()>>,
    alive: Arc<AtomicBool>,
    event_rx: Option<tokio::sync::watch::Receiver<VpnEvent>>,
    host: String,
    port: u16,
    cookie: String,
    trusted_cert: String,
    /// Spare TUN descriptor, so the interface can outlive this session's bridge.
    /// `None` on platforms that cannot duplicate the device.
    tun_keeper: Option<platform::TunKeeper>,
    assigned_ip: Ipv4Addr,
    peer_ip: Ipv4Addr,
    mtu: u16,
    stats: Arc<bridge::BridgeStats>,
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
        Self::connect_reusing(
            host,
            port,
            username,
            password,
            trusted_cert,
            helper_client,
            &mut None,
        )
        .await
    }

    /// Connect, adopting the TUN device and routes left behind by a previous
    /// session when the gateway hands back the same addressing.
    ///
    /// `preserved` is taken only once its fate is decided: adopted on a matching
    /// reconnect, released when the addressing changed. It is left untouched if
    /// this attempt fails earlier than that, so the caller can retry with the
    /// routes still in place.
    #[allow(clippy::too_many_arguments)]
    pub async fn connect_reusing(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        trusted_cert: &str,
        helper_client: &mut helper::HelperClient,
        preserved: &mut Option<PreservedNetwork>,
    ) -> Result<Self, FortiError> {
        // Every phase is timed and logged. When a connect fails, the last phase
        // on record is the one that failed — previously the caller got a single
        // error string with no indication of how far it had got.
        let t0 = std::time::Instant::now();
        let mut mark = t0;
        let mut phase = |name: &str| {
            let ms = mark.elapsed().as_millis();
            mark = std::time::Instant::now();
            log::info!(target: "vpn", "connect[{host}:{port}] {name} in {ms} ms");
        };

        log::info!(target: "vpn", "connect[{host}:{port}] starting as {username}");

        // Phase 1: Authenticate (sync TLS in blocking task)
        let (cookie, config) = tokio::task::spawn_blocking({
            let host = host.to_string();
            let username = username.to_string();
            let password = password.to_string();
            let trusted_cert = trusted_cert.to_string();
            move || auth::authenticate(&host, port, &username, &password, &trusted_cert)
        })
        .await
        .map_err(|e| FortiError::Io(std::io::Error::other(e)))?
        .inspect_err(
            |e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED in auth: {e}"),
        )?;
        phase("auth ok");

        // Phase 2: Open async TLS tunnel
        let mut tls_stream = bridge::async_tls_connect(host, port, trusted_cert)
            .await
            .inspect_err(
                |e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED in tls_connect: {e}"),
            )?;
        phase("tls connected");

        bridge::open_tunnel(&mut tls_stream, host, port, &cookie)
            .await
            .inspect_err(
                |e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED in open_tunnel: {e}"),
            )?;
        phase("tunnel opened");

        // Phase 3: PPP negotiation
        let (mut tls_reader, mut tls_writer) = tokio::io::split(tls_stream);
        let ppp = bridge::negotiate_ppp(&mut tls_reader, &mut tls_writer)
            .await
            .inspect_err(
                |e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED in ppp: {e}"),
            )?;
        phase(&format!(
            "ppp negotiated (ip {}, mtu {})",
            ppp.assigned_ip, ppp.mtu
        ));

        // Reassemble TLS stream from halves
        let tls_stream = tls_reader.unsplit(tls_writer);

        // Use PPP-negotiated IP if different from XML config
        let final_ip = if !ppp.assigned_ip.is_unspecified() {
            ppp.assigned_ip
        } else {
            config.assigned_ip
        };

        // Phase 4: Adopt the previous TUN device, or create a new one.
        // Adoption is only safe when the gateway assigned the same addressing —
        // the installed routes point at that exact local IP.
        let can_reuse = preserved
            .as_ref()
            .is_some_and(|p| p.matches(host, port, final_ip, config.peer_ip));

        let (tun_dev, tun_keeper, route_manager) = if can_reuse {
            let p = preserved.take().expect("checked by can_reuse");
            let tun_dev = p
                .tun
                .open_async()
                .map_err(|e| FortiError::TunDeviceError(format!("Reopen tun: {e}")))
                .inspect_err(|e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED reopening preserved tun: {e}"))?;
            phase("adopted preserved tun and routes");
            (tun_dev, Some(p.tun), p.route_manager)
        } else {
            // Addressing changed (or nothing to adopt) — drop the stale routes
            // and TUN before installing fresh ones.
            if let Some(p) = preserved.take() {
                p.release(Some(helper_client));
            }
            let tun_handle = helper_client
                .create_tun(final_ip, config.peer_ip, ppp.mtu)
                .inspect_err(|e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED in create_tun: {e}"))?;
            let tun_name = tun_handle.1.clone();
            // Duplicate before the bridge takes ownership; without this copy the
            // interface dies with the bridge and takes its routes with it.
            let keeper = platform::TunKeeper::from_handle(&tun_handle).ok();
            let tun_dev = platform::AsyncTunFd::from_handle(tun_handle)
                .map_err(|e| FortiError::TunDeviceError(format!("Async tun: {e}")))?;

            let gateway_ip = format!("{host}:{port}")
                .to_socket_addrs()
                .ok()
                .and_then(|mut addrs| addrs.next())
                .map(|a| match a.ip() {
                    std::net::IpAddr::V4(ip) => ip,
                    _ => Ipv4Addr::UNSPECIFIED,
                })
                .unwrap_or(Ipv4Addr::UNSPECIFIED);
            phase(&format!("tun {tun_name} created"));

            let mut route_manager = routing::RouteManager::new(gateway_ip, &tun_name);
            route_manager
                .configure_via_helper(&config, helper_client)
                .inspect_err(
                    |e| log::error!(target: "vpn", "connect[{host}:{port}] FAILED in routes: {e}"),
                )?;
            phase(if config.routes.is_empty() {
                "routes configured (full tunnel)"
            } else {
                "routes configured (split tunnel)"
            });
            (tun_dev, keeper, route_manager)
        };

        // Phase 5: Start bridge (tun ↔ tunnel)
        let shutdown = Arc::new(Notify::new());
        let bridge_handle =
            bridge::start_bridge(tls_stream, tun_dev, shutdown.clone(), ppp.magic_number);
        log::info!(
            target: "vpn",
            "connect[{host}:{port}] ESTABLISHED in {} ms total (ip {final_ip}, mtu {})",
            t0.elapsed().as_millis(),
            ppp.mtu
        );

        Ok(Self {
            shutdown,
            route_manager: Some(route_manager),
            bridge_tasks: bridge_handle.tasks,
            alive: bridge_handle.alive,
            event_rx: Some(bridge_handle.event_rx),
            stats: bridge_handle.stats,
            host: host.to_string(),
            port,
            cookie,
            trusted_cert: trusted_cert.to_string(),
            tun_keeper,
            assigned_ip: final_ip,
            peer_ip: config.peer_ip,
            mtu: ppp.mtu,
        })
    }

    /// MTU the gateway negotiated for this session's TUN device.
    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// Live counters for this session's tunnel.
    pub fn stats(&self) -> &bridge::BridgeStats {
        &self.stats
    }

    /// Take the event receiver for external monitoring.
    /// Returns `None` if already taken.
    pub fn take_event_rx(&mut self) -> Option<tokio::sync::watch::Receiver<VpnEvent>> {
        self.event_rx.take()
    }

    /// Check if the session is still alive (LCP echo health).
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && self.bridge_tasks.iter().all(|t| !t.is_finished())
    }

    /// Disconnect: stop tunnel, restore routes, send logout.
    pub async fn disconnect(&mut self, mut helper: Option<&mut helper::HelperClient>) {
        log::info!(target: "vpn", "Disconnecting {} | {}", self.host, self.stats.summary());
        self.stop_bridge().await;

        // Restore routes via helper
        if let Some(ref mut rm) = self.route_manager {
            rm.restore_via_helper(helper.as_deref_mut());
        }
        self.route_manager = None;

        // Close the TUN device — ours and the helper's copy — to prevent a stale
        // utun on reconnect.
        self.tun_keeper = None;
        if let Some(ref mut h) = helper {
            let _ = h.destroy_tun();
        }

        self.send_logout().await;
    }

    /// Stop the tunnel but leave the routes, DNS and TUN device installed, so a
    /// reconnect can resume without a host-wide route teardown.
    ///
    /// Returns the state for the next [`Self::connect_reusing`] to adopt. Returns
    /// `None` — after a full [`Self::disconnect`] — when this platform cannot
    /// hold the TUN device open, since routes cannot outlive their interface.
    pub async fn disconnect_preserving_network(
        &mut self,
        helper: Option<&mut helper::HelperClient>,
    ) -> Option<PreservedNetwork> {
        if self.tun_keeper.is_none() || self.route_manager.is_none() {
            // Routes cannot outlive their interface, so with no spare TUN
            // descriptor there is nothing safe to hold on to — tear it all down.
            self.disconnect(helper).await;
            return None;
        }

        log::info!(
            target: "vpn",
            "Stopping tunnel but keeping routes/DNS/TUN for reconnect | {}",
            self.stats.summary()
        );
        self.stop_bridge().await;
        self.send_logout().await;

        let mut route_manager = self.route_manager.take().expect("checked above");
        // Route cleanup needs the privileged helper, which Drop cannot reach —
        // same reason VpnSession::drop skips it. release() is the real path.
        route_manager.skip_drop_restore();

        Some(PreservedNetwork {
            tun: self.tun_keeper.take().expect("checked above"),
            host: self.host.clone(),
            port: self.port,
            assigned_ip: self.assigned_ip,
            peer_ip: self.peer_ip,
            route_manager,
        })
    }

    /// Signal the bridge tasks to stop and wait for them to wind down.
    ///
    /// A task that misses the shutdown notification is aborted rather than left
    /// running: it still owns a TUN descriptor, and the next session may be about
    /// to adopt that same interface.
    async fn stop_bridge(&mut self) {
        self.shutdown.notify_waiters();
        for mut task in self.bridge_tasks.drain(..) {
            if tokio::time::timeout(tokio::time::Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
    }

    /// Tell the gateway the session is over (best-effort).
    async fn send_logout(&self) {
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
        // Note: route cleanup requires the privileged helper, which Drop cannot access.
        // The caller must call disconnect() before dropping to properly restore routes.
        // Take the route_manager to prevent RouteManager::Drop from running
        // unprivileged route commands (which would fail with "Permission denied").
        if let Some(mut rm) = self.route_manager.take() {
            rm.skip_drop_restore();
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

    /// Build a PreservedNetwork without touching the network — only the address
    /// fields matter for the adoption decision.
    #[cfg(unix)]
    fn preserved_for(assigned_ip: Ipv4Addr, peer_ip: Ipv4Addr) -> PreservedNetwork {
        use std::os::fd::AsRawFd;
        // A socket pair stands in for the tun device: pollable like the real
        // thing, and never read or written here. from_handle dups the descriptor,
        // so the pair keeps its own copy.
        let (sock, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let handle: platform::TunHandle = (sock.as_raw_fd(), "utun9".to_string());
        let tun = platform::TunKeeper::from_handle(&handle).unwrap();
        let mut route_manager = routing::RouteManager::new(Ipv4Addr::new(1, 2, 3, 4), "utun9");
        route_manager.skip_drop_restore();
        PreservedNetwork {
            tun,
            host: "vpn.example.com".to_string(),
            port: 443,
            assigned_ip,
            peer_ip,
            route_manager,
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_preserved_network_matches_same_addressing() {
        let p = preserved_for(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(169, 254, 2, 1));
        assert!(p.matches(
            "vpn.example.com",
            443,
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(169, 254, 2, 1)
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_preserved_network_rejects_new_assigned_ip() {
        // A different local IP means the installed routes point at the wrong
        // gateway — they have to be reinstalled, not adopted.
        let p = preserved_for(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(169, 254, 2, 1));
        assert!(!p.matches(
            "vpn.example.com",
            443,
            Ipv4Addr::new(10, 0, 0, 6),
            Ipv4Addr::new(169, 254, 2, 1)
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_preserved_network_rejects_other_gateway() {
        // Two profiles can hand out the same private IP. The preserved host route
        // points at the old gateway, so adopting across gateways would risk
        // routing the new tunnel's own traffic into the tunnel.
        let p = preserved_for(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(169, 254, 2, 1));
        assert!(!p.matches(
            "other.example.com",
            443,
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(169, 254, 2, 1)
        ));
        assert!(!p.matches(
            "vpn.example.com",
            10443,
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(169, 254, 2, 1)
        ));
    }

    #[test]
    #[cfg(unix)]
    fn test_preserved_network_rejects_new_peer_ip() {
        let p = preserved_for(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(169, 254, 2, 1));
        assert!(!p.matches(
            "vpn.example.com",
            443,
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(169, 254, 2, 9)
        ));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_preserved_network_reopens_tun_handle() {
        // Adoption depends on the kept descriptor still being usable.
        let p = preserved_for(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(169, 254, 2, 1));
        assert!(p.tun.open_async().is_ok());
    }

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
