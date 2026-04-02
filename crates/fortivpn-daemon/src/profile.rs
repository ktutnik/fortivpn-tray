use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub trusted_cert: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileStore {
    pub profiles: Vec<VpnProfile>,
    #[serde(skip)]
    save_path: Option<PathBuf>,
}

impl ProfileStore {
    pub fn config_path() -> PathBuf {
        let config_dir = dirs::config_dir()
            .expect("Could not find config directory")
            .join("fortivpn-tray");
        fs::create_dir_all(&config_dir).expect("Could not create config directory");
        config_dir.join("profiles.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        let mut store = if path.exists() {
            let data = fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Self::default()
        };
        store.save_path = Some(path);
        store
    }

    /// Create an in-memory store that doesn't write to disk. Used for tests.
    #[cfg(test)]
    pub fn in_memory(profiles: Vec<VpnProfile>) -> Self {
        Self {
            profiles,
            save_path: None,
        }
    }

    pub fn save(&self) {
        if let Some(ref path) = self.save_path {
            let data = serde_json::to_string_pretty(self).expect("Failed to serialize profiles");
            fs::write(path, data).expect("Failed to write profiles");
        }
        // If save_path is None (in-memory/test mode), don't write to disk
    }

    pub fn add(&mut self, profile: VpnProfile) {
        self.profiles.push(profile);
        self.save();
    }

    pub fn remove(&mut self, id: &str) {
        self.profiles.retain(|p| p.id != id);
        self.save();
    }

    pub fn get(&self, id: &str) -> Option<&VpnProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn update(&mut self, profile: VpnProfile) {
        if let Some(existing) = self.profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        }
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vpn_profile_serialization_roundtrip() {
        let profile = VpnProfile {
            id: "test-id-123".to_string(),
            name: "Test VPN".to_string(),
            host: "vpn.example.com".to_string(),
            port: 443,
            username: "testuser".to_string(),
            trusted_cert: "abcdef1234567890".to_string(),
        };
        let json = serde_json::to_string(&profile).unwrap();
        let deserialized: VpnProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "test-id-123");
        assert_eq!(deserialized.name, "Test VPN");
        assert_eq!(deserialized.host, "vpn.example.com");
        assert_eq!(deserialized.port, 443);
        assert_eq!(deserialized.username, "testuser");
        assert_eq!(deserialized.trusted_cert, "abcdef1234567890");
    }

    #[test]
    fn test_vpn_profile_deserialization() {
        let json = r#"{"id":"p1","name":"Office","host":"gw.corp.com","port":10443,"username":"admin","trusted_cert":"abc"}"#;
        let profile: VpnProfile = serde_json::from_str(json).unwrap();
        assert_eq!(profile.id, "p1");
        assert_eq!(profile.port, 10443);
    }

    #[test]
    fn test_profile_store_default() {
        let store = ProfileStore::default();
        assert!(store.profiles.is_empty());
    }

    #[test]
    fn test_profile_store_serialization_roundtrip() {
        let store = ProfileStore::in_memory(vec![
            VpnProfile {
                id: "1".to_string(),
                name: "VPN1".to_string(),
                host: "host1".to_string(),
                port: 443,
                username: "user1".to_string(),
                trusted_cert: "cert1".to_string(),
            },
            VpnProfile {
                id: "2".to_string(),
                name: "VPN2".to_string(),
                host: "host2".to_string(),
                port: 8443,
                username: "user2".to_string(),
                trusted_cert: "cert2".to_string(),
            },
        ]);
        let json = serde_json::to_string(&store).unwrap();
        let deserialized: ProfileStore = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.profiles.len(), 2);
        assert_eq!(deserialized.profiles[0].name, "VPN1");
        assert_eq!(deserialized.profiles[1].port, 8443);
    }

    #[test]
    fn test_profile_store_get() {
        let store = ProfileStore::in_memory(vec![VpnProfile {
            id: "abc".to_string(),
            name: "Test".to_string(),
            host: "host".to_string(),
            port: 443,
            username: "user".to_string(),
            trusted_cert: "".to_string(),
        }]);
        assert!(store.get("abc").is_some());
        assert!(store.get("xyz").is_none());
    }

    #[test]
    fn test_profile_store_config_path_contains_expected_parts() {
        let path = ProfileStore::config_path();
        assert!(path.to_string_lossy().contains("fortivpn-tray"));
        assert!(path.to_string_lossy().contains("profiles.json"));
    }

    #[test]
    fn test_in_memory_store_add_does_not_write_disk() {
        let mut store = ProfileStore::in_memory(vec![]);
        store.add(VpnProfile {
            id: "test".into(),
            name: "Test".into(),
            host: "h".into(),
            port: 443,
            username: "u".into(),
            trusted_cert: "".into(),
        });
        assert_eq!(store.profiles.len(), 1);
        // No file written — save_path is None
    }

    #[test]
    fn test_in_memory_store_remove() {
        let mut store = ProfileStore::in_memory(vec![VpnProfile {
            id: "p1".into(),
            name: "VPN1".into(),
            host: "h1".into(),
            port: 443,
            username: "u1".into(),
            trusted_cert: "".into(),
        }]);
        store.remove("p1");
        assert!(store.profiles.is_empty());
    }

    #[test]
    fn test_in_memory_store_update() {
        let mut store = ProfileStore::in_memory(vec![VpnProfile {
            id: "p1".into(),
            name: "Old".into(),
            host: "h1".into(),
            port: 443,
            username: "u1".into(),
            trusted_cert: "".into(),
        }]);
        store.update(VpnProfile {
            id: "p1".into(),
            name: "New".into(),
            host: "h2".into(),
            port: 8443,
            username: "u2".into(),
            trusted_cert: "cert".into(),
        });
        assert_eq!(store.get("p1").unwrap().name, "New");
        assert_eq!(store.get("p1").unwrap().port, 8443);
    }

    fn make_test_store() -> ProfileStore {
        ProfileStore::in_memory(vec![
            VpnProfile {
                id: "p1".into(),
                name: "VPN1".into(),
                host: "h1".into(),
                port: 443,
                username: "u1".into(),
                trusted_cert: "".into(),
            },
            VpnProfile {
                id: "p2".into(),
                name: "VPN2".into(),
                host: "h2".into(),
                port: 8443,
                username: "u2".into(),
                trusted_cert: "".into(),
            },
        ])
    }

    #[test]
    fn test_profile_store_get_not_found() {
        let store = make_test_store();
        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn test_profile_store_get_by_id() {
        let store = make_test_store();
        let p = store.get("p2").unwrap();
        assert_eq!(p.name, "VPN2");
        assert_eq!(p.port, 8443);
    }

    #[test]
    fn test_vpn_profile_clone() {
        let p = VpnProfile {
            id: "id".into(),
            name: "name".into(),
            host: "host".into(),
            port: 443,
            username: "user".into(),
            trusted_cert: "cert".into(),
        };
        let cloned = p.clone();
        assert_eq!(cloned.id, "id");
        assert_eq!(cloned.trusted_cert, "cert");
    }

    #[test]
    fn test_vpn_profile_debug() {
        let p = VpnProfile {
            id: "id".into(),
            name: "name".into(),
            host: "host".into(),
            port: 443,
            username: "user".into(),
            trusted_cert: "cert".into(),
        };
        let debug = format!("{:?}", p);
        assert!(debug.contains("VpnProfile"));
        assert!(debug.contains("name"));
    }

    #[test]
    fn test_profile_store_debug() {
        let store = ProfileStore::default();
        let debug = format!("{:?}", store);
        assert!(debug.contains("ProfileStore"));
    }

    #[test]
    fn test_profile_store_clone() {
        let store = make_test_store();
        let cloned = store.clone();
        assert_eq!(cloned.profiles.len(), 2);
        assert_eq!(cloned.profiles[0].id, "p1");
    }

    #[test]
    fn test_profile_store_serialize_pretty() {
        let store = make_test_store();
        let json = serde_json::to_string_pretty(&store).unwrap();
        assert!(json.contains("\"profiles\""));
        assert!(json.contains("VPN1"));
        assert!(json.contains('\n'));
    }

    #[test]
    fn test_profile_store_get_returns_correct_profile() {
        let store = ProfileStore::in_memory(vec![
            VpnProfile {
                id: "a".to_string(),
                name: "First".to_string(),
                host: "h1".to_string(),
                port: 443,
                username: "u1".to_string(),
                trusted_cert: "".to_string(),
            },
            VpnProfile {
                id: "b".to_string(),
                name: "Second".to_string(),
                host: "h2".to_string(),
                port: 8443,
                username: "u2".to_string(),
                trusted_cert: "".to_string(),
            },
        ]);
        let profile = store.get("b").unwrap();
        assert_eq!(profile.name, "Second");
        assert_eq!(profile.host, "h2");
    }
}
