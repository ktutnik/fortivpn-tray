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
        </dict>
    </dict>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>
```

**Behavior:**
- `KeepAlive: false` with socket activation — daemon sleeps until tray app connects to socket, zero energy when idle
- launchd creates the socket at boot; daemon process only starts when data arrives
- Daemon handles TUN creation, route/DNS commands, fd passing (same as today)
- Daemon exits when socket closes (or stays alive briefly for reconnect scenarios)

**First-launch installation flow:**
1. App detects helper not installed (check for plist or try connecting to socket)
2. Shows explanation dialog: "FortiVPN needs to install a helper for VPN connections"
3. One-time `osascript` admin prompt to copy binary + plist and run `launchctl bootstrap system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist`
4. Never asks for admin password again

**Code changes:**
- `helper.rs`: `HelperClient::spawn()` replaced with `HelperClient::connect()` — simply opens `/var/run/fortivpn-helper.sock`
- `fortivpn-helper/main.rs`: Accept socket fd from launchd (check `LAUNCH_DAEMON_SOCKET_NAME` env var or use `launch_activate_socket`) instead of binding its own socket
- New `installer.rs`: First-launch detection and installation logic
- `vpn.rs`: `ensure_helper()` simplified — just try connecting to the socket, no spawn logic

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

**New:**
- `VpnSession` creates a `tokio::sync::watch<VpnEvent>` channel at connect time
- `bridge.rs` sends a death event on the channel when:
  - LCP echo misses exceed threshold (>3 missed)
  - Bridge task exits unexpectedly
  - TLS tunnel errors
- `lib.rs` spawns a listener task at connect time that `await`s the channel — zero CPU until event arrives
- When disconnected, no monitor task exists (zero energy)
- `VpnSession::is_alive()` atomic flag remains for synchronous checks (CLI status queries)

**Energy savings:** 17,280 wake-ups/day reduced to zero when idle. When connected, wake-ups only occur on actual connection state changes.

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
- `send_error_notification()` gains `app: &AppHandle` parameter
- Update all call sites (already have `app` handle available)
- Remove `build_notification_script()` helper function

### 4. Inline Password Prompt on First Connect

When VPN password is not in Keychain, show a small dialog instead of requiring the user to navigate to Settings.

**Flow:**
1. `handle_connect()` calls `keychain::get_password(&profile.id)`
2. If error (no password stored) → open a small password prompt window
3. User enters password → saved to Keychain → `handle_connect()` retries
4. Subsequent connects retrieve password from Keychain silently

**Implementation:**
- New `password-prompt.html` + `password-prompt.js` — minimal dialog (~300x150px)
- Tauri webview window with fields: password input, "Remember password" checkbox (default: checked), Connect/Cancel buttons
- Tauri command `cmd_prompt_password_and_connect` — saves password if "remember" checked, then calls connect
- If "remember" unchecked, password used for this session only (stored in memory, not Keychain)

## Files Affected

| File | Change |
|------|--------|
| `src-tauri/crates/fortivpn/src/helper.rs` | `spawn()` → `connect()`, remove osascript logic |
| `src-tauri/crates/fortivpn/src/bridge.rs` | Send death event on watch channel |
| `src-tauri/crates/fortivpn/src/lib.rs` | Add watch channel to `VpnSession` |
| `src-tauri/crates/fortivpn-helper/src/main.rs` | Accept launchd socket activation |
| `src-tauri/src/lib.rs` | Remove polling loop, add channel listener, use Tauri notifications, add password prompt flow |
| `src-tauri/src/vpn.rs` | Simplify `ensure_helper()`, add password-prompt fallback in connect |
| `src-tauri/src/installer.rs` (new) | First-launch helper installation |
| `src/password-prompt.html` (new) | Password dialog UI |
| `src/password-prompt.js` (new) | Password dialog logic |
| `src-tauri/Cargo.toml` | Add `tauri-plugin-notification` |

## What Stays the Same

- VPN protocol (auth.rs, tunnel.rs, ppp.rs, routing.rs, tun.rs, async_tun.rs)
- Profile management (profile.rs, Settings UI)
- Keychain integration (keychain.rs)
- CLI companion (fortivpn-cli, IPC)
- All existing tests

## Expected Energy Impact

| Metric | Before | After |
|--------|--------|-------|
| 12hr energy (Activity Monitor) | 127.38 | Near zero (idle), minimal (connected) |
| osascript processes | Persistent child + per-notification | None |
| CPU wake-ups (disconnected) | Every 5 seconds | Zero |
| CPU wake-ups (connected) | Every 5s (poll) + every 30s (LCP echo) | Only LCP echo (30s, required for health) |
| Admin password prompts | Every helper spawn | Once at install |
| VPN password prompts | Manual via Settings | Once on first connect per profile |
