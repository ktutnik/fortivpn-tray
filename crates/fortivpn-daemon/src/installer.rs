//! First-launch detection and helper daemon installation.

use crate::platform;

/// Check if the helper daemon is installed and reachable.
pub fn is_helper_installed() -> bool {
    platform::is_helper_installed()
}

/// Check if the installed helper needs upgrading.
#[allow(dead_code)]
pub fn needs_upgrade(bundled_version: &str) -> bool {
    platform::needs_upgrade(bundled_version)
}

/// Install or upgrade the helper daemon. Prompts for admin password once.
pub fn install_helper() -> Result<(), String> {
    platform::install_helper()
}
