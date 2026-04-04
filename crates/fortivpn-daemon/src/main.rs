// Suppress console window on Windows
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod installer;
mod ipc;
mod notification;
mod platform;
mod profile;
mod vpn;

use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    platform::init_logger();

    // Install rustls CryptoProvider (required since multiple providers may be available)
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    log::info!(target: "daemon", "FortiVPN daemon starting");

    let store = profile::ProfileStore::load();
    log::info!(target: "daemon", "Loaded {} profiles", store.profiles.len());

    let vpn_manager = vpn::VpnManager::new();

    let (status_tx, _) = tokio::sync::broadcast::channel::<String>(16);

    let state = ipc::AppState {
        vpn: Arc::new(tokio::sync::Mutex::new(vpn_manager)),
        store: Arc::new(Mutex::new(store)),
        status_tx,
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
