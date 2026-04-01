mod installer;
mod ipc;
mod notification;
mod profile;
mod vpn;

use std::sync::{Arc, Mutex};

fn init_logger() {
    // macOS: unified logging (Console.app, `log stream`)
    #[cfg(feature = "macos-logging")]
    {
        oslog::OsLogger::new("com.fortivpn-tray")
            .level_filter(log::LevelFilter::Info)
            .category_level_filter("ipc", log::LevelFilter::Debug)
            .category_level_filter("vpn", log::LevelFilter::Debug)
            .init()
            .ok();
    }

    // Linux/Windows: env_logger (stderr, RUST_LOG env var)
    #[cfg(feature = "generic-logging")]
    {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    }

    // Fallback if no logging feature enabled
    #[cfg(not(any(feature = "macos-logging", feature = "generic-logging")))]
    {
        let _ = log::LevelFilter::Info;
    }
}

#[tokio::main]
async fn main() {
    init_logger();

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
