# Platform Module Refactor — Design Spec

## Problem

The codebase has 82 `#[cfg(target_os = "...")]` guards scattered across 10 files. Platform-specific code (macOS GCD dispatch, Windows PostMessageW, routing commands, TUN creation) is interleaved with shared logic, making files hard to read and new platform support (e.g., FreeBSD) require touching many files.

## Goal

Replace scattered `#[cfg]` guards with **compile-time platform modules** — one file per OS per crate. Shared logic stays in the original files and delegates to `platform::*`. The only `#[cfg]` in the codebase will be on `mod` declarations in `platform/mod.rs`.

## Architecture

### Pattern

Each crate gets a `platform/` directory:

```
src/platform/
├── mod.rs       # single #[cfg] on mod declarations, re-exports
├── macos.rs     # macOS implementation
├── windows.rs   # Windows implementation
└── linux.rs     # Linux implementation
```

`mod.rs` re-exports the active platform:
```rust
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
```

Original files keep shared logic and call `platform::function_name()`. Zero `#[cfg]` outside of `platform/mod.rs`.

### Design Principle

- **Shared logic stays in original files** — `RouteManager` struct, route tracking, install/uninstall flow, `HelperClient` public API
- **Only divergent implementations move to platform** — shell commands, OS APIs, fd/handle passing
- **Each platform file exports the same function signatures** — adding a platform = create one file

---

## Crate 1: `fortivpn-app`

### Structure

```
crates/fortivpn-app/src/
├── main.rs              # shared: GPUI init, tray, menu, subscribe, event handling
├── ipc_client.rs        # unchanged (cross-platform TCP)
├── keychain.rs          # unchanged (keyring crate handles platform)
├── settings.rs          # unchanged (GPUI views)
├── notification.rs      # REMOVE — moves to platform
└── platform/
    ├── mod.rs
    ├── macos.rs         # ~80 lines
    ├── windows.rs       # ~120 lines (includes win_dispatch module)
    └── linux.rs         # ~30 lines
```

### Platform API

Each platform file exports:

```rust
/// One-time platform initialization (before GPUI event loop).
pub fn init() { ... }

/// Dispatch a function to the main thread safely.
pub fn dispatch_to_main(f: fn()) { ... }

/// Start the daemon process (with elevation on Windows).
pub fn ensure_daemon(daemon_path: &Path) { ... }

/// Hide app from Dock/Taskbar (tray-only mode).
pub fn hide_from_dock(cx: &mut gpui::App) { ... }

/// Show a desktop notification.
pub fn show_notification(title: &str, body: &str) { ... }

/// Update the tray icon (handles template icons on macOS).
pub fn set_tray_icon(tray: &TrayIcon, icon: Icon) { ... }

/// Create a hidden window to keep GPUI alive (Windows only, no-op elsewhere).
pub fn create_keepalive_window(cx: &mut gpui::App) { ... }
```

### What moves where

| From | To | Functions |
|------|----|-----------|
| `main.rs` dispatch_to_main | `platform/*.rs` | `dispatch_to_main()` |
| `main.rs` win_dispatch module | `platform/windows.rs` | `init()`, internal `WNDCLASSW` setup |
| `main.rs` ensure_daemon | `platform/*.rs` | `ensure_daemon()` |
| `main.rs` #[cfg] NSApplication | `platform/macos.rs` | `hide_from_dock()` |
| `main.rs` #[cfg] HiddenView | `platform/windows.rs` | `create_keepalive_window()` |
| `main.rs` refresh_icon #[cfg] | `platform/*.rs` | `set_tray_icon()` |
| `notification.rs` | `platform/*.rs` | `show_notification()` |

### main.rs after refactor (sketch)

```rust
mod platform;

fn main() {
    platform::init();
    platform::ensure_daemon(&daemon_path);

    app.run(|cx| {
        platform::hide_from_dock(cx);
        platform::create_keepalive_window(cx);
        // ... tray setup, menu, subscribe ...
    });
}

fn refresh_icon() {
    if let Ok(guard) = TRAY.lock() {
        if let Some(holder) = guard.as_ref() {
            if let Ok(icon) = load_icon(icon_bytes) {
                platform::set_tray_icon(&holder.0, icon);
            }
        }
    }
}

fn subscribe_loop() {
    // ...
    platform::dispatch_to_main(refresh_tray);
}
```

---

## Crate 2: `fortivpn` (VPN library)

### Structure

```
crates/fortivpn/src/
├── lib.rs               # VpnSession, shared types, silent_cmd
├── auth.rs              # unchanged
├── bridge.rs            # unchanged
├── ppp.rs               # unchanged
├── tunnel.rs            # unchanged
├── tun.rs               # shared create_tun, delegates device_name to platform
├── helper.rs            # shared HelperClient API, delegates to platform
├── routing.rs           # shared RouteManager, delegates commands to platform
├── async_tun.rs         # re-exports platform::AsyncTunFd
└── platform/
    ├── mod.rs
    ├── macos.rs         # ~250 lines
    ├── windows.rs       # ~200 lines
    └── linux.rs         # ~150 lines
```

### Platform API

```rust
// ── Helper ──

/// Platform-specific helper client internals.
pub struct HelperClientInner { ... }

impl HelperClientInner {
    pub fn connect() -> Result<Self, FortiError>;
    pub fn version(&mut self) -> Result<String, FortiError>;
    pub fn create_tun(ip, peer_ip, mtu) -> Result<TunHandle, FortiError>;
    pub fn destroy_tun(&mut self) -> Result<(), FortiError>;
    pub fn add_route(dest, gateway) -> Result<(), FortiError>;
    pub fn delete_route(dest) -> Result<(), FortiError>;
    pub fn configure_dns(config) -> Result<(), FortiError>;
    pub fn restore_dns(&mut self) -> Result<(), FortiError>;
    pub fn ping(&mut self) -> Result<(), FortiError>;
    pub fn shutdown(&mut self);
}

// ── TUN handle type (differs per platform) ──

// macOS/Linux:
pub type TunHandle = (std::os::fd::RawFd, String);
// Windows:
pub type TunHandle = (tun2::AsyncDevice, String);

// ── Async TUN ──

pub struct AsyncTunFd { ... }
// Implements AsyncRead + AsyncWrite

// ── Routing commands ──

pub fn run_route(action: &str, dest: &str, gateway: &str) -> Result<(), FortiError>;
pub fn get_default_gateway() -> Option<Ipv4Addr>;
pub fn configure_dns(tun_name: &str, config: &VpnConfig) -> Result<(), FortiError>;
pub fn restore_dns(tun_name: &str) -> Result<(), FortiError>;
pub fn disable_ipv6() -> Vec<String>;
pub fn restore_ipv6(interfaces: &[String]);

// ── Device name ──

pub fn device_name(dev: &tun2::AsyncDevice) -> String;
```

### What moves where

| From | To | Functions |
|------|----|-----------|
| `helper.rs` #[cfg(unix)] mod platform | `platform/macos.rs` + `platform/linux.rs` | `HelperClientInner`, `recv_fd()` |
| `helper.rs` #[cfg(windows)] mod platform | `platform/windows.rs` | `HelperClientInner` (direct TUN) |
| `routing.rs` run_route #[cfg] blocks | `platform/*.rs` | `run_route()` |
| `routing.rs` get_default_gateway #[cfg] | `platform/*.rs` | `get_default_gateway()` |
| `routing.rs` configure/restore_dns #[cfg] | `platform/*.rs` | `configure_dns()`, `restore_dns()` |
| `routing.rs` disable/restore_ipv6 #[cfg] | `platform/*.rs` | `disable_ipv6()`, `restore_ipv6()` |
| `async_tun.rs` #[cfg] modules | `platform/*.rs` | `AsyncTunFd` struct + impls |
| `tun.rs` device_name #[cfg] | `platform/*.rs` | `device_name()` |
| `lib.rs` VpnSession::connect #[cfg] | `platform/*.rs` via `TunHandle` type | TUN creation step |

### helper.rs after refactor (sketch)

```rust
mod platform;

pub struct HelperClient {
    inner: platform::HelperClientInner,
}

impl HelperClient {
    pub fn connect() -> Result<Self, FortiError> {
        Ok(Self { inner: platform::HelperClientInner::connect()? })
    }
    pub fn create_tun(&mut self, ip, peer_ip, mtu) -> Result<platform::TunHandle, FortiError> {
        self.inner.create_tun(ip, peer_ip, mtu)
    }
    // ... delegates all methods
}
```

### lib.rs VpnSession::connect after refactor

```rust
// No #[cfg] — TunHandle type is platform-defined
let (tun_handle, tun_name) = helper_client.create_tun(ip, peer_ip, mtu)?;
let async_tun = platform::AsyncTunFd::from_handle(tun_handle)?;
```

---

## Crate 3: `fortivpn-daemon`

### Structure

```
crates/fortivpn-daemon/src/
├── main.rs              # shared entry point
├── ipc.rs               # unchanged
├── vpn.rs               # unchanged
├── profile.rs           # unchanged
├── notification.rs      # unchanged (already no-op)
├── installer.rs         # shared API, delegates to platform
└── platform/
    ├── mod.rs
    ├── macos.rs         # ~60 lines (oslog, launchctl install)
    ├── windows.rs       # ~15 lines (env_logger, no-op install)
    └── linux.rs         # ~15 lines (env_logger, stub install)
```

### Platform API

```rust
pub fn init_logger();
pub fn is_helper_installed() -> bool;
pub fn install_helper() -> Result<(), String>;
```

---

## Crate 4: `fortivpn-helper`

### Structure

```
crates/fortivpn-helper/src/
├── main.rs              # dispatches to platform::run()
├── commands.rs          # shared command handlers (already exists)
└── platform/
    ├── mod.rs
    ├── macos.rs         # launchd socket activation (current unix_main.rs content)
    ├── windows.rs       # print error + exit
    └── linux.rs         # systemd (stub)
```

### Platform API

```rust
pub fn run() -> !;
```

`unix_main.rs` gets renamed to `platform/macos.rs`. `commands.rs` stays shared.

---

## Result

| Metric | Before | After |
|--------|--------|-------|
| `#[cfg]` guards in business logic | 82 | 0 |
| `#[cfg]` in `platform/mod.rs` files | 0 | ~8 (2 per mod.rs × 4 crates) |
| Files with scattered `#[cfg]` | 10 | 0 |
| Platform files | 1 (`unix_main.rs`) | 12 (4 crates × 3 platforms) |
| Adding a new platform | Touch 10+ files | Create 4 files (one per crate) |

## Migration Strategy

Refactor one crate at a time:
1. `fortivpn-app` — most scattered, biggest readability win
2. `fortivpn` (lib) — most complex, helper/routing/tun
3. `fortivpn-daemon` — simple, logger + installer
4. `fortivpn-helper` — rename `unix_main.rs`, formalize

Each crate refactor is a single commit. Tests pass after each commit.
