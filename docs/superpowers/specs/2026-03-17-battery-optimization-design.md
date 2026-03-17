# Battery Optimization & Password Elimination

## Problem

FortiVPN Tray is the #2 energy consumer on macOS (127.38 over 12 hours), primarily caused by:

1. **osascript child process** (146.5 energy impact) — spawned to show macOS admin password dialog for privileged helper
2. **5-second polling loop** — wakes CPU 17,280 times/day to check VPN session health
3. **osascript notifications** — spawns a process for each macOS notification

Additionally, the app repeatedly asks for:
- **Admin password** — every time the helper process needs to start
- **VPN password** — requires navigating to Settings UI before first connect

## Goal

Match FortiClient VPN behavior: enter credentials once, never again. Minimize energy consumption to near-zero when idle.

## Design

### 1. launchd Daemon for Privileged Helper

Replace osascript-based helper spawning with a launchd-managed daemon using socket activation.

**Install location:**
- Helper binary: `/Library/PrivilegedHelperTools/fortivpn-helper`
- Plist: `/Library/LaunchDaemons/com.fortivpn-tray.helper.plist`
- Socket: `/var/run/fortivpn-helper.sock`

**Plist configuration:**
```xml
<?xml version="1.0" encoding="UTF-8"?>
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
            <integer>438</integer>  <!-- 0o666: any local user can connect; auth via getpeereid -->
        </dict>
    </dict>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>
```

**Socket authentication:**
The daemon validates every connecting client using `getpeereid()` to obtain the peer's UID/GID. Only connections from the same UID that installed the app (stored during installation) or from the `staff` group are accepted. All other connections are rejected immediately. This prevents arbitrary local users from sending root commands to the daemon.

**Daemon idle strategy:**
After the last client disconnects, the daemon stays alive for 30 seconds (idle timeout). If no new connection arrives within that window, it exits cleanly. This prevents race conditions during disconnect/reconnect cycles while avoiding persistent idle processes.

**Behavior:**
- `KeepAlive: false` with socket activation — daemon sleeps until tray app connects to socket, zero energy when idle
- launchd creates the socket at boot; daemon process only starts when data arrives
- Daemon handles TUN creation, route/DNS commands, fd passing (same as today)
- Single well-known socket replaces the per-process temp socket (`/tmp/fortivpn-helper-{pid}.sock`)

**First-launch installation flow:**
1. App detects helper not installed (try connecting to `/var/run/fortivpn-helper.sock` — if connection refused or socket missing, helper needs install)
2. Shows explanation dialog: "FortiVPN needs to install a helper for VPN connections"
3. One-time `osascript` admin prompt to run an install script that:
   - Copies helper binary to `/Library/PrivilegedHelperTools/fortivpn-helper`
   - Copies plist to `/Library/LaunchDaemons/com.fortivpn-tray.helper.plist`
   - Runs `launchctl bootstrap system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist`
4. Never asks for admin password again

**Note on SMJobBless/SMAppService:** These are Apple's recommended mechanisms but require Xcode-specific code signing entitlements and embedded `Info.plist` configurations that are difficult to set up with a Rust/Tauri build pipeline. The `osascript` + `launchctl bootstrap` approach works for non-App-Store distribution (which this app uses — distributed via GitHub Releases as `.dmg`). If macOS hardened runtime blocks the install script, we fall back to prompting the user to run the install command manually in Terminal.

**Helper versioning and upgrades:**
On app launch, if the daemon socket is reachable, the tray app sends a `version` command. If the installed helper version is older than the bundled version, the app triggers the install flow again (one admin prompt to replace the binary and restart the daemon via `launchctl bootout` + `launchctl bootstrap`).

**Code changes:**
- `helper.rs`: `HelperClient::spawn()` replaced with `HelperClient::connect()` — simply opens `/var/run/fortivpn-helper.sock`
- `fortivpn-helper/main.rs`: Use `launch_activate_socket("Listeners")` to receive the pre-created socket fd from launchd. Rust FFI binding: call the C function `launch_activate_socket` from `liblaunch` directly via `extern "C"` block (no crate needed — it's a single function call returning `*mut c_int` fds). Add `version` command handler. Add `getpeereid()` check on accept. Add 30-second idle timeout after last client disconnects.
- New `installer.rs`: First-launch detection (try socket connect), version check, install/upgrade flow via osascript
- `vpn.rs`: `ensure_helper()` simplified — just try connecting to the socket, trigger install if unavailable

**Migration for existing users:** On first launch after update, the app detects the daemon is not installed (socket doesn't exist) and triggers the install flow. The old osascript-based helper code path is removed entirely.

### 2. Event-Driven Status Monitor

Replace the 5-second polling loop with channel-based notification.

**Current** (`lib.rs:361-380`):
```
loop {
    sleep(5 seconds)
    lock VpnState
    check_alive()
    if died → rebuild tray + notify
}
```

**New architecture:**

**Channel type and ownership:**
- `VpnSession::connect()` creates a `tokio::sync::watch::Sender<VpnEvent>` and returns the `watch::Receiver` to the caller
- `VpnEvent` enum: `Alive`, `Died(String)` (string = error reason)
- The `watch::Sender` is passed into `start_bridge()` and stored alongside the `alive: Arc<AtomicBool>` in `BridgeHandle`
- When bridge detects death (LCP echo timeout, task exit, TLS error), it sets `alive` to false AND sends `VpnEvent::Died(reason)` on the watch channel

**Listener lifecycle:**
- `lib.rs`: After a successful `connect()`, spawns a listener task that holds a clone of `AppHandle` and `await`s the `watch::Receiver`
- When `VpnEvent::Died(reason)` is received: updates `VpnManager.status` to `Error(reason)`, calls `rebuild_tray()`, sends notification via Tauri notification API
- The listener task's `JoinHandle` is stored in `VpnManager` and aborted on `disconnect()` — no orphan tasks
- When disconnected, no monitor task exists (zero energy)

**`VpnSession::is_alive()` atomic flag remains** for synchronous checks (CLI `fortivpn status` via IPC).

**Energy savings:** 17,280 wake-ups/day reduced to zero when idle. When connected, wake-ups only on actual state changes.

### 3. Tauri Native Notifications

Replace osascript notification spawning with `tauri-plugin-notification`.

**Current** (`lib.rs:55-60`):
```rust
std::process::Command::new("osascript")
    .args(["-e", &script])
    .spawn();
```

**New:**
```rust
use tauri_plugin_notification::NotificationExt;
app.notification().builder().title(title).body(message).show();
```

**Changes:**
- Add `tauri-plugin-notification` to `Cargo.toml` dependencies
- Add `"notification:default"` to Tauri capabilities in `tauri.conf.json`
- `send_error_notification()` gains `app: &AppHandle` parameter
- Update all call sites (already have `app` handle available)
- Remove `build_notification_script()` helper function and its tests (`test_notification_script_*`)
- `tauri-plugin-shell` remains — still needed for the one-time install flow osascript call in `installer.rs`

### 4. Inline Password Prompt on First Connect

When VPN password is not in Keychain, show a small dialog instead of requiring the user to navigate to Settings.

**Flow:**
1. `handle_connect()` calls `keychain::get_password(&profile.id)`
2. If error (no password stored) → open a password prompt window and return early
3. User enters password in the prompt window → Tauri command saves to Keychain → emits a Tauri event `password-submitted` with profile ID and password
4. A Tauri event listener in `lib.rs` receives the event → calls `handle_connect()` again (this time Keychain has the password)
5. Subsequent connects retrieve password from Keychain silently

**Async coordination:** The password prompt is not awaited inline. Instead, `handle_connect()` opens the window and returns. The prompt window's "Connect" button invokes a Tauri command that saves the password and emits an event. The event listener retriggers the connect flow. This avoids blocking async tasks on UI input.

**Session-only passwords:** If "remember" is unchecked, the password is stored in a `HashMap<String, String>` (profile_id → password) on `VpnManager`. Cleared on disconnect or app exit. `handle_connect()` checks this map before Keychain.

**Implementation:**
- New `src/password-prompt.html` + `src/password-prompt.js` — minimal dialog (~300x150px)
- Tauri webview window with fields: password input, "Remember password" checkbox (default: checked), Connect/Cancel buttons
- New Tauri command `cmd_submit_password` — saves password to Keychain (or in-memory map) and emits `password-submitted` event
- Password prompt is always scoped to a specific profile ID (passed as URL query param to the window)

## Files Affected

| File | Change |
|------|--------|
| `src-tauri/crates/fortivpn/src/helper.rs` | `spawn()` → `connect()`, remove osascript logic |
| `src-tauri/crates/fortivpn/src/bridge.rs` | Accept `watch::Sender`, send death event on channel |
| `src-tauri/crates/fortivpn/src/lib.rs` | Return `watch::Receiver` from `connect()`, add `VpnEvent` enum |
| `src-tauri/crates/fortivpn-helper/src/main.rs` | `launch_activate_socket` FFI, `getpeereid` auth, `version` command, idle timeout |
| `src-tauri/src/lib.rs` | Remove polling loop, add event listener spawning, use Tauri notifications, add password prompt flow, remove `build_notification_script` + tests |
| `src-tauri/src/vpn.rs` | Simplify `ensure_helper()`, store listener `JoinHandle`, add session password map |
| `src-tauri/src/installer.rs` (new) | First-launch detection, version check, install/upgrade flow |
| `src/password-prompt.html` (new) | Password dialog UI |
| `src/password-prompt.js` (new) | Password dialog logic |
| `src-tauri/Cargo.toml` | Add `tauri-plugin-notification` |
| `src-tauri/tauri.conf.json` | Add `notification:default` capability |
| `com.fortivpn-tray.helper.plist` (new) | launchd daemon configuration |

## What Stays the Same

- VPN protocol (auth.rs, tunnel.rs, ppp.rs, routing.rs, tun.rs, async_tun.rs)
- Profile management (profile.rs, Settings UI)
- Keychain integration (keychain.rs)
- CLI companion (fortivpn-cli, IPC)

## Test Impact

- Remove `test_notification_script_*` tests (dead code after osascript removal)
- Update any tests that reference `HelperClient::spawn()` → `HelperClient::connect()`
- New tests for: `installer.rs` (detection logic), `VpnEvent` channel wiring, session password map, `getpeereid` auth
- Existing protocol/bridge/VpnManager unit tests remain valid

## Expected Energy Impact

| Metric | Before | After |
|--------|--------|-------|
| 12hr energy (Activity Monitor) | 127.38 | Near zero (idle), minimal (connected) |
| osascript processes | Persistent child + per-notification | None (except one-time install) |
| CPU wake-ups (disconnected) | Every 5 seconds | Zero |
| CPU wake-ups (connected) | Every 5s (poll) + every 30s (LCP echo) | Only LCP echo (30s, required for health) |
| Admin password prompts | Every helper spawn | Once at install |
| VPN password prompts | Manual via Settings | Once on first connect per profile |
