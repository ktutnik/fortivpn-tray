//! First-launch detection and helper daemon installation.

use std::path::Path;
use std::process::Command;

const HELPER_SOCKET: &str = "/var/run/fortivpn-helper.sock";
const HELPER_INSTALL_PATH: &str = "/Library/PrivilegedHelperTools/fortivpn-helper";
const PLIST_INSTALL_PATH: &str = "/Library/LaunchDaemons/com.fortivpn-tray.helper.plist";

/// Check if the helper daemon is installed and reachable.
pub fn is_helper_installed() -> bool {
    std::os::unix::net::UnixStream::connect(HELPER_SOCKET).is_ok()
        || Path::new(PLIST_INSTALL_PATH).exists()
}

/// Check if the installed helper needs upgrading.
pub fn needs_upgrade(bundled_version: &str) -> bool {
    match fortivpn::helper::HelperClient::connect() {
        Ok(mut client) => match client.version() {
            Ok(installed) => installed != bundled_version,
            Err(_) => true,
        },
        Err(_) => false,
    }
}

/// Install or upgrade the helper daemon. Prompts for admin password once.
pub fn install_helper(app: &tauri::AppHandle) -> Result<(), String> {
    let helper_src = find_bundled_helper(app)?;
    let plist_src = find_bundled_plist(app)?;

    let script = format!(
        r#"do shell script "
            mkdir -p /Library/PrivilegedHelperTools && \
            cp '{}' '{}' && \
            chmod 755 '{}' && \
            chown root:wheel '{}' && \
            cp '{}' '{}' && \
            chown root:wheel '{}' && \
            launchctl bootout system '{}' 2>/dev/null; \
            launchctl bootstrap system '{}'
        " with administrator privileges"#,
        helper_src.replace('\'', "'\\''"),
        HELPER_INSTALL_PATH,
        HELPER_INSTALL_PATH,
        HELPER_INSTALL_PATH,
        plist_src.replace('\'', "'\\''"),
        PLIST_INSTALL_PATH,
        PLIST_INSTALL_PATH,
        PLIST_INSTALL_PATH,
        PLIST_INSTALL_PATH,
    );

    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| format!("Failed to run installer: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("User canceled") || stderr.contains("(-128)") {
            return Err("Installation cancelled by user".to_string());
        }
        return Err(format!("Installation failed: {stderr}"));
    }

    Ok(())
}

fn find_bundled_helper(app: &tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("Resource dir: {e}"))?;

    for name in &[
        "fortivpn-helper",
        "fortivpn-helper-aarch64-apple-darwin",
        "fortivpn-helper-x86_64-apple-darwin",
    ] {
        let path = resource_dir.join("binaries").join(name);
        if path.exists() {
            return Ok(path.to_string_lossy().to_string());
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("fortivpn-helper");
            if path.exists() {
                return Ok(path.to_string_lossy().to_string());
            }
        }
    }

    Err("Bundled helper binary not found".to_string())
}

fn find_bundled_plist(app: &tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("Resource dir: {e}"))?;
    let path = resource_dir.join("com.fortivpn-tray.helper.plist");
    if path.exists() {
        return Ok(path.to_string_lossy().to_string());
    }
    Err("Bundled plist not found".to_string())
}
