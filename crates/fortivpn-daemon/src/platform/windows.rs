pub fn init_logger() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}

pub fn is_helper_installed() -> bool {
    true
}

#[allow(dead_code)]
pub fn needs_upgrade(_bundled_version: &str) -> bool {
    false
}

pub fn install_helper() -> Result<(), String> {
    Ok(())
}
