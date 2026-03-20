mod installer;
mod ipc;
mod keychain;
mod notification;
mod profile;
mod vpn;

use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    let store = profile::ProfileStore::load();
    let vpn_manager = vpn::VpnManager::new();

    let state = ipc::AppState {
        vpn: Arc::new(tokio::sync::Mutex::new(vpn_manager)),
        store: Arc::new(Mutex::new(store)),
    };

    if !installer::is_helper_installed() {
        if let Err(e) = installer::install_helper() {
            eprintln!("Helper installation failed: {e}");
        }
    }

    ipc::start_ipc_server(state).await;
}
