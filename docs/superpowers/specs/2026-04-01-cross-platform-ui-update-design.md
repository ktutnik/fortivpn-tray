# Cross-Platform UI Update — Design Spec

## Problem

The cross-platform UI (`crates/fortivpn-ui/`) for Windows and Linux is out of sync with the macOS Swift app. It has broken connect flows (space-separated IPC breaks on profile names with spaces), no password prompt, no auto-reconnect, no notifications, and the daemon still accesses the keychain directly which causes issues on macOS.

## Goal

Bring the cross-platform UI to feature parity with the macOS Swift app, and make the daemon a pure VPN engine with zero keychain dependencies. All credential access moves to the client (UI or CLI).

## Design Principles

- **Zero macOS changes** (except removing unused IPC calls from Swift `DaemonClient`) — macOS is stable.
- **Daemon owns VPN, clients own credentials** — clean separation.
- **Consistent pattern** across all platforms — every client reads keychain and sends `connect_with_password`.

## Architecture

```
                    Keychain        IPC (Unix socket)        VPN
                    ────────        ─────────────────        ───
Swift UI            read/write  →   connect_with_password →  daemon → gateway
Cross-platform UI   read/write  →   connect_with_password →  daemon → gateway
CLI                 read/write  →   connect_with_password →  daemon → gateway
Daemon              (none)          serves commands           owns VPN logic
```

## Changes by Component

### 1. Daemon (`src/`)

**Remove:**
- `connect` IPC command match arm (no longer used by any client)
- `set_password` IPC command match arm
- `has_password` IPC command match arm
- `src/keychain.rs` (entire file)
- `keyring` dependency from `Cargo.toml`
- `mod keychain;` from `main.rs`

**Add:**
- `subscribe` IPC command — keeps socket open, pushes status change events as JSON lines
- `tokio::sync::broadcast` channel in `AppState` for status change events
- Send to broadcast channel from: connect success, disconnect, session death handlers

**IPC changes:**

Remaining commands: `status`, `list`, `get_profiles`, `save_profile`, `delete_profile`, `connect_with_password`, `disconnect`, `subscribe`

`subscribe` protocol:
```
→ subscribe
← {"event":"status","data":{"status":"connected","profile":"MIMS SG"}}
← {"event":"status","data":{"status":"error: TLS tunnel read error","profile":null}}
← {"event":"status","data":{"status":"disconnected","profile":null}}
```

The connection stays open. Daemon writes a JSON line on each state change. Client reads lines. Connection closes when client disconnects.

### 2. CLI (`crates/fortivpn-cli/`)

**Add dependencies:** `keyring` (with `apple-native` feature)

**Update `connect` command:**
1. Send `list` to find profile by name
2. Read password from keychain via `keyring` crate (service: `fortivpn-tray`, account: profile ID)
3. If no password in keychain, prompt interactively via `rpassword` crate (hidden input)
4. Optionally store the prompted password in keychain
5. Send `connect_with_password {"name":"...","password":"..."}` to daemon

**Update `set-password` command:**
- Move keychain write from daemon IPC to local `keyring` call in the CLI binary

### 3. Cross-Platform UI (`crates/fortivpn-ui/`)

**Add dependencies:** `keyring` (with `apple-native` feature), `notify-rust`

**IPC client (`ipc_client.rs`):**
- Add `connect_with_password(name, password)` — sends JSON format
- Add `subscribe()` — returns a persistent `UnixStream` for reading events
- Increase read timeout to 30s
- Remove `connect_vpn()` (replaced by `connect_with_password`)
- Remove `has_password()` (UI checks keychain directly)
- Remove `set_password()` (UI writes keychain directly)

**Keychain module (new: `keychain.rs`):**
- `read_password(profile_id) -> Option<String>` — via `keyring` crate
- `store_password(profile_id, password)` — via `keyring` crate
- `has_password(profile_id) -> bool` — via `keyring` crate

**Main app (`main.rs`):**

Menu changes:
- Remove status menu item
- Disable other profiles while connected or connecting (`is_busy` check)
- Rebuild menu on every click (currently rebuilds on click via `MenuEvent` — already works, just needs `ipc_client::get_status()` before building)

Connect flow:
1. Check `keychain::has_password(profile_id)`
2. If no password → open password prompt WebView window
3. Read password from keychain
4. Send `connect_with_password` to daemon
5. Show notification on success/failure

Password prompt:
- Small `tao::Window` (300x180) with embedded `wry::WebView`
- HTML form: password field, "Remember" checkbox, Connect/Cancel buttons
- On submit: JS sends IPC message to Rust, Rust stores in keychain via `keyring`, closes window, triggers connect

Auto-reconnect:
- Background thread subscribes to daemon via `subscribe` IPC command
- On `error` status: retry `connect_with_password` up to 3 times with 3s delay
- On clean `disconnected`: no retry, just update icon
- On `connected`: update icon, show notification

Notifications:
- Via `notify-rust` crate (works on both Linux and Windows)
- On connect success: "FortiVPN Connected — Connected to {name}"
- On disconnect: "FortiVPN Disconnected"
- On reconnect attempt: "FortiVPN Reconnecting — Attempt {n}/3"
- On connection failed: "Connection Failed — {error}"

Icon updates:
- Updated immediately from subscribe events (no polling needed)

**Password prompt HTML (new: `resources/password-prompt.html`):**
- Minimal form: title showing profile name, secure password input, "Remember password" checkbox (checked by default), Connect and Cancel buttons
- JS sends result via `window.ipc.postMessage(JSON.stringify({...}))`
- Styled to match the settings page (same CSS variables, dark mode support)

**Settings HTML (`resources/settings.html`):**
- Password status indicator: UI sends `has_password` check to IPC handler in Rust UI code (not to daemon). The Rust handler calls `keychain::has_password()` locally and returns the result to JS.
- Actually simpler: when loading profiles, the Rust UI enriches each profile with `has_password` before passing JSON to the webview init script. No extra IPC roundtrip.

### 4. Swift macOS UI (`macos/FortiVPNTray/`)

**Minimal changes:**
- `ProfileFormView.swift`: Replace `state.client.setPassword(id:password:)` calls with `AppDelegate.storeKeychainPassword()` (or move the keychain functions to a shared location accessible from SwiftUI views). The Settings form currently saves passwords via daemon IPC — this must change to local Keychain writes.
- `DaemonClient.swift`: Remove `setPassword()`, `hasPassword()`, and `connectVPN()` methods (no longer used).
- No VPN behavior changes. macOS functionality is unchanged.

## Error Handling

- If daemon is not running when UI starts: try to spawn it, retry 5 times with 0.5s delay (same as macOS)
- If `connect_with_password` times out (30s): show "Connection timed out" notification, stop loading
- If `subscribe` connection drops: reconnect after 1s delay, retry up to 5 times
- If keychain read fails: show password prompt (same as "no password" case)

## Testing

- Existing 279 tests should still pass (daemon changes are removals + additions)
- Add tests for `subscribe` command (mock broadcast channel)
- CLI keychain integration can't be unit-tested easily (requires actual keychain) — manual test
- Cross-platform UI is manual testing (tray app)

## What This Does NOT Change

- VPN protocol library (`crates/fortivpn/`) — untouched
- Privileged helper (`crates/fortivpn-helper/`) — untouched
- macOS Swift UI behavior — unchanged (only removes unused `DaemonClient` methods)
- IPC socket path — unchanged
- Profile storage — unchanged
- Existing `connect_with_password` format — unchanged
