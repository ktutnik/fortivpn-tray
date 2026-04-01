# Cross-Platform UI Update — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the cross-platform UI to feature parity with macOS, remove all keychain access from the daemon, and add a `subscribe` IPC command for real-time status updates.

**Architecture:** Remove `connect`, `set_password`, `has_password` IPC commands and `keychain.rs` from the daemon. Add `subscribe` command with broadcast channel. Update CLI to read keychain locally. Update cross-platform UI with password prompt, auto-reconnect, notifications, and subscribe-based icon updates. Minimal Swift changes (replace IPC setPassword calls with local Keychain writes).

**Tech Stack:** Rust (tokio broadcast channel, keyring, notify-rust, wry), Swift (Security.framework)

**Spec:** `docs/superpowers/specs/2026-04-01-cross-platform-ui-update-design.md`

---

## File Structure

### Modified Files
| File | Changes |
|------|---------|
| `src/ipc.rs` | Remove `connect`, `set_password`, `has_password` match arms. Add `subscribe` command. Add `broadcast::Sender` to `AppState`. Send to broadcast on connect/disconnect/death. |
| `src/main.rs` | Remove `mod keychain`. Create broadcast channel in AppState. |
| `src/notification.rs` | Remove `send_notification` (no longer needed — clients handle notifications). Keep `post_distributed_notification`. |
| `Cargo.toml` | Remove `keyring` dependency. |
| `crates/fortivpn-cli/Cargo.toml` | Add `keyring`, `rpassword` dependencies. |
| `crates/fortivpn-cli/src/main.rs` | Rewrite `connect` to read keychain locally, send `connect_with_password`. Add interactive password prompt. |
| `crates/fortivpn-ui/Cargo.toml` | Add `keyring`, `notify-rust` dependencies. |
| `crates/fortivpn-ui/src/main.rs` | Add password prompt window, auto-reconnect, notifications, subscribe listener, remove status menu item, disable profiles while busy. |
| `crates/fortivpn-ui/src/ipc_client.rs` | Add `connect_with_password`, `subscribe`. Remove `connect_vpn`, `set_password`, `has_password`. Increase timeout to 30s. |
| `crates/fortivpn-ui/resources/settings.html` | Fix password status (UI enriches via local keychain, not daemon). |
| `macos/FortiVPNTray/Sources/DaemonClient.swift` | Remove `setPassword`, `hasPassword`, `connectVPN` methods. |
| `macos/FortiVPNTray/Sources/ProfileFormView.swift` | Replace `state.client.setPassword()` with local Keychain writes. |

### New Files
| File | Responsibility |
|------|---------------|
| `crates/fortivpn-ui/src/keychain.rs` | Keychain read/write/check via `keyring` crate. |
| `crates/fortivpn-ui/resources/password-prompt.html` | Small HTML password dialog. |

### Deleted Files
| File | Reason |
|------|--------|
| `src/keychain.rs` | Daemon no longer accesses keychain. |

---

## Task 1: Remove Keychain from Daemon

**Files:**
- Delete: `src/keychain.rs`
- Modify: `src/main.rs` — remove `mod keychain`
- Modify: `src/ipc.rs` — remove `connect`, `set_password`, `has_password` match arms; remove `crate::keychain` references
- Modify: `src/notification.rs` — remove `send_notification` function (keep `post_distributed_notification`)
- Modify: `Cargo.toml` — remove `keyring` dependency

- [ ] **Step 1: Remove `mod keychain` from main.rs**

In `src/main.rs`, remove line `mod keychain;`.

- [ ] **Step 2: Delete `src/keychain.rs`**

```bash
rm src/keychain.rs
```

- [ ] **Step 3: Remove `keyring` from `Cargo.toml`**

Remove this line from `[dependencies]`:
```toml
keyring = { version = "3", features = ["apple-native"] }
```

- [ ] **Step 4: Remove `connect` match arm from `src/ipc.rs`**

Remove lines 157-269 (the entire `"connect" =>` block).

- [ ] **Step 5: Remove `set_password` match arm from `src/ipc.rs`**

Remove lines 483-511 (the entire `"set_password" =>` block).

- [ ] **Step 6: Remove `has_password` match arm from `src/ipc.rs`**

Remove lines 513-529 (the entire `"has_password" =>` block).

- [ ] **Step 7: Update `send_notification` in `src/notification.rs`**

Replace the `send_notification` function with a no-op on all platforms (clients own notifications now):

```rust
/// No-op — desktop notifications are handled by the UI clients, not the daemon.
/// Kept as a function signature so call sites don't need conditional compilation.
pub fn send_notification(_title: &str, _body: &str) {}
```

- [ ] **Step 8: Remove `crate::keychain` references in `src/ipc.rs`**

Search for any remaining `crate::keychain::` calls and remove them. The `connect_with_password` handler should NOT have any keychain references (it receives the password via JSON). The `get_profiles` handler should NOT check `has_password` via keychain.

- [ ] **Step 9: Update unknown command help text in `src/ipc.rs`**

Update the default match arm to list only remaining commands:
```rust
"Unknown command: {command}. Available: status, list, connect_with_password, disconnect, get_profiles, save_profile, delete_profile, subscribe"
```

- [ ] **Step 10: Fix tests that reference removed commands**

Update or remove tests for `connect`, `set_password`, `has_password`:
- `test_ipc_connect_*` tests — remove all (command no longer exists)
- `test_ipc_set_password_*` tests — remove all
- `test_ipc_has_password_*` tests — remove all
- Update `test_ipc_unknown_command` if it references removed commands

- [ ] **Step 11: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 12: Commit**

```bash
git add -A && git commit -m "refactor: remove keychain from daemon — clients own all credential access"
```

---

## Task 2: Add Subscribe IPC Command

**Files:**
- Modify: `src/ipc.rs` — add broadcast channel to AppState, add `subscribe` handler, send events on state changes
- Modify: `src/main.rs` — create broadcast channel

- [ ] **Step 1: Add broadcast channel to AppState**

In `src/ipc.rs`, update `AppState`:

```rust
#[derive(Clone)]
pub struct AppState {
    pub vpn: VpnState,
    pub store: StoreState,
    pub status_tx: tokio::sync::broadcast::Sender<String>,
}
```

- [ ] **Step 2: Create broadcast channel in main.rs**

In `src/main.rs`, create the channel and pass to AppState:

```rust
let (status_tx, _) = tokio::sync::broadcast::channel::<String>(16);

let state = ipc::AppState {
    vpn: Arc::new(tokio::sync::Mutex::new(vpn_manager)),
    store: Arc::new(Mutex::new(store)),
    status_tx,
};
```

- [ ] **Step 3: Add `subscribe` match arm in `src/ipc.rs`**

The `subscribe` command is different from others — it doesn't return an `IpcResponse`. Instead it holds the connection open and writes events. This needs special handling.

Add a new function and update `handle_ipc_command` to handle subscribe separately. In the `start_ipc_server` accept loop, before calling `handle_ipc_command`, check if the command is `subscribe` and handle it differently:

In the `subscribe` handler, read from a broadcast receiver and write JSON lines to the socket:

```rust
async fn handle_subscribe(state: &AppState, writer: &mut tokio::io::BufWriter<tokio::net::unix::OwnedWriteHalf>) {
    use tokio::io::AsyncWriteExt;
    let mut rx = state.status_tx.subscribe();
    loop {
        match rx.recv().await {
            Ok(event_json) => {
                let line = format!("{}\n", event_json);
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break; // client disconnected
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
        }
    }
}
```

- [ ] **Step 4: Send status events on connect/disconnect/death**

In the `connect_with_password` handler, after successful connect:
```rust
let _ = state.status_tx.send(serde_json::json!({"event":"status","data":{"status":"connected","profile": profile.name}}).to_string());
```

After failed connect:
```rust
let _ = state.status_tx.send(serde_json::json!({"event":"status","data":{"status": format!("error: {e}"), "profile": null}}).to_string());
```

In the `disconnect` handler:
```rust
let _ = state.status_tx.send(serde_json::json!({"event":"status","data":{"status":"disconnected","profile":null}}).to_string());
```

In the session death monitor (inside the `tokio::spawn` block):
```rust
let _ = st.status_tx.send(serde_json::json!({"event":"status","data":{"status": format!("error: {reason}"), "profile": null}}).to_string());
```

- [ ] **Step 5: Update `make_state()` test helper**

In `src/ipc.rs` tests, update `make_state()` to include the new `status_tx` field:

```rust
fn make_state() -> AppState {
    let store = make_store();
    let (status_tx, _) = tokio::sync::broadcast::channel::<String>(16);
    AppState {
        vpn: std::sync::Arc::new(tokio::sync::Mutex::new(crate::vpn::VpnManager::new())),
        store: std::sync::Arc::new(std::sync::Mutex::new(store)),
        status_tx,
    }
}
```

- [ ] **Step 6: Add test for subscribe broadcast**

```rust
#[tokio::test]
async fn test_broadcast_channel_sends_on_disconnect() {
    let state = make_state();
    let mut rx = state.status_tx.subscribe();
    let _ = state.status_tx.send(r#"{"event":"status","data":{"status":"disconnected","profile":null}}"#.to_string());
    let msg = rx.recv().await.unwrap();
    assert!(msg.contains("disconnected"));
}
```

- [ ] **Step 6: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(ipc): add subscribe command with broadcast channel for real-time status updates"
```

---

## Task 3: Update CLI to Read Keychain Locally

**Files:**
- Modify: `crates/fortivpn-cli/Cargo.toml` — add `keyring`, `rpassword`
- Modify: `crates/fortivpn-cli/src/main.rs` — rewrite connect to use `connect_with_password`

- [ ] **Step 1: Add dependencies to CLI Cargo.toml**

Add to `[dependencies]`:
```toml
keyring = { version = "3", features = ["apple-native"] }
rpassword = "5"
```

- [ ] **Step 2: Rewrite connect command in CLI**

In `crates/fortivpn-cli/src/main.rs`, update the connect handling to:
1. Send `list` to get profiles
2. Find the profile by name/partial match
3. Read password from keychain via `keyring`
4. If no password, prompt interactively via `rpassword`
5. Send `connect_with_password` JSON to daemon

The connect section should use `keyring::Entry` to read the password, and fall back to `rpassword::prompt_password` if not found.

- [ ] **Step 3: Build and test CLI**

```bash
cargo build -p fortivpn-cli
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(cli): read keychain locally, send connect_with_password — daemon-free credential access"
```

---

## Task 4: Update Cross-Platform UI — IPC Client and Keychain

**Files:**
- Modify: `crates/fortivpn-ui/Cargo.toml` — add `keyring`, `notify-rust`
- Modify: `crates/fortivpn-ui/src/ipc_client.rs` — add `connect_with_password`, `subscribe`, remove old methods, increase timeout
- Create: `crates/fortivpn-ui/src/keychain.rs` — local keychain access

- [ ] **Step 1: Add dependencies**

In `crates/fortivpn-ui/Cargo.toml` add:
```toml
keyring = { version = "3", features = ["apple-native"] }
notify-rust = "4"
```

- [ ] **Step 2: Create `crates/fortivpn-ui/src/keychain.rs`**

```rust
use keyring::Entry;

const SERVICE_NAME: &str = "fortivpn-tray";

pub fn read_password(profile_id: &str) -> Option<String> {
    Entry::new(SERVICE_NAME, profile_id)
        .ok()?
        .get_password()
        .ok()
}

pub fn store_password(profile_id: &str, password: &str) -> Result<(), String> {
    Entry::new(SERVICE_NAME, profile_id)
        .map_err(|e| format!("Keychain entry error: {e}"))?
        .set_password(password)
        .map_err(|e| format!("Keychain store error: {e}"))
}

pub fn has_password(profile_id: &str) -> bool {
    read_password(profile_id).is_some()
}
```

- [ ] **Step 3: Update `crates/fortivpn-ui/src/ipc_client.rs`**

- Increase read timeout from 10s to 30s
- Add `connect_with_password(name: &str, password: &str)` — sends JSON format
- Add `subscribe() -> Option<std::os::unix::net::UnixStream>` — sends `subscribe\n`, returns the open stream for reading events
- Remove `connect_vpn()`, `set_password()`, `has_password()`
- Remove `has_password` field from `VpnProfile` struct (UI enriches it locally)

- [ ] **Step 4: Build**

```bash
cargo build -p fortivpn-ui
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(ui): add keychain module, connect_with_password, subscribe to IPC client"
```

---

## Task 5: Cross-Platform UI — Password Prompt, Menu, Notifications

**Files:**
- Create: `crates/fortivpn-ui/resources/password-prompt.html`
- Modify: `crates/fortivpn-ui/src/main.rs` — password prompt window, remove status item, disable profiles while busy, notifications, subscribe listener, auto-reconnect

- [ ] **Step 1: Create password prompt HTML**

Create `crates/fortivpn-ui/resources/password-prompt.html` — small form with:
- Title showing profile name
- Secure password input
- "Remember password" checkbox (checked by default)
- Connect and Cancel buttons
- Dark mode support (same CSS variables as settings.html)
- JS sends result via `window.ipc.postMessage(JSON.stringify({password, remember}))`

- [ ] **Step 2: Update `main.rs` — remove status menu item**

In `build_tray_menu()`, remove the status text menu item block (lines 142-161). Keep the separator, settings, and quit items.

- [ ] **Step 3: Update `main.rs` — disable profiles while busy**

Change `!is_connected` to `!is_busy` where `is_busy = status == "connected" || status == "connecting" || status == "disconnecting"`.

- [ ] **Step 4: Update `main.rs` — add password prompt**

Add a function `open_password_prompt(event_loop, profile_name) -> Option<String>` that:
1. Creates a small `tao::Window` (320x200)
2. Embeds a `wry::WebView` with the password prompt HTML
3. Uses IPC handler to capture the password from JS
4. Returns `Some(password)` or `None` if cancelled

Use `std::sync::mpsc::channel` to communicate between the webview IPC handler and the calling code.

- [ ] **Step 5: Update `main.rs` — rewrite connect flow**

In the connect menu handler:
1. Check `keychain::has_password(profile_id)`
2. If no password → open password prompt → `keychain::store_password()` if remember checked
3. Read password via `keychain::read_password()`
4. Send `ipc_client::connect_with_password(name, password)`
5. `update_tray()`

- [ ] **Step 6: Update `main.rs` — add notifications**

Add a helper function:
```rust
fn show_notification(title: &str, body: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(body)
        .show();
}
```

Call it on: connect success, connect failure, disconnect, reconnect attempt.

- [ ] **Step 7: Update `main.rs` — add subscribe listener and auto-reconnect**

Spawn a background thread that:
1. Calls `ipc_client::subscribe()` to get a persistent stream
2. Reads JSON lines from the stream
3. On status change → send `AppEvent::RebuildTray` to event loop via proxy
4. On error status with was-connected → auto-reconnect (read keychain, send `connect_with_password`, up to 3 retries with 3s delay)
5. On clean disconnect → just update tray

Store `EventLoopProxy` in a static or pass to the thread.

- [ ] **Step 8: Update `main.rs` — handle AppEvent::RebuildTray from subscribe**

In the event loop, handle `Event::UserEvent(AppEvent::RebuildTray)` already exists — just ensure `update_tray()` is called.

- [ ] **Step 9: Build and test**

```bash
cargo build -p fortivpn-ui
```

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(ui): add password prompt, auto-reconnect, notifications, subscribe listener"
```

---

## Task 6: Update Settings HTML

**Files:**
- Modify: `crates/fortivpn-ui/resources/settings.html`

- [ ] **Step 1: Fix password status in settings HTML**

The settings HTML currently expects `has_password` from the `get_profiles` response. Since the daemon no longer sends this field, the Rust UI must enrich profiles before passing to the webview.

In `main.rs`, update `build_profiles_json()` to add `has_password` from local keychain:

```rust
fn build_profiles_json() -> String {
    let profiles = ipc_client::get_profiles();
    let enriched: Vec<serde_json::Value> = profiles.iter().map(|p| {
        serde_json::json!({
            "id": p.id,
            "name": p.name,
            "host": p.host,
            "port": p.port,
            "username": p.username,
            "trusted_cert": p.trusted_cert,
            "has_password": keychain::has_password(&p.id),
        })
    }).collect();
    serde_json::to_string(&enriched).unwrap_or_else(|_| "[]".to_string())
}
```

- [ ] **Step 2: Update settings HTML IPC handler for set_password**

In `handle_settings_ipc`, replace the `set_password` IPC call with a local keychain write:

```rust
"set_password" => {
    if let (Some(id), Some(pw)) = (data["id"].as_str(), data["password"].as_str()) {
        let _ = keychain::store_password(id, pw);
    }
}
```

- [ ] **Step 3: Build**

```bash
cargo build -p fortivpn-ui
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "fix(ui): enrich profiles with local keychain status, handle set_password locally"
```

---

## Task 7: Update Swift macOS UI

**Files:**
- Modify: `macos/FortiVPNTray/Sources/DaemonClient.swift` — remove `setPassword`, `hasPassword`, `connectVPN`
- Modify: `macos/FortiVPNTray/Sources/ProfileFormView.swift` — replace IPC setPassword with local Keychain

- [ ] **Step 1: Remove unused methods from DaemonClient.swift**

Remove these methods:
- `connectVPN(name:)` (lines 73-75)
- `setPassword(id:password:)` (lines 97-99)
- `hasPassword(id:)` (lines 101-105)

- [ ] **Step 2: Update ProfileFormView.swift — replace IPC setPassword**

At line 78, replace:
```swift
let resp = state.client.setPassword(id: id, password: password)
if resp?.ok == true {
```
with a local Keychain write. Add a helper function to `ProfileFormView` or call `AppDelegate`'s `storeKeychainPassword` (which needs to be made accessible — either move to a shared utility or duplicate).

Simplest: add a static keychain helper:
```swift
private func storePassword(profileId: String, password: String) -> Bool {
    let passwordData = password.data(using: .utf8)!
    let query: [String: Any] = [
        kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: "fortivpn-tray",
        kSecAttrAccount as String: profileId,
    ]
    let update: [String: Any] = [kSecValueData as String: passwordData]
    let status = SecItemUpdate(query as CFDictionary, update as CFDictionary)
    if status == errSecItemNotFound {
        var addQuery = query
        addQuery[kSecValueData as String] = passwordData
        return SecItemAdd(addQuery as CFDictionary, nil) == errSecSuccess
    }
    return status == errSecSuccess
}
```

Replace both `state.client.setPassword(...)` calls (lines 78 and 149) with `storePassword(profileId:password:)`.

- [ ] **Step 3: Build Swift**

```bash
cd macos/FortiVPNTray && swift build -c release
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor(swift): replace IPC setPassword with local Keychain writes"
```

---

## Task 8: Final Verification

- [ ] **Step 1: Full CI check**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Expected: all tests pass, no warnings.

- [ ] **Step 2: Build Swift**

```bash
cd macos/FortiVPNTray && swift build -c release
```

- [ ] **Step 3: Integration test — macOS**

```bash
./install.sh
```
Verify: tray icon, connect, disconnect, settings, password prompt, auto-reconnect all work.

- [ ] **Step 4: Integration test — cross-platform UI**

```bash
cargo build --release -p fortivpn-ui
./target/release/fortivpn-ui
```
Verify: tray icon, connect with password prompt, settings, notifications.

- [ ] **Step 5: Integration test — CLI**

```bash
cargo build --release -p fortivpn-cli
./target/release/fortivpn status
./target/release/fortivpn connect sg
./target/release/fortivpn disconnect
```

- [ ] **Step 6: Commit cleanup**

```bash
git add -A && git commit -m "chore: final verification and cleanup"
```
