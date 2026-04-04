# Platform Module Refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace 82 scattered `#[cfg]` guards with compile-time platform modules — one file per OS per crate. Zero `#[cfg]` in business logic.

**Architecture:** Each crate gets `src/platform/{mod.rs, macos.rs, windows.rs, linux.rs}`. `mod.rs` has the only `#[cfg]` blocks (on mod declarations + re-exports). Original files keep shared logic and call `platform::*`. Each platform file exports identical function signatures.

**Tech Stack:** Rust conditional compilation, same dependencies as current.

**Spec:** `docs/superpowers/specs/2026-04-03-platform-module-refactor-design.md`

---

## File Structure

### Task 1: `fortivpn-app` (the tray app)

| Action | File |
|--------|------|
| Create | `crates/fortivpn-app/src/platform/mod.rs` |
| Create | `crates/fortivpn-app/src/platform/macos.rs` |
| Create | `crates/fortivpn-app/src/platform/windows.rs` |
| Create | `crates/fortivpn-app/src/platform/linux.rs` |
| Modify | `crates/fortivpn-app/src/main.rs` — remove all `#[cfg]`, call `platform::*` |
| Delete | `crates/fortivpn-app/src/notification.rs` — content moves to platform files |

### Task 2: `fortivpn` (VPN library)

| Action | File |
|--------|------|
| Create | `crates/fortivpn/src/platform/mod.rs` |
| Create | `crates/fortivpn/src/platform/macos.rs` |
| Create | `crates/fortivpn/src/platform/windows.rs` |
| Create | `crates/fortivpn/src/platform/linux.rs` |
| Modify | `crates/fortivpn/src/helper.rs` — thin wrapper delegating to `platform::HelperClientInner` |
| Modify | `crates/fortivpn/src/routing.rs` — shared logic only, delegates to `platform::*` |
| Modify | `crates/fortivpn/src/async_tun.rs` — re-export `platform::AsyncTunFd` |
| Modify | `crates/fortivpn/src/tun.rs` — delegate `device_name` to `platform::device_name` |
| Modify | `crates/fortivpn/src/lib.rs` — `VpnSession::connect` uses `platform::TunHandle` |

### Task 3: `fortivpn-daemon`

| Action | File |
|--------|------|
| Create | `crates/fortivpn-daemon/src/platform/mod.rs` |
| Create | `crates/fortivpn-daemon/src/platform/macos.rs` |
| Create | `crates/fortivpn-daemon/src/platform/windows.rs` |
| Create | `crates/fortivpn-daemon/src/platform/linux.rs` |
| Modify | `crates/fortivpn-daemon/src/main.rs` — call `platform::init_logger()` |
| Modify | `crates/fortivpn-daemon/src/installer.rs` — delegate to `platform::*` |

### Task 4: `fortivpn-helper`

| Action | File |
|--------|------|
| Create | `crates/fortivpn-helper/src/platform/mod.rs` |
| Create | `crates/fortivpn-helper/src/platform/macos.rs` |
| Create | `crates/fortivpn-helper/src/platform/windows.rs` |
| Create | `crates/fortivpn-helper/src/platform/linux.rs` |
| Rename | `crates/fortivpn-helper/src/unix_main.rs` → content moves to `platform/macos.rs` |
| Modify | `crates/fortivpn-helper/src/main.rs` — just calls `platform::run()` |

---

## Task 1: Refactor `fortivpn-app` into Platform Modules

**Files:**
- Create: `crates/fortivpn-app/src/platform/mod.rs`
- Create: `crates/fortivpn-app/src/platform/macos.rs`
- Create: `crates/fortivpn-app/src/platform/windows.rs`
- Create: `crates/fortivpn-app/src/platform/linux.rs`
- Modify: `crates/fortivpn-app/src/main.rs`
- Delete: `crates/fortivpn-app/src/notification.rs`

- [ ] **Step 1: Read all source files**

Read these files completely before making changes:
- `crates/fortivpn-app/src/main.rs`
- `crates/fortivpn-app/src/notification.rs`

Understand every `#[cfg]` block — you'll be moving each one to the appropriate platform file.

- [ ] **Step 2: Create `platform/mod.rs`**

Create `crates/fortivpn-app/src/platform/mod.rs`:

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

- [ ] **Step 3: Create `platform/macos.rs`**

Create `crates/fortivpn-app/src/platform/macos.rs` containing:

1. `pub fn init()` — no-op (macOS doesn't need pre-init)
2. `pub fn dispatch_to_main(f: fn())` — move the GCD `dispatch_async_f` code from `main.rs` lines 155-176
3. `pub fn ensure_daemon(daemon_dir: &std::path::Path)` — move the Unix daemon spawn from `main.rs` lines 496-506 (Command::new + spawn)
4. `pub fn hide_from_dock(cx: &mut gpui::App)` — move the NSApplication Accessory code from `main.rs` lines 68-75
5. `pub fn show_notification(title: &str, body: &str)` — move the osascript code from `notification.rs` lines 3-11
6. `pub fn set_tray_icon(tray: &tray_icon::TrayIcon, icon: tray_icon::Icon)` — call `tray.set_icon_with_as_template(Some(icon), true)`
7. `pub fn create_keepalive_window(_cx: &mut gpui::App)` — no-op on macOS

Include all necessary imports at the top of the file. The `dispatch_to_main` function uses raw FFI for `dispatch_async_f` and `_dispatch_main_q` — copy the entire implementation including the `extern "C"` block and trampoline function.

- [ ] **Step 4: Create `platform/windows.rs`**

Create `crates/fortivpn-app/src/platform/windows.rs` containing:

1. The entire `win_dispatch` module (from `main.rs` lines 190-304) as a private submodule or inline
2. `pub fn init()` — call `win_dispatch::init()`
3. `pub fn dispatch_to_main(f: fn())` — call `win_dispatch::post(f)`
4. `pub fn ensure_daemon(daemon_dir: &std::path::Path)` — move `ShellExecuteW` runas code from `main.rs` lines 508-527
5. `pub fn hide_from_dock(_cx: &mut gpui::App)` — no-op
6. `pub fn show_notification(title: &str, body: &str)` — move `notify_rust` code from `notification.rs` lines 14-20
7. `pub fn set_tray_icon(tray: &tray_icon::TrayIcon, icon: tray_icon::Icon)` — call `tray.set_icon(Some(icon))`
8. `pub fn create_keepalive_window(cx: &mut gpui::App)` — move the hidden window + HiddenView code from `main.rs` lines 50-65 and 413-425

Include `use std::os::windows::ffi::OsStrExt;` and the `windows_sys` import for `ShellExecuteW`.

- [ ] **Step 5: Create `platform/linux.rs`**

Create `crates/fortivpn-app/src/platform/linux.rs` containing:

1. `pub fn init()` — no-op
2. `pub fn dispatch_to_main(f: fn())` — just call `f()` directly
3. `pub fn ensure_daemon(daemon_dir: &std::path::Path)` — same as macOS: `Command::new(daemon_dir.join("fortivpn-daemon")).spawn()`
4. `pub fn hide_from_dock(_cx: &mut gpui::App)` — no-op
5. `pub fn show_notification(title: &str, body: &str)` — same as Windows: `notify_rust::Notification`
6. `pub fn set_tray_icon(tray: &tray_icon::TrayIcon, icon: tray_icon::Icon)` — call `tray.set_icon(Some(icon))`
7. `pub fn create_keepalive_window(_cx: &mut gpui::App)` — no-op

- [ ] **Step 6: Rewrite `main.rs` — remove ALL `#[cfg]` blocks**

Replace `main.rs` to use `platform::*` for all platform-specific code:

- Remove `#[cfg(unix)] use std::process::Command;` — not needed, platform handles it
- Remove the `dispatch_to_main` function definitions — now in platform
- Remove the `win_dispatch` module — now in `platform/windows.rs`
- Remove the `HiddenView` struct — now in `platform/windows.rs`
- Remove `#[cfg]` blocks in `refresh_icon` — call `platform::set_tray_icon()`
- Remove `#[cfg]` blocks in `ensure_daemon` — call `platform::ensure_daemon()`
- Remove `#[cfg]` blocks in app.run callback — call `platform::hide_from_dock()` and `platform::create_keepalive_window()`
- Keep `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]` at the top — this is a crate-level attribute, not business logic
- Add `mod platform;` declaration

The `ensure_daemon` function becomes:
```rust
fn ensure_daemon() {
    if ipc_client::is_daemon_running() { return; }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            platform::ensure_daemon(dir);
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
}
```

The `refresh_icon` function becomes:
```rust
fn refresh_icon() {
    let is_connected = get_cached_connected();
    let icon_bytes = if is_connected {
        include_bytes!("../../../icons/vpn-connected.png").as_slice()
    } else {
        include_bytes!("../../../icons/vpn-disconnected.png").as_slice()
    };
    if let Ok(guard) = TRAY.lock() {
        if let Some(holder) = guard.as_ref() {
            if let Ok(icon) = load_icon(icon_bytes) {
                platform::set_tray_icon(&holder.0, icon);
            }
        }
    }
}
```

The `handle_menu_event` for "connect:" notification:
```rust
notification::show(...) → platform::show_notification(...)
```

- [ ] **Step 7: Delete `notification.rs`**

Remove `crates/fortivpn-app/src/notification.rs` — its content is now in each platform file. Remove `mod notification;` from `main.rs`.

- [ ] **Step 8: Build and test**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Fix any issues. The key check: `main.rs` should have ZERO `#[cfg(target_os)]` or `#[cfg(unix)]` or `#[cfg(windows)]` blocks (except the crate-level `cfg_attr` on line 2).

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "refactor(app): extract platform-specific code into platform/ modules — zero #[cfg] in main.rs"
```

---

## Task 2: Refactor `fortivpn` Library into Platform Modules

**Files:**
- Create: `crates/fortivpn/src/platform/mod.rs`
- Create: `crates/fortivpn/src/platform/macos.rs`
- Create: `crates/fortivpn/src/platform/windows.rs`
- Create: `crates/fortivpn/src/platform/linux.rs`
- Modify: `crates/fortivpn/src/helper.rs`
- Modify: `crates/fortivpn/src/routing.rs`
- Modify: `crates/fortivpn/src/async_tun.rs`
- Modify: `crates/fortivpn/src/tun.rs`
- Modify: `crates/fortivpn/src/lib.rs`

- [ ] **Step 1: Read all source files**

Read completely:
- `crates/fortivpn/src/helper.rs`
- `crates/fortivpn/src/routing.rs`
- `crates/fortivpn/src/async_tun.rs`
- `crates/fortivpn/src/tun.rs`
- `crates/fortivpn/src/lib.rs`

- [ ] **Step 2: Create `platform/mod.rs`**

Create `crates/fortivpn/src/platform/mod.rs`:

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

Add `pub mod platform;` to `lib.rs`.

- [ ] **Step 3: Create `platform/macos.rs`**

Move from existing files:

**From `helper.rs` (Unix platform module, lines 9-223):**
- The entire Unix `HelperClient` implementation → rename to `HelperClientInner`
- Include `recv_fd()` function
- `create_tun` returns `(RawFd, String)`

**From `routing.rs`:**
- `run_route()` macOS block (lines 168-186) — calls `/sbin/route`
- `get_default_gateway()` macOS block (lines 249-257) — calls `route -n get default`
- `parse_gateway_output()` helper (lines 237-246)
- `configure_dns()` macOS block (lines 309-332) — calls `scutil`
- `build_dns_script()` helper (lines 293-302)
- `restore_dns()` macOS block (lines 360-373) — calls `scutil`
- `disable_ipv6()` macOS block (lines 389-398) — calls `networksetup`
- `restore_ipv6()` macOS block (lines 413-418)
- `get_ipv6_active_interfaces()` (lines 428-460)

**From `async_tun.rs` (Unix block, lines 6-117):**
- `AsyncTunFd` struct + `AsyncRead`/`AsyncWrite` impls using `libc`

**From `tun.rs` (Unix block, lines 24-27):**
- `pub fn device_name(dev: &tun2::AsyncDevice) -> String` using `dev.as_ref().tun_name()`

**Define type alias:**
```rust
pub type TunHandle = (std::os::fd::RawFd, String);
```

- [ ] **Step 4: Create `platform/windows.rs`**

Move from existing files:

**From `helper.rs` (Windows platform module, lines 230-366):**
- The entire Windows `HelperClient` → rename to `HelperClientInner`
- Uses `crate::silent_cmd` for `route.exe` and `netsh`
- `create_tun` returns `(tun2::AsyncDevice, String)`

**From `routing.rs`:**
- `run_route()` Windows block (lines 214-231) — calls `silent_cmd("route")`
- `get_default_gateway()` Windows block (lines 276-291)
- `configure_dns()` Windows block (lines 334-350)
- `restore_dns()` Windows block (lines 375-378) — no-op
- `disable_ipv6()` Windows block (lines 405-408) — returns empty vec
- `restore_ipv6()` Windows block (lines 420-425) — no-op

**From `async_tun.rs` (Windows block, lines 124-171):**
- `AsyncTunFd` struct wrapping `tun2::AsyncDevice`

**From `tun.rs` (Windows block, lines 28-32):**
- `pub fn device_name(dev: &tun2::AsyncDevice) -> String` using `tun2::AbstractDevice`

**Define type alias:**
```rust
pub type TunHandle = (tun2::AsyncDevice, String);
```

- [ ] **Step 5: Create `platform/linux.rs`**

Similar to macOS (Unix-based) with these differences:

**Helper:** Same as macOS — Unix socket, SCM_RIGHTS. Copy from macOS, same code.

**Routing:**
- `run_route()` — calls `ip route add/del` (from routing.rs lines 188-212)
- `get_default_gateway()` — calls `ip route show default` (lines 259-274)
- `configure_dns()` — no-op (TODO)
- `restore_dns()` — no-op
- `disable_ipv6()` — returns empty vec
- `restore_ipv6()` — no-op

**AsyncTunFd:** Same as macOS (Unix fd-based).

**device_name:** Same as macOS.

**TunHandle:** `(std::os::fd::RawFd, String)` — same as macOS.

- [ ] **Step 6: Rewrite `helper.rs` as thin wrapper**

```rust
use crate::platform;
use crate::FortiError;

pub struct HelperClient {
    inner: platform::HelperClientInner,
}

impl HelperClient {
    pub fn connect() -> Result<Self, FortiError> {
        Ok(Self { inner: platform::HelperClientInner::connect()? })
    }
    pub fn version(&mut self) -> Result<String, FortiError> {
        self.inner.version()
    }
    pub fn create_tun(&mut self, ip: std::net::Ipv4Addr, peer_ip: std::net::Ipv4Addr, mtu: u16) -> Result<platform::TunHandle, FortiError> {
        self.inner.create_tun(ip, peer_ip, mtu)
    }
    pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
        self.inner.destroy_tun()
    }
    pub fn add_route(&mut self, dest: &str, gateway: &str) -> Result<(), FortiError> {
        self.inner.add_route(dest, gateway)
    }
    pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
        self.inner.delete_route(dest)
    }
    pub fn configure_dns(&mut self, config: &crate::VpnConfig) -> Result<(), FortiError> {
        self.inner.configure_dns(config)
    }
    pub fn restore_dns(&mut self) -> Result<(), FortiError> {
        self.inner.restore_dns()
    }
    pub fn ping(&mut self) -> Result<(), FortiError> {
        self.inner.ping()
    }
    pub fn shutdown(&mut self) {
        self.inner.shutdown()
    }
}
```

- [ ] **Step 7: Rewrite `routing.rs` — shared logic only**

Keep:
- `RouteManager` struct and its methods (`install_routes`, `uninstall_routes`, etc.)
- Route tracking logic (split tunnel vs full tunnel)

Replace platform calls with `platform::*`:
```rust
use crate::platform;

// In install_routes:
platform::run_route("add", &dest, &gateway)?;

// In uninstall_routes:
platform::run_route("delete", &dest, "")?;
```

Remove all `#[cfg(target_os = "...")]` blocks — they're now in platform files.
Remove the `#[cfg(not(target_os = "windows"))] use std::process::Command;` — not needed.
Remove the `#[cfg(target_os = "windows")] use crate::silent_cmd;` — not needed.
Remove `parse_gateway_output`, `build_dns_script`, `get_ipv6_active_interfaces` — moved to platform.

The macOS-gated tests stay in routing.rs but with `#[cfg(target_os = "macos")]` on the test functions — this is acceptable since tests are for specific platform behavior.

- [ ] **Step 8: Rewrite `async_tun.rs`**

Replace entire file with:
```rust
pub use crate::platform::AsyncTunFd;
```

- [ ] **Step 9: Rewrite `tun.rs`**

Remove `#[cfg]` blocks from `device_name`:
```rust
pub fn device_name(dev: &tun2::AsyncDevice) -> String {
    crate::platform::device_name(dev)
}
```

Keep `create_tun` with the macOS `platform_config` as-is (it's a tiny `#[cfg]` that's specific to tun2 config, not platform abstraction).

- [ ] **Step 10: Update `lib.rs` VpnSession::connect**

Replace the `#[cfg(unix)]` / `#[cfg(windows)]` TUN creation blocks with:
```rust
let (tun_handle, tun_name) = helper_client.create_tun(config.assigned_ip, peer_ip, config.mtu)?;
let async_tun = platform::AsyncTunFd::from_handle(tun_handle)?;
```

Add `from_handle` to each platform's `AsyncTunFd`:
- macOS/Linux: `pub fn from_handle(handle: TunHandle) -> io::Result<Self>` — calls `Self::new(handle.0)` (raw fd)
- Windows: `pub fn from_handle(handle: TunHandle) -> io::Result<Self>` — calls `Self::from_device(handle.0)`

- [ ] **Step 11: Build and test**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Verify: `helper.rs`, `routing.rs`, `async_tun.rs`, `tun.rs`, `lib.rs` have zero `#[cfg(target_os)]` or `#[cfg(unix)]` or `#[cfg(windows)]` guards (except the tun2 platform_config in `tun.rs` which is fine to keep).

- [ ] **Step 12: Commit**

```bash
git add -A && git commit -m "refactor(vpn): extract platform code into platform/ modules — helper, routing, async_tun, tun"
```

---

## Task 3: Refactor `fortivpn-daemon` into Platform Modules

**Files:**
- Create: `crates/fortivpn-daemon/src/platform/mod.rs`
- Create: `crates/fortivpn-daemon/src/platform/macos.rs`
- Create: `crates/fortivpn-daemon/src/platform/windows.rs`
- Create: `crates/fortivpn-daemon/src/platform/linux.rs`
- Modify: `crates/fortivpn-daemon/src/main.rs`
- Modify: `crates/fortivpn-daemon/src/installer.rs`

- [ ] **Step 1: Read source files**

Read:
- `crates/fortivpn-daemon/src/main.rs`
- `crates/fortivpn-daemon/src/installer.rs`

- [ ] **Step 2: Create platform files**

`platform/mod.rs` — same pattern as Task 1.

`platform/macos.rs`:
```rust
pub fn init_logger() {
    // Move oslog init from main.rs lines 14-22
}

pub fn is_helper_installed() -> bool {
    // Move from installer.rs lines 16-20
}

pub fn needs_upgrade() -> bool {
    // Move from installer.rs lines 23-33
}

pub fn install_helper() -> Result<(), String> {
    // Move from installer.rs lines 36-77
}
```

Include the helper constants (socket path, install path, plist path) and helper functions (`find_bundled_helper`, `find_bundled_plist`) from installer.rs.

`platform/windows.rs`:
```rust
pub fn init_logger() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}

pub fn is_helper_installed() -> bool { true }
pub fn needs_upgrade() -> bool { false }
pub fn install_helper() -> Result<(), String> { Ok(()) }
```

`platform/linux.rs`:
```rust
pub fn init_logger() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}

pub fn is_helper_installed() -> bool { false }
pub fn needs_upgrade() -> bool { false }
pub fn install_helper() -> Result<(), String> {
    Err("Linux helper installation not yet implemented".to_string())
}
```

- [ ] **Step 3: Update `main.rs` and `installer.rs`**

`main.rs` — replace `init_logger()` with `platform::init_logger()`. Remove `#[cfg]` blocks.

`installer.rs` — delegate to `platform::*`:
```rust
mod platform; // or use crate::platform if declared in main

pub fn is_helper_installed() -> bool { platform::is_helper_installed() }
pub fn install_helper() -> Result<(), String> { platform::install_helper() }
```

- [ ] **Step 4: Build and test**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor(daemon): extract platform code into platform/ modules — logger, installer"
```

---

## Task 4: Refactor `fortivpn-helper` into Platform Modules

**Files:**
- Create: `crates/fortivpn-helper/src/platform/mod.rs`
- Create: `crates/fortivpn-helper/src/platform/macos.rs`
- Create: `crates/fortivpn-helper/src/platform/windows.rs`
- Create: `crates/fortivpn-helper/src/platform/linux.rs`
- Delete: `crates/fortivpn-helper/src/unix_main.rs`
- Modify: `crates/fortivpn-helper/src/main.rs`

- [ ] **Step 1: Read source files**

Read:
- `crates/fortivpn-helper/src/main.rs`
- `crates/fortivpn-helper/src/unix_main.rs`

- [ ] **Step 2: Create platform files**

`platform/mod.rs` — same pattern.

`platform/macos.rs` — move entire contents of `unix_main.rs`:
```rust
pub fn run() -> ! {
    // Content from unix_main.rs: launchd socket activation, accept loop, handle_client, etc.
}
```

`platform/linux.rs` — copy from macOS (same Unix socket code):
```rust
pub fn run() -> ! {
    // Same as macOS for now (Unix sockets, SCM_RIGHTS)
    // TODO: systemd socket activation instead of launchd
    super::macos::run() // or duplicate the code
}
```

Actually, since macOS and Linux share Unix code, create a shared `unix` helper or just duplicate. Simplest: Linux re-uses the same code by having `platform/linux.rs` import from a shared unix module. But per the spec, each platform is one file. So duplicate the Unix helper code into both `macos.rs` and `linux.rs`, or factor out a `unix_common.rs` that both import. Use your judgment — if the code is identical, a shared module is better.

`platform/windows.rs`:
```rust
pub fn run() -> ! {
    eprintln!("Windows helper not yet implemented");
    eprintln!("The daemon creates TUN devices directly on Windows.");
    std::process::exit(1);
}
```

- [ ] **Step 3: Rewrite `main.rs`**

```rust
#[cfg(unix)]
mod commands;
mod platform;

fn main() {
    platform::run();
}
```

Delete `unix_main.rs`.

- [ ] **Step 4: Build and test**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor(helper): extract platform code into platform/ modules — formalize unix_main as platform/macos"
```

---

## Task 5: Final Verification

- [ ] **Step 1: Count remaining `#[cfg]` guards**

```bash
grep -rn "#\[cfg(target_os\|#\[cfg(unix\|#\[cfg(windows\|#\[cfg(not(target_os" crates/*/src/*.rs crates/*/src/**/*.rs --include="*.rs" | grep -v "platform/" | grep -v "test" | grep -v "cfg_attr" | wc -l
```

Expected: 0 (or near-zero — only the `tun.rs` platform_config and crate-level `cfg_attr` for windows_subsystem).

- [ ] **Step 2: Verify all tests pass**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Expected: 270 tests pass, zero warnings.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test: verify platform module refactor — zero cfg guards in business logic"
```
