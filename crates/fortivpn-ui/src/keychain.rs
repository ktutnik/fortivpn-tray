use keyring::Entry;

const SERVICE_NAME: &str = "fortivpn-tray";

pub fn read_password(profile_id: &str) -> Option<String> {
    Entry::new(SERVICE_NAME, profile_id)
        .ok()?
        .get_password()
        .ok()
}

pub fn store_password(profile_id: &str, password: &str) -> Result<(), String> {
    Entry::new(SERVICE_NAME, profile_id)
        .map_err(|e| format!("{e}"))?
        .set_password(password)
        .map_err(|e| format!("{e}"))
}

pub fn has_password(profile_id: &str) -> bool {
    read_password(profile_id).is_some()
}
