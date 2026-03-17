# Battery Optimization & Password Elimination — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce FortiVPN Tray battery drain to near-zero when idle and eliminate repeated password prompts — matching FortiClient VPN behavior.

**Architecture:** Replace osascript-based privilege escalation with a launchd socket-activated daemon. Replace the 5-second polling loop with a tokio watch channel for event-driven status monitoring. Replace osascript notifications with Tauri's native notification plugin. Add an inline password prompt for first-connect UX.

**Tech Stack:** Rust, Tauri 2, tokio (watch channels), launchd (socket activation), tauri-plugin-notification, macOS Keychain

**Spec:** `docs/superpowers/specs/2026-03-17-battery-optimization-design.md`

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `src-tauri/src/installer.rs` | First-launch detection, helper install/upgrade via osascript |
| `src-tauri/resources/com.fortivpn-tray.helper.plist` | launchd daemon configuration |
| `src/password-prompt.html` | Password dialog UI (300x150px) |
| `src/password-prompt.js` | Password dialog logic (save + emit event) |

### Modified Files
| File | Changes |
|------|---------|
| `src-tauri/crates/fortivpn/src/bridge.rs` | Add `watch::Sender<VpnEvent>` to `BridgeHandle`, send death events |
| `src-tauri/crates/fortivpn/src/lib.rs` | Add `VpnEvent` enum, return `watch::Receiver` from `connect()` |
| `src-tauri/crates/fortivpn-helper/src/main.rs` | `launch_activate_socket` FFI, multi-client accept loop, `getpeereid` auth, `version` cmd, 30s idle timeout |
| `src-tauri/crates/fortivpn/src/helper.rs` | `spawn()` → `connect()`, remove `socket_path` field, open well-known socket |
| `src-tauri/src/lib.rs` | Remove polling loop, add event listener, Tauri notifications, password prompt flow |
| `src-tauri/src/vpn.rs` | Simplify `ensure_helper()`, store monitor `JoinHandle`, add session password map |
| `src-tauri/Cargo.toml` | Add `tauri-plugin-notification` |
| `src-tauri/capabilities/default.json` | Add `notification:allow-notify`, add `notification:allow-is-permission-granted` |
| `src-tauri/tauri.conf.json` | Add `password-prompt` window config |

---

## Task 1: Add VpnEvent Enum and Watch Channel to Bridge

**Files:**
- Modify: `src-tauri/crates/fortivpn/src/lib.rs:70-79` (VpnSession struct)
- Modify: `src-tauri/crates/fortivpn/src/bridge.rs:287-345` (BridgeHandle, start_bridge)
- Test: `src-tauri/crates/fortivpn/src/bridge.rs` (inline tests)

- [ ] **Step 1: Define VpnEvent and add watch channel to BridgeHandle**

In `src-tauri/crates/fortivpn/src/lib.rs`, add above the `VpnSession` struct:

```rust
/// Events emitted by the VPN session for status monitoring.
#[derive(Debug, Clone, PartialEq)]
pub enum VpnEvent {
    Alive,
    Died(String),
}
```

In `src-tauri/crates/fortivpn/src/bridge.rs`, update the `BridgeHandle` struct:

```rust
pub struct BridgeHandle {
    pub tasks: Vec<JoinHandle<()>>,
    pub alive: Arc<AtomicBool>,
    pub event_rx: tokio::sync::watch::Receiver<crate::VpnEvent>,
}
```

- [ ] **Step 2: Update start_bridge to create and distribute the watch channel**

In `src-tauri/crates/fortivpn/src/bridge.rs`, update `start_bridge()`:

```rust
pub fn start_bridge<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static>(
    tls_stream: TlsStream<TcpStream>,
    tun_device: T,
    shutdown: Arc<Notify>,
    magic_number: u32,
) -> BridgeHandle {
    let alive = Arc::new(AtomicBool::new(true));
    let (event_tx, event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
    let event_tx = Arc::new(event_tx);
    let (tls_reader, tls_writer) = split(tls_stream);
    let (tun_reader, tun_writer) = split(tun_device);
    let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(256);

    let tunnel_writer_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            tunnel_writer_loop(tls_writer, outbound_rx, shutdown, alive, event_tx).await;
        })
    };

    let tunnel_reader_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        let outbound_tx = outbound_tx.clone();
        let event_tx = event_tx.clone();
        tokio::spawn(async move {
            tunnel_reader_loop(tls_reader, tun_writer, outbound_tx, shutdown, alive, magic_number, event_tx).await;
        })
    };

    let tun_reader_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        tokio::spawn(async move {
            tun_reader_loop(tun_reader, outbound_tx, shutdown, alive).await;
        })
    };

    BridgeHandle {
        tasks: vec![tunnel_writer_task, tunnel_reader_task, tun_reader_task],
        alive,
        event_rx,
    }
}
```

- [ ] **Step 3: Update bridge loop functions to send death events**

Update `tunnel_writer_loop` signature to accept `event_tx: Arc<tokio::sync::watch::Sender<crate::VpnEvent>>`. At each `alive.store(false, ...)` line (line 360), add immediately after:

```rust
alive.store(false, Ordering::Relaxed);
let _ = event_tx.send(crate::VpnEvent::Died("TLS write error".to_string()));
```

Update `tunnel_reader_loop` signature similarly. At line 400 (LCP echo timeout):
```rust
alive.store(false, Ordering::Relaxed);
let _ = event_tx.send(crate::VpnEvent::Died("LCP echo timeout — connection lost".to_string()));
```

At line 408 (TLS read error):
```rust
alive.store(false, Ordering::Relaxed);
let _ = event_tx.send(crate::VpnEvent::Died("TLS tunnel read error".to_string()));
```

At line 460 (LCP terminate):
```rust
alive.store(false, Ordering::Relaxed);
let _ = event_tx.send(crate::VpnEvent::Died("Server terminated connection".to_string()));
```

Note: `tun_reader_loop` does not need the event_tx since it doesn't set `alive` to false — it only checks it.

- [ ] **Step 4: Update VpnSession to store the event receiver**

In `src-tauri/crates/fortivpn/src/lib.rs`, update `VpnSession`:

```rust
pub struct VpnSession {
    shutdown: Arc<Notify>,
    route_manager: Option<routing::RouteManager>,
    bridge_tasks: Vec<JoinHandle<()>>,
    alive: Arc<AtomicBool>,
    event_rx: Option<tokio::sync::watch::Receiver<VpnEvent>>,
    host: String,
    port: u16,
    cookie: String,
    trusted_cert: String,
}
```

In `VpnSession::connect()`, update the `Ok(Self { ... })` block (around line 144) to include `event_rx: Some(bridge_handle.event_rx),`.

Add a public method to take the receiver:

```rust
/// Take the event receiver for external monitoring.
/// Can only be called once — returns None on subsequent calls.
pub fn take_event_rx(&mut self) -> Option<tokio::sync::watch::Receiver<VpnEvent>> {
    self.event_rx.take()
}
```

- [ ] **Step 5: Fix bridge tests to account for new event_rx field**

In `src-tauri/crates/fortivpn/src/bridge.rs` tests, any test that constructs or destructures `BridgeHandle` needs to account for the new `event_rx` field. Update test assertions that check `handle.alive` to also verify the event channel. For example, where a test checks `assert!(!handle.alive.load(Ordering::Relaxed))`, also add:

```rust
assert_eq!(*handle.event_rx.borrow(), crate::VpnEvent::Died(_));
// or use matches!:
assert!(matches!(*handle.event_rx.borrow(), crate::VpnEvent::Died(_)));
```

- [ ] **Step 6: Run tests**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo test -p fortivpn`
Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(vpn): add VpnEvent watch channel to bridge for event-driven monitoring"
```

---

## Task 2: Replace Polling Loop with Event-Driven Monitor

**Files:**
- Modify: `src-tauri/src/lib.rs:358-380` (remove polling loop, add event listener)
- Modify: `src-tauri/src/vpn.rs:15-20` (add monitor_handle to VpnManager)

- [ ] **Step 1: Add monitor_handle field to VpnManager**

In `src-tauri/src/vpn.rs`, update the struct:

```rust
pub struct VpnManager {
    pub status: VpnStatus,
    session: Option<VpnSession>,
    helper: Option<HelperClient>,
    connected_profile_id: Option<String>,
    monitor_handle: Option<tokio::task::JoinHandle<()>>,
}
```

Update `VpnManager::new()` to include `monitor_handle: None`.

In `disconnect()` method, before `self.session = None`:

```rust
if let Some(handle) = self.monitor_handle.take() {
    handle.abort();
}
```

- [ ] **Step 2: Remove the 5-second polling loop from lib.rs**

In `src-tauri/src/lib.rs`, delete lines 358-380 (the entire `// Background monitor: check if VPN process died every 5 seconds` block).

- [ ] **Step 3: Add event listener spawning after successful connect**

In `src-tauri/src/lib.rs`, update `handle_connect()` to spawn a monitor after connect succeeds. After the `rebuild_tray` block at the end of `handle_connect()`:

```rust
// Spawn event-driven monitor for this connection
{
    let vpn = app.state::<VpnState>();
    let mut vpn_lock = vpn.lock().await;
    if let Some(ref mut session) = vpn_lock.session {
        if let Some(event_rx) = session.take_event_rx() {
            let app_handle = app.clone();
            let handle = tauri::async_runtime::spawn(async move {
                let mut rx = event_rx;
                loop {
                    if rx.changed().await.is_err() {
                        break; // sender dropped
                    }
                    let event = rx.borrow().clone();
                    if let fortivpn::VpnEvent::Died(ref reason) = event {
                        let reason = reason.clone();
                        // Update VPN state (short lock scope)
                        {
                            let vpn = app_handle.state::<VpnState>();
                            let mut vpn_lock = vpn.lock().await;
                            vpn_lock.session = None;
                            vpn_lock.connected_profile_id = None;
                            vpn_lock.status = VpnStatus::Error(reason.clone());
                            vpn_lock.monitor_handle = None;
                        }
                        // Send notification and rebuild tray (outside VPN lock)
                        send_error_notification(&app_handle, "FortiVPN Disconnected", &reason);
                        {
                            let vpn = app_handle.state::<VpnState>();
                            let vpn_lock = vpn.lock().await;
                            let store = app_handle.state::<StoreState>();
                            let store_lock = store.lock().unwrap();
                            rebuild_tray(&app_handle, &vpn_lock, &store_lock);
                        }
                        break;
                    }
                }
            });
            vpn_lock.monitor_handle = Some(handle);
        }
    }
}
```

Note: `send_error_notification` will be updated in Task 4 to use Tauri notifications. For now, keep the existing osascript version but add the `app` parameter (or just skip the notification call and add it in Task 4).

- [ ] **Step 4: Run tests**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo test --workspace`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(vpn): replace 5-second polling loop with event-driven monitor"
```

---

## Task 3: Switch to Tauri Native Notifications

**Files:**
- Modify: `src-tauri/Cargo.toml:22-34` (add dependency)
- Modify: `src-tauri/capabilities/default.json` (add notification permissions)
- Modify: `src-tauri/src/lib.rs:45-60` (replace osascript with Tauri API)

- [ ] **Step 1: Add tauri-plugin-notification dependency**

In `src-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
tauri-plugin-notification = "2"
```

- [ ] **Step 2: Register the notification plugin**

In `src-tauri/src/lib.rs`, in the `run()` function, add the plugin registration after `tauri_plugin_shell::init()`:

```rust
.plugin(tauri_plugin_notification::init())
```

- [ ] **Step 3: Add notification capability**

Update `src-tauri/capabilities/default.json` — expand the windows scope and add permissions:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capability for the main window",
  "windows": ["main", "settings", "password-prompt"],
  "permissions": [
    "core:default",
    "core:window:allow-close",
    "shell:allow-execute",
    "shell:allow-spawn",
    "shell:allow-stdin-write",
    "shell:allow-kill",
    "notification:default"
  ]
}
```

- [ ] **Step 4: Replace send_error_notification**

In `src-tauri/src/lib.rs`, replace the existing `build_notification_script` and `send_error_notification` functions:

```rust
/// Send a macOS notification via Tauri plugin (no osascript).
fn send_error_notification(app: &tauri::AppHandle, title: &str, message: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(title).body(message).show();
}
```

Remove the `build_notification_script` function entirely.

- [ ] **Step 5: Update all call sites**

In `handle_connect()` (around line 427):
```rust
// Before: send_error_notification("FortiVPN Connection Failed", e);
send_error_notification(app, "FortiVPN Connection Failed", e);
```

In the event monitor spawned in Task 2, it already uses `send_error_notification(&app_handle, ...)`.

- [ ] **Step 6: Remove build_notification_script tests**

Delete these test functions from `lib.rs`:
- `test_notification_script_basic`
- `test_notification_script_escapes_quotes`
- `test_notification_script_escapes_backslash`
- `test_notification_script_empty_strings`
- `test_notification_script_special_chars`

- [ ] **Step 7: Run tests**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo test --workspace`
Expected: All tests pass (notification script tests removed, everything else green)

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat(ui): replace osascript notifications with tauri-plugin-notification"
```

---

## Task 4: Rewrite Helper Binary for launchd Socket Activation

**Files:**
- Modify: `src-tauri/crates/fortivpn-helper/src/main.rs` (full rewrite of main loop)
- Create: `src-tauri/resources/com.fortivpn-tray.helper.plist`
- Modify: `src-tauri/crates/fortivpn-helper/Cargo.toml` (add libc if not present)

- [ ] **Step 1: Create the launchd plist**

Create `src-tauri/resources/com.fortivpn-tray.helper.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.fortivpn-tray.helper</string>
    <key>Program</key>
    <string>/Library/PrivilegedHelperTools/fortivpn-helper</string>
    <key>Sockets</key>
    <dict>
        <key>Listeners</key>
        <dict>
            <key>SockPathName</key>
            <string>/var/run/fortivpn-helper.sock</string>
            <key>SockMode</key>
            <integer>438</integer>
        </dict>
    </dict>
    <key>KeepAlive</key>
    <false/>
    <key>AssociatedBundleIdentifiers</key>
    <array>
        <string>com.ktutnik.fortivpn-tray</string>
    </array>
</dict>
</plist>
```

- [ ] **Step 2: Add launch_activate_socket FFI and main loop rewrite**

Rewrite `src-tauri/crates/fortivpn-helper/src/main.rs`. The key changes:

1. Add FFI for `launch_activate_socket`:

```rust
mod launchd {
    use std::os::unix::io::RawFd;

    extern "C" {
        fn launch_activate_socket(name: *const libc::c_char, fds: *mut *mut libc::c_int, cnt: *mut libc::size_t) -> libc::c_int;
    }

    /// Get socket fds from launchd for the named socket.
    pub fn activate_socket(name: &str) -> Result<Vec<RawFd>, String> {
        use std::ffi::CString;
        let c_name = CString::new(name).map_err(|e| format!("CString: {e}"))?;
        let mut fds: *mut libc::c_int = std::ptr::null_mut();
        let mut cnt: libc::size_t = 0;
        let ret = unsafe { launch_activate_socket(c_name.as_ptr(), &mut fds, &mut cnt) };
        if ret != 0 {
            return Err(format!("launch_activate_socket failed: errno {ret}"));
        }
        let fd_slice = unsafe { std::slice::from_raw_parts(fds, cnt) };
        let result: Vec<RawFd> = fd_slice.to_vec();
        unsafe { libc::free(fds as *mut libc::c_void) };
        Ok(result)
    }
}
```

2. Rewrite `main()` to:
   - Try `launch_activate_socket("Listeners")` first (launchd mode)
   - Fall back to CLI arg socket path (legacy/dev mode)
   - Accept loop with `getpeereid()` validation
   - Handle `version` command (returns the binary version from `env!("CARGO_PKG_VERSION")`)
   - 30-second idle timeout after last client disconnects

```rust
const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
const IDLE_TIMEOUT_SECS: u64 = 30;

fn main() {
    // Two modes: launchd daemon (socket activation) or legacy (CLI arg)
    match launchd::activate_socket("Listeners") {
        Ok(fds) if !fds.is_empty() => {
            // Daemon mode: accept loop with idle timeout
            use std::os::unix::io::FromRawFd;
            let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fds[0]) };
            run_accept_loop(listener);
        }
        _ => {
            // Legacy mode: single client via CLI arg socket path (for dev/testing)
            let args: Vec<String> = std::env::args().collect();
            if args.len() != 2 {
                eprintln!("Usage: fortivpn-helper <socket-path>");
                std::process::exit(1);
            }
            let stream = match std::os::unix::net::UnixStream::connect(&args[1]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Failed to connect to {}: {e}", args[1]);
                    std::process::exit(1);
                }
            };
            handle_client(stream);
        }
    }
}

fn run_accept_loop(listener: std::os::unix::net::UnixListener) {
    listener.set_nonblocking(true).expect("set nonblocking");
    let mut last_activity = std::time::Instant::now();

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if !validate_peer(&stream) {
                    eprintln!("Rejected connection from unauthorized peer");
                    continue;
                }
                last_activity = std::time::Instant::now();
                handle_client(stream);
                last_activity = std::time::Instant::now();
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if last_activity.elapsed() > std::time::Duration::from_secs(IDLE_TIMEOUT_SECS) {
                    break; // No activity for 30 seconds, exit cleanly
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("Accept error: {e}");
                break;
            }
        }
    }
}

fn validate_peer(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let ret = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    if ret != 0 {
        return false;
    }
    // Accept any local user (staff group = gid 20 on macOS, all regular users)
    // This is a deliberate security trade-off for a single-user laptop tool.
    // See design spec for rationale.
    gid == 20 || uid == 0
}
```

3. Extract current command handling into `handle_client(stream: UnixStream)`:

```rust
fn handle_client(stream: std::os::unix::net::UnixStream) {
    let reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = send_error(&mut writer, &format!("Invalid JSON: {e}"));
                continue;
            }
        };

        let cmd = msg["cmd"].as_str().unwrap_or("");
        match cmd {
            "ping" => { let _ = send_ok(&mut writer, None); }
            "version" => {
                let _ = send_ok(&mut writer, Some(serde_json::json!({"version": HELPER_VERSION})));
            }
            "create_tun" => handle_create_tun(&msg, &mut writer),
            "add_route" => handle_add_route(&msg, &mut writer),
            "delete_route" => handle_delete_route(&msg, &mut writer),
            "configure_dns" => handle_configure_dns(&msg, &mut writer),
            "restore_dns" => handle_restore_dns(&mut writer),
            "shutdown" => {
                let _ = send_ok(&mut writer, None);
                break;
            }
            _ => { let _ = send_error(&mut writer, &format!("Unknown command: {cmd}")); }
        }
    }
}
```

- [ ] **Step 3: Build to verify compilation**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo build -p fortivpn-helper`
Expected: Compiles successfully

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(helper): add launchd socket activation with auth and idle timeout"
```

---

## Task 5: Rewrite HelperClient to Connect via Well-Known Socket

**Files:**
- Modify: `src-tauri/crates/fortivpn/src/helper.rs:22-105` (replace spawn with connect)

- [ ] **Step 1: Replace HelperClient::spawn() with HelperClient::connect()**

First, update the `HelperClient` struct to remove the `socket_path` field (no longer needed — socket is managed by launchd):

```rust
pub struct HelperClient {
    reader: BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
}
```

Remove the `Drop` impl entirely (the socket is managed by launchd, not us).

Then replace the entire `spawn()` method:

```rust
const HELPER_SOCKET_PATH: &str = "/var/run/fortivpn-helper.sock";

impl HelperClient {
    /// Connect to the launchd-managed helper daemon via well-known socket.
    pub fn connect() -> Result<Self, FortiError> {
        let stream = std::os::unix::net::UnixStream::connect(HELPER_SOCKET_PATH)
            .map_err(|e| FortiError::TunDeviceError(format!(
                "Connect to helper daemon: {e}. Is the helper installed? Run the app to trigger installation."
            )))?;

        // Set a read timeout for responses
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| FortiError::TunDeviceError(format!("Set timeout: {e}")))?;

        let writer = stream.try_clone()
            .map_err(|e| FortiError::TunDeviceError(format!("Clone stream: {e}")))?;
        let reader = BufReader::new(stream);

        Ok(Self { reader, writer })
    }

    /// Check the helper version.
    pub fn version(&mut self) -> Result<String, FortiError> {
        let cmd = serde_json::json!({"cmd": "version"});
        self.send_cmd(&cmd)?;
        let resp = self.read_response()?;
        Ok(resp["version"].as_str().unwrap_or("unknown").to_string())
    }
}
```

- [ ] **Step 2: Update vpn.rs ensure_helper()**

In `src-tauri/src/vpn.rs`, update `ensure_helper()`:

```rust
fn ensure_helper(&mut self) -> Result<&mut HelperClient, String> {
    if let Some(ref mut h) = self.helper {
        if h.ping().is_ok() {
            return Ok(self.helper.as_mut().unwrap());
        }
        self.helper = None;
    }

    let helper = HelperClient::connect().map_err(|e| e.to_string())?;
    self.helper = Some(helper);
    Ok(self.helper.as_mut().unwrap())
}
```

- [ ] **Step 3: Run tests**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo test --workspace`
Expected: All tests pass (helper tests don't require actual socket)

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(helper): replace osascript spawning with launchd socket connection"
```

---

## Task 6: Add Helper Installer Module

**Files:**
- Create: `src-tauri/src/installer.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod installer`, call from setup)
- Modify: `src-tauri/src/vpn.rs` (trigger install on connect failure)

- [ ] **Step 1: Create installer.rs**

Create `src-tauri/src/installer.rs`:

```rust
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
            Err(_) => true, // can't get version = needs upgrade
        },
        Err(_) => false, // can't connect = needs install, not upgrade
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
    // In Tauri, external binaries are in the resource dir
    let resource_dir = app.path()
        .resource_dir()
        .map_err(|e| format!("Resource dir: {e}"))?;

    // Try the external binary path (Tauri bundles with arch suffix)
    for name in &["fortivpn-helper", "fortivpn-helper-aarch64-apple-darwin", "fortivpn-helper-x86_64-apple-darwin"] {
        let path = resource_dir.join("binaries").join(name);
        if path.exists() {
            return Ok(path.to_string_lossy().to_string());
        }
    }

    // Fallback: next to the main executable
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
    let resource_dir = app.path()
        .resource_dir()
        .map_err(|e| format!("Resource dir: {e}"))?;
    let path = resource_dir.join("com.fortivpn-tray.helper.plist");
    if path.exists() {
        return Ok(path.to_string_lossy().to_string());
    }
    Err("Bundled plist not found".to_string())
}
```

- [ ] **Step 2: Add `mod installer` to lib.rs and call from setup**

In `src-tauri/src/lib.rs`, add `mod installer;` at the top with the other module declarations.

In the `setup()` closure, after state registration but before tray building, add:

```rust
// Check if helper needs installation or upgrade
if !installer::is_helper_installed() {
    if let Err(e) = installer::install_helper(app.handle()) {
        eprintln!("Helper installation failed: {e}");
        // App can still launch — user will see error on first connect
    }
}
```

- [ ] **Step 3: Add plist to Tauri bundle resources**

In `src-tauri/tauri.conf.json`, add to the `bundle` section:

```json
"resources": [
    "resources/com.fortivpn-tray.helper.plist"
]
```

- [ ] **Step 4: Build to verify**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo build`
Expected: Compiles successfully

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(installer): add first-launch helper daemon installation"
```

---

## Task 7: Add Inline Password Prompt

**Files:**
- Create: `src/password-prompt.html`
- Create: `src/password-prompt.js`
- Modify: `src-tauri/src/lib.rs` (add window, command, event listener)
- Modify: `src-tauri/src/vpn.rs` (add session password map)

- [ ] **Step 1: Add session password map to VpnManager**

In `src-tauri/src/vpn.rs`, add to `VpnManager`:

```rust
use std::collections::HashMap;

pub struct VpnManager {
    pub status: VpnStatus,
    session: Option<VpnSession>,
    helper: Option<HelperClient>,
    connected_profile_id: Option<String>,
    monitor_handle: Option<tokio::task::JoinHandle<()>>,
    session_passwords: HashMap<String, String>,
}
```

Update `new()` to include `session_passwords: HashMap::new()`.

In `disconnect()`, clear the session password for the profile:
```rust
if let Some(ref id) = self.connected_profile_id {
    self.session_passwords.remove(id);
}
```

Add a method to get password (session map first, then keychain):

```rust
pub fn get_password(&self, profile_id: &str) -> Result<String, String> {
    if let Some(pw) = self.session_passwords.get(profile_id) {
        return Ok(pw.clone());
    }
    crate::keychain::get_password(profile_id)
}
```

- [ ] **Step 2: Create password-prompt.html**

Create `src/password-prompt.html`:

```html
<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <title>Enter Password</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, sans-serif;
            padding: 20px;
            background: transparent;
            color: #333;
            margin: 0;
        }
        @media (prefers-color-scheme: dark) {
            body { color: #eee; }
            input { background: #2a2a2a; color: #eee; border-color: #555; }
        }
        .field { margin-bottom: 12px; }
        label { display: block; font-size: 13px; margin-bottom: 4px; font-weight: 500; }
        input[type="password"] {
            width: 100%; padding: 6px 8px; border: 1px solid #ccc;
            border-radius: 6px; font-size: 14px; box-sizing: border-box;
        }
        .checkbox-row { display: flex; align-items: center; gap: 6px; margin-bottom: 16px; font-size: 13px; }
        .buttons { display: flex; justify-content: flex-end; gap: 8px; }
        button {
            padding: 6px 16px; border-radius: 6px; border: 1px solid #ccc;
            font-size: 13px; cursor: pointer; background: #f0f0f0;
        }
        @media (prefers-color-scheme: dark) {
            button { background: #3a3a3a; border-color: #555; color: #eee; }
        }
        button.primary { background: #007aff; color: white; border: none; }
    </style>
</head>
<body>
    <div class="field">
        <label id="prompt-label">Password</label>
        <input type="password" id="password" autofocus>
    </div>
    <div class="checkbox-row">
        <input type="checkbox" id="remember" checked>
        <label for="remember">Remember password</label>
    </div>
    <div class="buttons">
        <button onclick="cancel()">Cancel</button>
        <button class="primary" onclick="submit()">Connect</button>
    </div>
    <script src="password-prompt.js"></script>
</body>
</html>
```

- [ ] **Step 3: Create password-prompt.js**

Create `src/password-prompt.js`:

```javascript
const params = new URLSearchParams(window.location.search);
const profileId = params.get('profileId');
const profileName = params.get('profileName');

if (profileName) {
    document.getElementById('prompt-label').textContent = `Password for ${profileName}`;
}

document.getElementById('password').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') submit();
    if (e.key === 'Escape') cancel();
});

async function submit() {
    const password = document.getElementById('password').value;
    const remember = document.getElementById('remember').checked;
    if (!password) return;

    try {
        await window.__TAURI__.core.invoke('cmd_submit_password', {
            profileId,
            password,
            remember,
        });
    } catch (e) {
        console.error('Submit failed:', e);
    }

    window.__TAURI__.event.emit('password-submitted', { profileId });
    window.__TAURI__.window.getCurrent().close();
}

function cancel() {
    window.__TAURI__.window.getCurrent().close();
}
```

- [ ] **Step 4: Add Tauri command and event handling in lib.rs**

In `src-tauri/src/lib.rs`, add the new command:

```rust
#[tauri::command]
async fn cmd_submit_password(
    vpn: tauri::State<'_, VpnState>,
    profile_id: String,
    password: String,
    remember: bool,
) -> Result<(), String> {
    if remember {
        keychain::store_password(&profile_id, &password)?;
    } else {
        let mut vpn_lock = vpn.lock().await;
        vpn_lock.session_passwords.insert(profile_id, password);
    }
    Ok(())
}
```

Register the command in `invoke_handler`:
```rust
.invoke_handler(tauri::generate_handler![
    get_profiles,
    save_profile,
    delete_profile,
    cmd_set_password,
    has_password,
    cmd_submit_password,
])
```

Add a helper function to open the password prompt window:

```rust
fn open_password_prompt(app: &tauri::AppHandle, profile_id: &str, profile_name: &str) {
    // URL-encode params to handle spaces/special chars in profile names
    let encoded_id = urlencoding::encode(profile_id);
    let encoded_name = urlencoding::encode(profile_name);
    let url = format!("/password-prompt.html?profileId={}&profileName={}", encoded_id, encoded_name);
    let _ = tauri::WebviewWindowBuilder::new(
        app,
        "password-prompt",
        tauri::WebviewUrl::App(url.into()),
    )
    .title("Enter VPN Password")
    .inner_size(320.0, 180.0)
    .resizable(false)
    .center()
    .build();
}
```

Add `urlencoding = "2"` to `src-tauri/Cargo.toml` dependencies.
```

- [ ] **Step 5: Update handle_connect() to show password prompt on missing password**

In `handle_connect()`, update the password retrieval:

```rust
pub(crate) async fn handle_connect(app: &tauri::AppHandle, profile_id: &str) {
    let profile = {
        let store = app.state::<StoreState>();
        let store_lock = store.lock().unwrap();
        store_lock.get(profile_id).cloned()
    };

    let Some(profile) = profile else {
        eprintln!("Profile not found: {profile_id}");
        return;
    };

    // Check for password (session map first, then keychain)
    let has_pw = {
        let vpn = app.state::<VpnState>();
        let vpn_lock = vpn.lock().await;
        vpn_lock.get_password(&profile.id).is_ok()
    };

    if !has_pw {
        // No password — show prompt and return. Connect will be retriggered by event.
        open_password_prompt(app, &profile.id, &profile.name);
        return;
    }

    // ... rest of connect logic (use vpn_lock.get_password instead of keychain::get_password)
```

- [ ] **Step 6: Add event listener for password-submitted**

In the `setup()` closure in `lib.rs`, add after tray building:

```rust
// Listen for password-submitted events to retrigger connect
let app_handle = app.handle().clone();
app.listen("password-submitted", move |event| {
    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) {
        if let Some(profile_id) = payload["profileId"].as_str() {
            let app = app_handle.clone();
            let pid = profile_id.to_string();
            tauri::async_runtime::spawn(async move {
                handle_connect(&app, &pid).await;
            });
        }
    }
});
```

- [ ] **Step 7: Update vpn.rs connect() to use get_password**

In `src-tauri/src/vpn.rs`, update `connect()` method to accept password as parameter instead of reading from keychain directly:

```rust
pub async fn connect(&mut self, profile: &VpnProfile, password: &str) -> Result<(), String> {
```

And in `handle_connect()`, get the password from VpnManager:

```rust
let result = {
    let vpn = app.state::<VpnState>();
    let mut vpn_lock = vpn.lock().await;
    let password = vpn_lock.get_password(&profile.id).map_err(|e| e.to_string())?;
    vpn_lock.connect(&profile, &password).await
};
```

- [ ] **Step 8: Run tests**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo test --workspace`
Expected: All tests pass

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(ui): add inline password prompt on first connect"
```

---

## Task 8: Integration Testing and Cleanup

**Files:**
- Modify: Various files for final cleanup

- [ ] **Step 1: Full build verification**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo build --release --workspace`
Expected: All binaries compile

- [ ] **Step 2: Run full test suite**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo test --workspace`
Expected: All tests pass

- [ ] **Step 3: Run clippy**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo clippy --workspace -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Run fmt check**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray/src-tauri && cargo fmt --check`
Expected: No formatting issues

- [ ] **Step 5: Verify osascript is no longer used except installer**

Run: `cd /Users/ktutnik/Documents/fortivpn-tray && grep -r "osascript" src-tauri/src/ src-tauri/crates/`
Expected: Only `installer.rs` should contain `osascript` references

- [ ] **Step 6: Commit any cleanup**

```bash
git add -A && git commit -m "chore: final cleanup and verification"
```
