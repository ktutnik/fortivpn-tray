use std::path::Path;
use std::process::Command;

const HELPER_SOCKET: &str = "/var/run/fortivpn-helper.sock";
const HELPER_INSTALL_PATH: &str = "/Library/PrivilegedHelperTools/fortivpn-helper";
const PLIST_INSTALL_PATH: &str = "/Library/LaunchDaemons/com.fortivpn-tray.helper.plist";

pub fn init_logger() {
    oslog::OsLogger::new("com.fortivpn-tray")
        .level_filter(log::LevelFilter::Info)
        .category_level_filter("ipc", log::LevelFilter::Debug)
        .category_level_filter("vpn", log::LevelFilter::Debug)
        .init()
        .ok();
}

/// Check if the helper daemon is installed and reachable.
pub fn is_helper_installed() -> bool {
    std::os::unix::net::UnixStream::connect(HELPER_SOCKET).is_ok()
        || Path::new(PLIST_INSTALL_PATH).exists()
}

/// Check if the installed helper needs upgrading.
#[allow(dead_code)]
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
pub fn install_helper() -> Result<(), String> {
    let helper_src = find_bundled_helper()?;
    let plist_src = find_bundled_plist()?;

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

fn find_bundled_helper() -> Result<String, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // .app bundle: Contents/MacOS/../Resources/
            if let Some(parent) = dir.parent() {
                let rd = parent.join("Resources");
                for name in &[
                    "fortivpn-helper",
                    "fortivpn-helper-aarch64-apple-darwin",
                    "fortivpn-helper-x86_64-apple-darwin",
                ] {
                    let path = rd.join(name);
                    if path.exists() {
                        return Ok(path.to_string_lossy().to_string());
                    }
                }
            }
            // Next to executable (dev builds)
            let path = dir.join("fortivpn-helper");
            if path.exists() {
                return Ok(path.to_string_lossy().to_string());
            }
        }
    }
    Err("Bundled helper binary not found".to_string())
}

fn find_bundled_plist() -> Result<String, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(parent) = dir.parent() {
                let path = parent
                    .join("Resources")
                    .join("com.fortivpn-tray.helper.plist");
                if path.exists() {
                    return Ok(path.to_string_lossy().to_string());
                }
            }
        }
    }
    // Dev: relative to working directory
    let path = std::path::Path::new("resources/com.fortivpn-tray.helper.plist");
    if path.exists() {
        return Ok(path.to_string_lossy().to_string());
    }
    Err("Bundled plist not found".to_string())
}
