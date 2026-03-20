use keyring::Entry;

const SERVICE_NAME: &str = "fortivpn-tray";

pub fn store_password(profile_id: &str, password: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, profile_id)
        .map_err(|e| format!("Failed to create keychain entry: {e}"))?;
    entry
        .set_password(password)
        .map_err(|e| format!("Failed to store password: {e}"))
}

pub fn get_password(profile_id: &str) -> Result<String, String> {
    let entry = Entry::new(SERVICE_NAME, profile_id)
        .map_err(|e| format!("Failed to create keychain entry: {e}"))?;
    entry
        .get_password()
        .map_err(|e| format!("Failed to retrieve password: {e}"))
}

#[allow(dead_code)]
pub fn delete_password(profile_id: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, profile_id)
        .map_err(|e| format!("Failed to create keychain entry: {e}"))?;
    entry
        .delete_credential()
        .map_err(|e| format!("Failed to delete password: {e}"))
}
