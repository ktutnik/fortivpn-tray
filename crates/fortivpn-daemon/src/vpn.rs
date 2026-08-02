use std::collections::HashMap;

use crate::profile::VpnProfile;
use fortivpn::helper::HelperClient;
use fortivpn::{PreservedNetwork, VpnSession};

#[derive(Debug, Clone, PartialEq)]
pub enum VpnStatus {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Reconnecting,
    Error(String),
}

pub struct VpnManager {
    pub status: VpnStatus,
    pub(crate) session: Option<VpnSession>,
    helper: Option<HelperClient>,
    pub(crate) connected_profile_id: Option<String>,
    pub(crate) monitor_handle: Option<tokio::task::JoinHandle<()>>,
    pub(crate) session_passwords: HashMap<String, String>,
    /// Routes, DNS and TUN device held open between a dropped tunnel and its
    /// reconnect. Must be released if the reconnect is abandoned, or the host is
    /// left routing into a dead tunnel.
    pub(crate) preserved_network: Option<PreservedNetwork>,
}

impl VpnManager {
    pub fn new() -> Self {
        Self {
            status: VpnStatus::Disconnected,
            session: None,
            helper: None,
            connected_profile_id: None,
            monitor_handle: None,
            session_passwords: HashMap::new(),
            preserved_network: None,
        }
    }

    pub fn connected_profile_id(&self) -> Option<&str> {
        self.connected_profile_id.as_deref()
    }

    /// Ensure we have a connection to the privileged helper daemon.
    fn ensure_helper(&mut self) -> Result<&mut HelperClient, String> {
        // Check if existing connection is still alive by sending a ping
        if let Some(ref mut h) = self.helper {
            if h.ping().is_ok() {
                return Ok(self.helper.as_mut().unwrap());
            }
            // Connection lost, drop it
            self.helper = None;
        }

        // Connect to the launchd-managed helper daemon
        let helper = HelperClient::connect().map_err(|e| e.to_string())?;
        self.helper = Some(helper);
        Ok(self.helper.as_mut().unwrap())
    }

    pub async fn connect_with_password(
        &mut self,
        profile: &VpnProfile,
        password: &str,
    ) -> Result<(), String> {
        if self.status == VpnStatus::Connected {
            return Err("Already connected".to_string());
        }

        self.status = VpnStatus::Connecting;

        // Ensure helper is running (spawns on first connect, reuses after)
        self.ensure_helper()?;
        let helper = self.helper.as_mut().unwrap();

        // Hand the previous session's routes/DNS/TUN to the new session so a
        // reconnect to the same address does not touch the host's routing table.
        // Anything left in `preserved` after the call is still installed and is
        // reused by the next attempt.
        let mut preserved = self.preserved_network.take();
        let result = VpnSession::connect_reusing(
            &profile.host,
            profile.port,
            &profile.username,
            password,
            &profile.trusted_cert,
            helper,
            &mut preserved,
        )
        .await;
        self.preserved_network = preserved;

        let session = result.map_err(|e| {
            self.status = VpnStatus::Error(e.to_string());
            self.connected_profile_id = None;
            e.to_string()
        })?;

        // Log the negotiated MTU: it comes from the gateway's LCP MRU now, and it
        // is the first thing to check if large transfers stall.
        log::info!(
            target: "vpn",
            "Connected to {} — tunnel MTU {}",
            profile.name,
            session.mtu()
        );

        self.session = Some(session);
        self.connected_profile_id = Some(profile.id.clone());
        // Retain the password in memory for the session so the monitor can
        // auto-reconnect after an unexpected drop. Cleared on disconnect.
        self.session_passwords
            .insert(profile.id.clone(), password.to_string());
        self.status = VpnStatus::Connected;
        Ok(())
    }

    /// Check if the VPN session is still alive.
    /// Returns true if it was connected but the session has died.
    #[allow(dead_code)]
    pub async fn check_alive(&mut self) -> bool {
        if self.status != VpnStatus::Connected {
            return false;
        }
        if let Some(ref session) = self.session {
            if !session.is_alive() {
                self.session = None;
                self.connected_profile_id = None;
                self.status = VpnStatus::Error("VPN connection lost".to_string());
                return true;
            }
        }
        false
    }

    /// Handle session death detected by the event monitor.
    /// Unlike disconnect(), this does NOT abort the monitor (caller IS the monitor).
    /// Returns the (profile_id, password) of the dead session so the caller can
    /// attempt an auto-reconnect.
    pub async fn handle_session_death(&mut self, reason: String) -> Option<(String, String)> {
        let reconnect_info = self
            .connected_profile_id
            .take()
            .and_then(|id| self.session_passwords.remove(&id).map(|pw| (id, pw)));

        if let Some(ref mut session) = self.session {
            if reconnect_info.is_some() {
                // A reconnect is coming: keep routes, DNS and the TUN device
                // installed so the gap is a stall rather than a host-wide
                // routing teardown that kills every open connection.
                self.preserved_network = session
                    .disconnect_preserving_network(self.helper.as_mut())
                    .await;
            } else {
                session.disconnect(self.helper.as_mut()).await;
            }
        }
        self.session = None;
        self.status = VpnStatus::Error(reason);
        self.monitor_handle = None;
        reconnect_info
    }

    /// Restore whatever a pending reconnect was holding open — routes, DNS and
    /// the TUN device. A no-op when nothing is held.
    pub(crate) fn release_preserved_network(&mut self) {
        if let Some(preserved) = self.preserved_network.take() {
            preserved.release(self.helper.as_mut());
        }
    }

    pub async fn disconnect(&mut self) -> Result<(), String> {
        self.status = VpnStatus::Disconnecting;

        if let Some(handle) = self.monitor_handle.take() {
            handle.abort();
        }

        if let Some(ref mut session) = self.session {
            session.disconnect(self.helper.as_mut()).await;
        }
        self.release_preserved_network();

        if let Some(ref id) = self.connected_profile_id {
            self.session_passwords.remove(id);
        }

        self.session = None;
        self.connected_profile_id = None;
        self.status = VpnStatus::Disconnected;
        // Note: helper stays alive for next connect
        Ok(())
    }
}

impl Drop for VpnManager {
    fn drop(&mut self) {
        // Shut down the helper when the manager is dropped (app exit)
        if let Some(ref mut h) = self.helper {
            h.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vpn_status_eq() {
        assert_eq!(VpnStatus::Disconnected, VpnStatus::Disconnected);
        assert_eq!(VpnStatus::Connecting, VpnStatus::Connecting);
        assert_eq!(VpnStatus::Connected, VpnStatus::Connected);
        assert_eq!(VpnStatus::Disconnecting, VpnStatus::Disconnecting);
        assert_eq!(
            VpnStatus::Error("test".to_string()),
            VpnStatus::Error("test".to_string())
        );
    }

    #[test]
    fn test_vpn_status_ne() {
        assert_ne!(VpnStatus::Disconnected, VpnStatus::Connected);
        assert_ne!(VpnStatus::Connecting, VpnStatus::Disconnecting);
        assert_ne!(
            VpnStatus::Error("a".to_string()),
            VpnStatus::Error("b".to_string())
        );
    }

    #[test]
    fn test_vpn_manager_new() {
        let manager = VpnManager::new();
        assert_eq!(manager.status, VpnStatus::Disconnected);
        assert!(manager.connected_profile_id().is_none());
    }

    #[test]
    fn test_vpn_status_clone() {
        let status = VpnStatus::Error("test error".to_string());
        let cloned = status.clone();
        assert_eq!(status, cloned);
    }

    #[test]
    fn test_vpn_status_debug() {
        let status = VpnStatus::Connected;
        let debug = format!("{:?}", status);
        assert_eq!(debug, "Connected");
    }

    #[test]
    fn test_vpn_status_error_debug() {
        let status = VpnStatus::Error("timeout".to_string());
        let debug = format!("{:?}", status);
        assert!(debug.contains("timeout"));
    }

    #[test]
    fn test_vpn_manager_initial_no_session() {
        let manager = VpnManager::new();
        assert!(manager.session.is_none());
    }

    #[test]
    fn test_vpn_manager_connected_profile_id_none_initially() {
        let manager = VpnManager::new();
        assert_eq!(manager.connected_profile_id(), None);
    }

    #[test]
    fn test_vpn_status_all_variants_debug() {
        assert_eq!(format!("{:?}", VpnStatus::Disconnected), "Disconnected");
        assert_eq!(format!("{:?}", VpnStatus::Connecting), "Connecting");
        assert_eq!(format!("{:?}", VpnStatus::Connected), "Connected");
        assert_eq!(format!("{:?}", VpnStatus::Disconnecting), "Disconnecting");
        let err_debug = format!("{:?}", VpnStatus::Error("msg".to_string()));
        assert!(err_debug.contains("Error"));
        assert!(err_debug.contains("msg"));
    }

    #[test]
    fn test_vpn_status_clone_all_variants() {
        let variants = vec![
            VpnStatus::Disconnected,
            VpnStatus::Connecting,
            VpnStatus::Connected,
            VpnStatus::Disconnecting,
            VpnStatus::Error("err".to_string()),
        ];
        for v in &variants {
            assert_eq!(v, &v.clone());
        }
    }

    #[test]
    fn test_vpn_manager_drop_no_panic() {
        let manager = VpnManager::new();
        drop(manager);
    }

    #[tokio::test]
    async fn test_vpn_manager_check_alive_when_disconnected() {
        let mut manager = VpnManager::new();
        assert!(!manager.check_alive().await);
    }

    #[tokio::test]
    async fn test_vpn_manager_disconnect_when_not_connected() {
        let mut manager = VpnManager::new();
        let result = manager.disconnect().await;
        assert!(result.is_ok());
        assert_eq!(manager.status, VpnStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_vpn_manager_disconnect_clears_state() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.connected_profile_id = Some("test-id".to_string());
        // No actual session, so disconnect should just clear state
        let result = manager.disconnect().await;
        assert!(result.is_ok());
        assert_eq!(manager.status, VpnStatus::Disconnected);
        assert!(manager.connected_profile_id().is_none());
        assert!(manager.session.is_none());
    }

    #[tokio::test]
    async fn test_vpn_manager_disconnect_from_connecting() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connecting;
        let result = manager.disconnect().await;
        assert!(result.is_ok());
        assert_eq!(manager.status, VpnStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_vpn_manager_disconnect_from_error() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Error("something broke".to_string());
        manager.connected_profile_id = Some("old-id".to_string());
        let result = manager.disconnect().await;
        assert!(result.is_ok());
        assert_eq!(manager.status, VpnStatus::Disconnected);
        assert!(manager.connected_profile_id().is_none());
    }

    #[tokio::test]
    async fn test_vpn_manager_check_alive_when_connecting() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connecting;
        assert!(!manager.check_alive().await);
    }

    #[tokio::test]
    async fn test_vpn_manager_check_alive_when_error() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Error("err".to_string());
        assert!(!manager.check_alive().await);
    }

    #[tokio::test]
    async fn test_vpn_manager_check_alive_connected_no_session() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.connected_profile_id = Some("test".to_string());
        // No session object - check_alive should return false (no session to check)
        let died = manager.check_alive().await;
        assert!(!died);
    }

    #[tokio::test]
    async fn test_vpn_manager_multiple_disconnects() {
        let mut manager = VpnManager::new();
        manager.disconnect().await.unwrap();
        manager.disconnect().await.unwrap();
        assert_eq!(manager.status, VpnStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_disconnect_clears_session_password() {
        let mut manager = VpnManager::new();
        manager.connected_profile_id = Some("profile-1".to_string());
        manager
            .session_passwords
            .insert("profile-1".to_string(), "pw".to_string());
        manager.status = VpnStatus::Connected;

        manager.disconnect().await.unwrap();

        assert!(!manager.session_passwords.contains_key("profile-1"));
        assert_eq!(manager.status, VpnStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_connect_with_password_rejects_when_connected() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;

        let profile = VpnProfile {
            id: "p1".into(),
            name: "Test".into(),
            host: "h".into(),
            port: 443,
            username: "u".into(),
            trusted_cert: "".into(),
        };

        let result = manager.connect_with_password(&profile, "pw").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Already connected"));
    }

    #[tokio::test]
    async fn test_handle_session_death_sets_error() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.connected_profile_id = Some("p1".to_string());

        manager
            .handle_session_death("LCP echo timeout".to_string())
            .await;

        assert_eq!(
            manager.status,
            VpnStatus::Error("LCP echo timeout".to_string())
        );
        assert!(manager.connected_profile_id.is_none());
        assert!(manager.session.is_none());
    }

    #[tokio::test]
    async fn test_handle_session_death_returns_reconnect_info() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.connected_profile_id = Some("p1".to_string());
        manager
            .session_passwords
            .insert("p1".to_string(), "secret".to_string());

        let info = manager.handle_session_death("link down".to_string()).await;

        assert_eq!(info, Some(("p1".to_string(), "secret".to_string())));
        assert!(manager.session_passwords.is_empty());
    }

    #[tokio::test]
    async fn test_handle_session_death_no_password_returns_none() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.connected_profile_id = Some("p1".to_string());

        let info = manager.handle_session_death("link down".to_string()).await;

        assert_eq!(info, None);
    }

    #[tokio::test]
    async fn test_handle_session_death_clears_monitor() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.monitor_handle = Some(tokio::spawn(async {}));

        manager.handle_session_death("died".to_string()).await;

        assert!(manager.monitor_handle.is_none());
    }

    #[tokio::test]
    async fn test_release_preserved_network_is_noop_when_empty() {
        let mut manager = VpnManager::new();
        manager.release_preserved_network();
        assert!(manager.preserved_network.is_none());
    }

    #[tokio::test]
    async fn test_disconnect_releases_preserved_network() {
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Reconnecting;
        manager.disconnect().await.unwrap();
        // Nothing may be left holding the host's routes hostage.
        assert!(manager.preserved_network.is_none());
        assert_eq!(manager.status, VpnStatus::Disconnected);
    }

    #[tokio::test]
    async fn test_session_death_without_reconnect_info_preserves_nothing() {
        // No retained password means no reconnect is coming, so the routes must be
        // restored rather than held.
        let mut manager = VpnManager::new();
        manager.status = VpnStatus::Connected;
        manager.connected_profile_id = Some("p1".to_string());

        let info = manager.handle_session_death("link down".to_string()).await;

        assert_eq!(info, None);
        assert!(manager.preserved_network.is_none());
    }

    #[test]
    fn test_vpn_status_is_busy() {
        // isBusy is checked in Swift, but test the Rust status variants that map to it
        assert_ne!(VpnStatus::Connecting, VpnStatus::Disconnected);
        assert_ne!(VpnStatus::Disconnecting, VpnStatus::Disconnected);
    }
}
