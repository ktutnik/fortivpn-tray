# Windows VPN Support — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make FortiVPN Tray fully functional on Windows — TUN device creation, VPN tunnel, routing, DNS, and privilege escalation all working.

**Architecture:** Use `tun2` crate's built-in WinTun support for TUN devices. Run the daemon elevated (Option B from the spec — simplest first, Windows Service can come later). Use Windows `route.exe` for routing, `netsh` for DNS, `netsh` for IPv6. No helper daemon needed on Windows — the elevated daemon creates TUN directly.

**Tech Stack:** Rust, `tun2` (WinTun), `tokio`, Windows `route.exe`, `netsh`

**Spec:** `docs/superpowers/specs/windows-support-status.md`

---

## File Structure

### Modified Files
| File | Changes |
|------|---------|
| `crates/fortivpn/src/lib.rs` | Remove `#[cfg(unix)]` from module declarations (lines 1-9). Add `#[cfg(windows)]` `VpnSession::connect()` using tun2 directly. |
| `crates/fortivpn/src/async_tun.rs` | Add `#[cfg(windows)]` `AsyncTunFd` wrapping tun2's `AsyncDevice` with `AsyncRead`/`AsyncWrite`. |
| `crates/fortivpn/src/helper.rs` | Windows `HelperClient` creates TUN directly via `tun2` (no separate helper process). Routes/DNS via `route.exe`/`netsh`. |
| `crates/fortivpn/src/routing.rs` | Implement `get_default_gateway()`, `configure_dns()`, `restore_dns()`, `disable_ipv6()`, `restore_ipv6()` for Windows. |
| `crates/fortivpn-daemon/src/installer.rs` | Windows: `is_helper_installed()` returns true (no helper needed). `install_helper()` is no-op. |

### No New Files
### No Deleted Files

---

## Task 1: Ungating — Make VPN Library Modules Available on Windows

**Files:**
- Modify: `crates/fortivpn/src/lib.rs:1-9`

The core problem: line 1 has `#[cfg(unix)]` wrapping ALL module declarations. This means `auth`, `bridge`, `ppp`, `tunnel`, `routing`, `tun`, and `helper` don't exist on Windows. Most of these are cross-platform (TLS, PPP parsing, framing). Only `async_tun` and parts of `helper` are truly Unix-specific.

- [ ] **Step 1: Remove `#[cfg(unix)]` from module declarations**

In `crates/fortivpn/src/lib.rs`, the modules are currently wrapped:
```rust
#[cfg(unix)]
pub mod async_tun;
pub mod auth;
pub mod bridge;
pub mod helper;
pub mod ppp;
pub mod routing;
pub mod tun;
pub mod tunnel;
```

Change to:
```rust
#[cfg(unix)]
pub mod async_tun;
pub mod auth;
pub mod bridge;
pub mod helper;
pub mod ppp;
pub mod routing;
pub mod tun;
pub mod tunnel;
```

Keep `async_tun` gated — it will get its own Windows impl in Task 2. All other modules should compile on Windows since they use cross-platform Rust (TLS via `rustls`, TCP sockets, byte parsing).

- [ ] **Step 2: Check what `VpnSession::connect()` references**

Read the `#[cfg(unix)]` `connect()` method (lines 92-165). It uses:
- `auth::authenticate()` — cross-platform (TLS + HTTP)
- `tunnel::open_tunnel()` — cross-platform (TLS + HTTP)
- `bridge::start_bridge()` — needs `AsyncTunFd` (Task 2)
- `helper::HelperClient` — has Windows stub (Task 3)
- `routing::RouteManager` — partially implemented (Task 4)
- `async_tun::AsyncTunFd` — Unix only (Task 2)

- [ ] **Step 3: Make `VpnSession::connect()` available on both platforms**

Remove `#[cfg(unix)]` from `connect()`. The function body references `AsyncTunFd` and `HelperClient` — both will be implemented for Windows in later tasks. For now, gate the TUN/bridge section:

```rust
pub async fn connect(/* ... */) -> Result<(), FortiError> {
    // Phase 1: Authentication (cross-platform)
    let (cookie, config) = auth::authenticate(/* ... */).await?;

    // Phase 2: Open TLS tunnel (cross-platform)
    let tls_stream = tunnel::open_tunnel(/* ... */).await?;

    // Phase 3: Create TUN device (platform-specific)
    #[cfg(unix)]
    let tun_fd = {
        let (raw_fd, tun_name) = helper_client.create_tun(/* ... */)?;
        async_tun::AsyncTunFd::new(raw_fd)?
    };

    #[cfg(windows)]
    let tun_fd = {
        // tun2 creates TUN directly on Windows (no helper needed)
        let dev = tun::create_tun(config.assigned_ip, config.peer_ip, config.mtu)?;
        dev // AsyncDevice already implements AsyncRead/AsyncWrite
    };

    // Phase 4: Bridge + routing (cross-platform with platform TUN)
    // ...
}
```

Actually, the exact implementation depends on how `bridge::start_bridge()` is parameterized. Read it to determine the right abstraction.

- [ ] **Step 4: Build on macOS to verify nothing is broken**

```bash
cargo build --workspace && cargo test --workspace
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: ungate VPN library modules for Windows compilation"
```

---

## Task 2: Windows AsyncTunFd — Wrap tun2 AsyncDevice

**Files:**
- Modify: `crates/fortivpn/src/async_tun.rs`

On Unix, `AsyncTunFd` wraps a raw file descriptor with tokio's `AsyncFd`. On Windows, `tun2::AsyncDevice` already implements `tokio::io::AsyncRead` and `tokio::io::AsyncWrite`. We need a thin wrapper.

- [ ] **Step 1: Read the bridge module to understand what AsyncTunFd needs**

Read `crates/fortivpn/src/bridge.rs` to see how `AsyncTunFd` is used. It likely does:
```rust
tokio::io::copy(&mut tun_reader, &mut tls_writer)
```
So we need `AsyncRead + AsyncWrite`.

- [ ] **Step 2: Add `#[cfg(windows)]` AsyncTunFd wrapping tun2::AsyncDevice**

In `crates/fortivpn/src/async_tun.rs`, add after the `#[cfg(unix)]` block:

```rust
#[cfg(windows)]
mod platform {
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tun2::AsyncDevice;

    pub struct AsyncTunFd {
        inner: AsyncDevice,
    }

    impl AsyncTunFd {
        pub fn from_device(dev: AsyncDevice) -> io::Result<Self> {
            Ok(Self { inner: dev })
        }
    }

    impl AsyncRead for AsyncTunFd {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for AsyncTunFd {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }
}

#[cfg(windows)]
pub use platform::AsyncTunFd;
```

- [ ] **Step 3: Build and test on macOS**

```bash
cargo build --workspace && cargo test --workspace
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(tun): add Windows AsyncTunFd wrapping tun2::AsyncDevice"
```

---

## Task 3: Windows HelperClient — Direct TUN Creation (No Helper Process)

**Files:**
- Modify: `crates/fortivpn/src/helper.rs:227-298`

On Windows, there's no separate helper daemon. The daemon itself runs elevated and creates TUN directly via `tun2`. The `HelperClient` on Windows becomes a thin wrapper that calls `tun2` and `route.exe`/`netsh` directly.

- [ ] **Step 1: Replace Windows HelperClient stub with real implementation**

Replace the `#[cfg(windows)]` block (lines 227-298) with:

```rust
#[cfg(windows)]
mod platform {
    use crate::FortiError;
    use std::net::Ipv4Addr;

    pub struct HelperClient {
        tun_name: Option<String>,
    }

    impl HelperClient {
        pub fn connect() -> Result<Self, FortiError> {
            // No helper process on Windows — daemon creates TUN directly
            Ok(Self { tun_name: None })
        }

        pub fn version(&mut self) -> Result<String, FortiError> {
            Ok("windows-direct".to_string())
        }

        pub fn create_tun(
            &mut self,
            ip: Ipv4Addr,
            peer_ip: Ipv4Addr,
            mtu: u16,
        ) -> Result<(tun2::AsyncDevice, String), FortiError> {
            let dev = crate::tun::create_tun(ip, peer_ip, mtu)?;
            let name = crate::tun::device_name(&dev);
            self.tun_name = Some(name.clone());
            Ok((dev, name))
        }

        pub fn destroy_tun(&mut self) -> Result<(), FortiError> {
            self.tun_name = None;
            Ok(()) // tun2 cleans up on drop
        }

        pub fn add_route(&mut self, dest: &str, gateway: &str) -> Result<(), FortiError> {
            let output = std::process::Command::new("route")
                .args(["ADD", dest, gateway])
                .output()
                .map_err(|e| FortiError::RoutingError(format!("route ADD: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(FortiError::RoutingError(format!("route ADD {dest}: {stderr}")));
            }
            Ok(())
        }

        pub fn delete_route(&mut self, dest: &str) -> Result<(), FortiError> {
            let output = std::process::Command::new("route")
                .args(["DELETE", dest])
                .output()
                .map_err(|e| FortiError::RoutingError(format!("route DELETE: {e}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(FortiError::RoutingError(format!("route DELETE {dest}: {stderr}")));
            }
            Ok(())
        }

        pub fn configure_dns(&mut self, config: &crate::VpnConfig) -> Result<(), FortiError> {
            if let Some(tun_name) = &self.tun_name {
                for dns in &config.dns_servers {
                    let _ = std::process::Command::new("netsh")
                        .args(["interface", "ip", "set", "dns", tun_name, "static", &dns.to_string()])
                        .output();
                }
            }
            Ok(())
        }

        pub fn restore_dns(&mut self) -> Result<(), FortiError> {
            // DNS is automatically removed when TUN adapter is destroyed
            Ok(())
        }

        pub fn ping(&mut self) -> Result<(), FortiError> {
            Ok(())
        }

        pub fn shutdown(&mut self) {}
    }
}
```

**Note:** The `create_tun` return type changes on Windows — it returns `(AsyncDevice, String)` instead of `(RawFd, String)`. This requires adjusting how `lib.rs` calls it. On Unix, a raw fd is passed to `AsyncTunFd::new(fd)`. On Windows, the `AsyncDevice` is passed to `AsyncTunFd::from_device(dev)`.

- [ ] **Step 2: Adjust `VpnSession::connect()` for the different return type**

In `crates/fortivpn/src/lib.rs`, the connect method needs platform branches:

```rust
// Unix: helper returns raw fd, wrap in AsyncTunFd
#[cfg(unix)]
let async_tun = {
    let (fd, tun_name) = helper_client.create_tun(ip, peer_ip, mtu)?;
    async_tun::AsyncTunFd::new(fd)?
};

// Windows: helper returns AsyncDevice directly, wrap in AsyncTunFd
#[cfg(windows)]
let (async_tun, _tun_name) = {
    let (dev, tun_name) = helper_client.create_tun(ip, peer_ip, mtu)?;
    (async_tun::AsyncTunFd::from_device(dev)?, tun_name)
};
```

- [ ] **Step 3: Build on macOS**

```bash
cargo build --workspace && cargo test --workspace
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(helper): Windows HelperClient — direct TUN creation via tun2, route/DNS via shell commands"
```

---

## Task 4: Windows Routing — get_default_gateway, DNS, IPv6

**Files:**
- Modify: `crates/fortivpn/src/routing.rs`

- [ ] **Step 1: Implement `get_default_gateway()` for Windows**

Replace the Windows stub (line 271-274) with:

```rust
#[cfg(target_os = "windows")]
{
    // Parse output of: route print 0.0.0.0
    let output = Command::new("route")
        .args(["print", "0.0.0.0"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Look for line: "0.0.0.0  0.0.0.0  <gateway>  <interface>  <metric>"
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 && parts[0] == "0.0.0.0" && parts[1] == "0.0.0.0" {
            return parts[2].parse().ok();
        }
    }
    None
}
```

- [ ] **Step 2: Implement `configure_dns()` for Windows**

Add after the macOS block:

```rust
#[cfg(target_os = "windows")]
{
    for dns in &config.dns_servers {
        let _ = Command::new("netsh")
            .args([
                "interface", "ip", "set", "dns",
                tun_name, "static", &dns.to_string(),
            ])
            .output();
    }
    Ok(())
}
```

- [ ] **Step 3: Implement `restore_dns()` for Windows**

```rust
#[cfg(target_os = "windows")]
{
    // DNS settings are removed when adapter is destroyed
    Ok(())
}
```

- [ ] **Step 4: Implement `disable_ipv6()` for Windows**

```rust
#[cfg(target_os = "windows")]
{
    let mut disabled = Vec::new();
    // Disable IPv6 on the VPN adapter
    if let Some(tun) = tun_name {
        let output = Command::new("netsh")
            .args(["interface", "ipv6", "set", "interface", tun, "disabled"])
            .output();
        if output.is_ok() {
            disabled.push(tun.to_string());
        }
    }
    disabled
}
```

- [ ] **Step 5: Implement `restore_ipv6()` for Windows**

```rust
#[cfg(target_os = "windows")]
{
    for iface in interfaces {
        let _ = Command::new("netsh")
            .args(["interface", "ipv6", "set", "interface", iface, "enabled"])
            .output();
    }
}
```

- [ ] **Step 6: Build and test**

```bash
cargo build --workspace && cargo test --workspace
```

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(routing): implement Windows get_default_gateway, DNS, IPv6 via netsh/route"
```

---

## Task 5: Windows Installer — No-Op (No Helper Needed)

**Files:**
- Modify: `crates/fortivpn-daemon/src/installer.rs`

- [ ] **Step 1: Add Windows stubs**

Add `#[cfg]` blocks:

```rust
#[cfg(windows)]
pub fn is_helper_installed() -> bool {
    // No helper on Windows — daemon creates TUN directly
    true
}

#[cfg(windows)]
pub fn install_helper() -> Result<(), String> {
    // No helper needed on Windows
    Ok(())
}
```

Wrap existing macOS code in `#[cfg(target_os = "macos")]`.

Add Linux stubs too:
```rust
#[cfg(target_os = "linux")]
pub fn is_helper_installed() -> bool {
    // TODO: check systemd service
    false
}

#[cfg(target_os = "linux")]
pub fn install_helper() -> Result<(), String> {
    Err("Linux helper installation not yet implemented".to_string())
}
```

- [ ] **Step 2: Build and test**

```bash
cargo build --workspace && cargo test --workspace
```

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(installer): platform-conditional installer — Windows no-op, macOS launchd, Linux stub"
```

---

## Task 6: Wire VpnSession::connect() for Windows

**Files:**
- Modify: `crates/fortivpn/src/lib.rs`

This is the final wiring task — making `VpnSession::connect()` work on Windows by connecting all the pieces from Tasks 1-5.

- [ ] **Step 1: Read the current Unix `connect()` implementation**

Read lines 92-165 of `lib.rs` to understand the full flow.

- [ ] **Step 2: Refactor `connect()` to be cross-platform**

The flow is:
1. `auth::authenticate()` — already cross-platform
2. `tunnel::open_tunnel()` — already cross-platform
3. `HelperClient::create_tun()` — now cross-platform (Task 3)
4. `AsyncTunFd` — now cross-platform (Task 2)
5. `bridge::start_bridge()` — needs `AsyncRead + AsyncWrite`, which AsyncTunFd provides on both platforms
6. `RouteManager` — now cross-platform (Task 4)

The main difference is:
- Unix: `create_tun()` returns `(RawFd, String)` → `AsyncTunFd::new(fd)`
- Windows: `create_tun()` returns `(AsyncDevice, String)` → `AsyncTunFd::from_device(dev)`

Use `#[cfg]` blocks only for the TUN creation section. Everything else should be shared.

- [ ] **Step 3: Remove `#[cfg(unix)]` from `connect()`**

Make the function available on all platforms with platform-conditional TUN creation inside.

- [ ] **Step 4: Build and test on macOS**

```bash
cargo build --workspace && cargo test --workspace
```

Verify all 270 tests still pass.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: wire VpnSession::connect() for Windows — cross-platform VPN connection flow"
```

---

## Task 7: Fix Compiler Warnings on Windows

**Files:**
- Modify: `crates/fortivpn/src/routing.rs`
- Modify: `crates/fortivpn-helper/src/commands.rs`
- Modify: `crates/fortivpn-daemon/src/vpn.rs`

- [ ] **Step 1: Fix unused function warnings in routing.rs**

Functions like `parse_gateway_output()`, `build_dns_script()` are macOS-only helpers. Gate them with `#[cfg(target_os = "macos")]`.

- [ ] **Step 2: Fix unused code in helper commands.rs**

Gate helper-specific functions with `#[cfg(unix)]` where appropriate.

- [ ] **Step 3: Fix unused variants in vpn.rs**

If `VpnStatus::Connecting` and `VpnStatus::Error` are now used (because `connect()` works on Windows), the warnings should disappear. If not, allow the warnings temporarily.

- [ ] **Step 4: Build with all warnings as errors**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "fix: resolve Windows compiler warnings — platform-gate unused functions"
```

---

## Task 8: Verify on Windows

- [ ] **Step 1: Build on Windows**

```bash
cargo build --release --workspace
```

Should compile with zero errors.

- [ ] **Step 2: Run daemon and CLI**

```bash
.\target\release\fortivpn-daemon.exe &
.\target\release\fortivpn.exe status
.\target\release\fortivpn.exe list
```

- [ ] **Step 3: Test VPN connection (requires admin)**

Run PowerShell as Administrator:
```powershell
.\target\release\fortivpn-daemon.exe
# In another admin terminal:
.\target\release\fortivpn.exe connect "MIMS SG"
```

Verify:
- TUN adapter appears in `ipconfig /all`
- Routes added: `route print`
- DNS configured: `ipconfig /all` shows VPN DNS
- Traffic flows through VPN

- [ ] **Step 4: Test disconnect**

```bash
.\target\release\fortivpn.exe disconnect
```

Verify:
- TUN adapter removed
- Routes cleaned up
- DNS restored

- [ ] **Step 5: Commit final**

```bash
git add -A && git commit -m "test: verify Windows VPN connection end-to-end"
```
