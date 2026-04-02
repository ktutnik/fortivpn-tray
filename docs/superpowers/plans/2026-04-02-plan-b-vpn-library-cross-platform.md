# Plan B: VPN Library Cross-Platform — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `crates/fortivpn/` compile on macOS, Linux, and Windows by adding `#[cfg]` guards for platform-specific code and Windows stubs where needed.

**Architecture:** Keep all existing macOS/Linux code behind `#[cfg(unix)]`. Add `#[cfg(windows)]` stubs for helper communication (named pipes), TUN async wrapper (wintun), and routing commands (`route.exe`, `netsh`). The stubs return errors with "not yet implemented" for now — full Windows VPN functionality comes in a later plan. The goal here is **compilation on all platforms**.

**Tech Stack:** Rust, `#[cfg]` target conditionals, `tun2` (already supports Windows via wintun)

---

## File Structure

### Modified Files
| File | Changes |
|------|---------|
| `crates/fortivpn/src/helper.rs` | Wrap Unix socket + SCM_RIGHTS in `#[cfg(unix)]`. Add `#[cfg(windows)]` stub that returns error. |
| `crates/fortivpn/src/async_tun.rs` | Wrap `libc`/`OwnedFd`/`AsyncFd` in `#[cfg(unix)]`. Add `#[cfg(windows)]` stub. |
| `crates/fortivpn/src/routing.rs` | Wrap `run_route`/DNS/IPv6 commands in `#[cfg]` per platform. Add Windows stubs. |
| `crates/fortivpn/src/lib.rs` | Conditional import of `async_tun` module. |
| `crates/fortivpn/Cargo.toml` | Make `libc` dependency unix-only. |

### No New Files
### No Deleted Files

---

## Task 1: Helper Client — Platform Conditional

**Files:**
- Modify: `crates/fortivpn/src/helper.rs`

- [ ] **Step 1: Wrap entire HelperClient in `#[cfg(unix)]`**

The `HelperClient` struct uses `UnixStream`, `SCM_RIGHTS`, `RawFd` — all Unix-only. Wrap the entire struct, impl, `recv_fd`, and `async_tun_from_fd` in `#[cfg(unix)]`.

- [ ] **Step 2: Add `#[cfg(windows)]` stub**

Add a Windows stub that compiles but returns errors:

```rust
#[cfg(windows)]
pub struct HelperClient;

#[cfg(windows)]
impl HelperClient {
    pub fn connect() -> Result<Self, crate::FortiError> {
        Err(crate::FortiError::TunDeviceError("Windows helper not yet implemented".to_string()))
    }

    pub fn version(&mut self) -> Result<String, crate::FortiError> {
        Err(crate::FortiError::TunDeviceError("Windows helper not yet implemented".to_string()))
    }

    pub fn create_tun(&mut self, _ip: std::net::Ipv4Addr, _peer_ip: std::net::Ipv4Addr, _mtu: u16) -> Result<(i32, String), crate::FortiError> {
        Err(crate::FortiError::TunDeviceError("Windows helper not yet implemented".to_string()))
    }

    pub fn destroy_tun(&mut self) -> Result<(), crate::FortiError> {
        Err(crate::FortiError::TunDeviceError("Windows helper not yet implemented".to_string()))
    }

    pub fn add_route(&mut self, _dest: &str, _gateway: &str) -> Result<(), crate::FortiError> {
        Err(crate::FortiError::RoutingError("Windows helper not yet implemented".to_string()))
    }

    pub fn delete_route(&mut self, _dest: &str) -> Result<(), crate::FortiError> {
        Err(crate::FortiError::RoutingError("Windows helper not yet implemented".to_string()))
    }

    pub fn configure_dns(&mut self, _config: &crate::VpnConfig) -> Result<(), crate::FortiError> {
        Err(crate::FortiError::RoutingError("Windows helper not yet implemented".to_string()))
    }

    pub fn restore_dns(&mut self) -> Result<(), crate::FortiError> {
        Err(crate::FortiError::RoutingError("Windows helper not yet implemented".to_string()))
    }

    pub fn ping(&mut self) -> Result<(), crate::FortiError> {
        Err(crate::FortiError::TunDeviceError("Windows helper not yet implemented".to_string()))
    }

    pub fn shutdown(&mut self) {}
}
```

- [ ] **Step 3: Build and test**

```bash
cargo test -p fortivpn
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(helper): add platform conditionals — Unix impl + Windows stubs"
```

---

## Task 2: Async TUN — Platform Conditional

**Files:**
- Modify: `crates/fortivpn/src/async_tun.rs`

- [ ] **Step 1: Wrap entire file contents in `#[cfg(unix)]`**

Everything in `async_tun.rs` uses `libc`, `OwnedFd`, `AsyncFd`, `RawFd` — all Unix-only. Wrap the `AsyncTunFd` struct, its impl, and the `AsyncRead`/`AsyncWrite` impls.

- [ ] **Step 2: Add `#[cfg(windows)]` stub**

```rust
#[cfg(windows)]
pub struct AsyncTunFd;

#[cfg(windows)]
impl AsyncTunFd {
    pub fn new(_fd: i32) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Windows TUN async not yet implemented",
        ))
    }
}
```

- [ ] **Step 3: Build and test**

```bash
cargo test -p fortivpn
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(tun): add platform conditionals for async TUN wrapper"
```

---

## Task 3: Routing — Platform Conditional

**Files:**
- Modify: `crates/fortivpn/src/routing.rs`

- [ ] **Step 1: Wrap `run_route` function**

The `run_route` function already has `#[cfg(target_os = "macos")]` and `#[cfg(target_os = "windows")]` blocks. Verify the Windows block exists and compiles. If it uses `route.exe`, good. If not, add a stub:

```rust
#[cfg(target_os = "windows")]
{
    let mut args = vec![action.to_uppercase().as_str(), dest];
    if !gateway.is_empty() {
        args.push(gateway);
    }
    let output = Command::new("route")
        .args(&args)
        .output()
        .map_err(|e| FortiError::RoutingError(format!("route {action}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(FortiError::RoutingError(format!("route {action} {dest}: {stderr}")));
    }
}
```

- [ ] **Step 2: Wrap DNS functions**

The `configure_dns`, `restore_dns` functions use `scutil` (macOS-only). Add `#[cfg]` guards:
- `#[cfg(target_os = "macos")]` for scutil-based DNS
- `#[cfg(target_os = "linux")]` for resolvconf/systemd-resolved (stub for now)
- `#[cfg(target_os = "windows")]` for netsh (stub for now)

- [ ] **Step 3: Wrap IPv6 functions**

`disable_ipv6`, `restore_ipv6`, `get_ipv6_active_interfaces` use `networksetup` (macOS-only). Add stubs for Linux/Windows.

- [ ] **Step 4: Wrap `get_default_gateway`**

Already has `#[cfg]` blocks. Verify Windows block exists.

- [ ] **Step 5: Build and test**

```bash
cargo test -p fortivpn
```

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(routing): add platform conditionals for route, DNS, IPv6 commands"
```

---

## Task 4: Cargo.toml — Platform-Specific Dependencies

**Files:**
- Modify: `crates/fortivpn/Cargo.toml`

- [ ] **Step 1: Make `libc` unix-only**

If `libc` is a direct dependency, make it unix-only:
```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

- [ ] **Step 2: Verify `tun2` is cross-platform**

`tun2` already supports Windows via wintun. No changes needed.

- [ ] **Step 3: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor: make libc dependency unix-only in fortivpn crate"
```

---

## Task 5: Build.rs — Skip Helper on Windows

**Files:**
- Modify: `build.rs`

- [ ] **Step 1: Wrap helper build in `#[cfg(unix)]` or target check**

The `build.rs` at the workspace root builds `fortivpn-helper`. On Windows, the helper binary is different (Windows Service). Skip the helper build on Windows:

```rust
fn main() {
    let target = std::env::var("TARGET").unwrap();

    // Only build the Unix helper on Unix targets
    if !target.contains("windows") {
        // existing helper build logic...
    }
}
```

- [ ] **Step 2: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "refactor(build): skip helper build on Windows targets"
```

---

## Task 6: Helper Binary — Platform Conditional

**Files:**
- Modify: `crates/fortivpn-helper/src/main.rs`
- Modify: `crates/fortivpn-helper/Cargo.toml`

- [ ] **Step 1: Wrap Unix-only code**

The helper uses `launchd`, `libc::getpeereid`, `SCM_RIGHTS`, Unix sockets. Wrap the entire functional code in `#[cfg(unix)]`. Add a `#[cfg(windows)]` main that prints "Windows helper not yet implemented" and exits.

- [ ] **Step 2: Make dependencies platform-conditional**

In `crates/fortivpn-helper/Cargo.toml`, make `libc` unix-only if needed.

- [ ] **Step 3: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(helper): add platform conditionals — Unix impl + Windows stub"
```

---

## Task 7: Final Verification

- [ ] **Step 1: Full CI check**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 2: Verify no Unix imports leak on root level**

```bash
grep -rn "std::os::unix" src/ crates/ --include="*.rs" | grep -v "#\[cfg" | grep -v "test" | grep -v "//"
```

Should return nothing outside of `#[cfg(unix)]` blocks.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "test: verify cross-platform compilation"
```
