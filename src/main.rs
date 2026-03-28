mod installer;
mod ipc;
mod keychain;
mod notification;
mod profile;
mod vpn;

use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    // Initialize macOS unified logging (visible in Console.app)
    oslog::OsLogger::new("com.fortivpn-tray")
        .level_filter(log::LevelFilter::Info)
        .category_level_filter("ipc", log::LevelFilter::Debug)
        .category_level_filter("vpn", log::LevelFilter::Debug)
        .init()
        .expect("Failed to initialize logger");

    log::info!(target: "daemon", "FortiVPN daemon starting");

    let store = profile::ProfileStore::load();
    log::info!(target: "daemon", "Loaded {} profiles", store.profiles.len());

    let vpn_manager = vpn::VpnManager::new();

    let state = ipc::AppState {
        vpn: Arc::new(tokio::sync::Mutex::new(vpn_manager)),
        store: Arc::new(Mutex::new(store)),
    };

    if !installer::is_helper_installed() {
        log::warn!(target: "daemon", "Helper not installed, attempting installation");
        if let Err(e) = installer::install_helper() {
            log::error!(target: "daemon", "Helper installation failed: {e}");
        }
    }

    log::info!(target: "daemon", "Starting IPC server");
    ipc::start_ipc_server(state).await;
}
