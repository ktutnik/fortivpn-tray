# Plan A: Daemon TCP Migration — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Unix socket IPC with TCP localhost across daemon, CLI, and cross-platform UI so all three build and run on macOS, Linux, and Windows.

**Architecture:** Replace `UnixListener`/`UnixStream` with `TcpListener`/`TcpStream` on `127.0.0.1:9847`. Remove `socket_path()` and `cleanup_socket()` functions. Remove `post_distributed_notification` (macOS-only, subscribe over TCP replaces it). Update CLI and cross-platform UI to connect via TCP. All existing IPC commands and protocol unchanged — only the transport layer changes.

**Tech Stack:** Rust, tokio (TcpListener/TcpStream), std::net::TcpStream (CLI, cross-platform UI)

**Spec:** `docs/superpowers/specs/2026-04-02-cross-platform-rust-rewrite-design.md`

---

## File Structure

### Modified Files
| File | Changes |
|------|---------|
| `src/ipc.rs` | Replace `UnixListener` with `TcpListener`. Remove `socket_path()`, `cleanup_socket()`. Update `start_ipc_server` and `handle_subscribe` to use TCP streams. |
| `src/main.rs` | Remove reference to cleanup_socket if any. |
| `src/notification.rs` | Remove `post_distributed_notification`. Keep `send_notification` as no-op. |
| `crates/fortivpn-cli/src/main.rs` | Replace `UnixStream` with `TcpStream` to `127.0.0.1:9847`. Remove `socket_path()`. |
| `crates/fortivpn-ui/src/ipc_client.rs` | Replace `UnixStream` with `TcpStream` to `127.0.0.1:9847`. Remove `socket_path()`. |

### No New Files

### No Deleted Files

---

## Task 1: Daemon — Replace Unix Socket with TCP

**Files:**
- Modify: `src/ipc.rs`

- [ ] **Step 1: Replace imports**

In `src/ipc.rs`, replace:
```rust
use tokio::net::UnixListener;
```
with:
```rust
use tokio::net::TcpListener;
```

Also remove `use std::path::PathBuf;` if it was only used for `socket_path`.

- [ ] **Step 2: Add TCP address constant**

Add near the top of `src/ipc.rs`:
```rust
const DAEMON_ADDR: &str = "127.0.0.1:9847";
```

- [ ] **Step 3: Remove `socket_path()` function**

Delete the `socket_path()` function (lines 21-26).

- [ ] **Step 4: Remove `cleanup_socket()` function**

Delete the `cleanup_socket()` function (lines 441-443).

- [ ] **Step 5: Update `start_ipc_server` to use TCP**

Replace the socket binding logic. Change:
```rust
let sock = socket_path();
let _ = std::fs::remove_file(&sock);
let listener = match UnixListener::bind(&sock) {
```
to:
```rust
let listener = match TcpListener::bind(DAEMON_ADDR).await {
```

Remove the `#[cfg(unix)]` permissions block (no file permissions needed for TCP).

Also update the log line:
```rust
log::info!(target: "ipc", "Listening on {}", DAEMON_ADDR);
```

- [ ] **Step 6: Update stream splitting in client handler**

The current code uses `stream.into_split()` which works for both Unix and TCP streams in tokio. Verify the types are compatible. `TcpStream::into_split()` returns `(OwnedReadHalf, OwnedWriteHalf)` — same API as Unix.

If the code uses `tokio::net::unix::OwnedWriteHalf` explicitly, change to `tokio::net::tcp::OwnedWriteHalf`.

- [ ] **Step 7: Update `handle_subscribe` function**

The `handle_subscribe` function takes a `BufWriter<OwnedWriteHalf>`. If the type was explicitly `tokio::net::unix::OwnedWriteHalf`, change to `tokio::net::tcp::OwnedWriteHalf`.

- [ ] **Step 8: Update test helper `test_socket_path_contains_ipc_sock`**

Remove or replace any test that references `socket_path()`. Add a test for the constant:
```rust
#[test]
fn test_daemon_addr() {
    assert_eq!(DAEMON_ADDR, "127.0.0.1:9847");
}
```

- [ ] **Step 9: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "refactor(ipc): replace Unix socket with TCP localhost:9847"
```

---

## Task 2: Remove `post_distributed_notification`

**Files:**
- Modify: `src/notification.rs`
- Modify: `src/ipc.rs` (remove calls to `post_distributed_notification`)

- [ ] **Step 1: Remove `post_distributed_notification` from `notification.rs`**

Delete the entire function (lines 7-14 of `src/notification.rs`). The file should only contain:
```rust
/// No-op — desktop notifications are handled by the UI clients, not the daemon.
pub fn send_notification(_title: &str, _body: &str) {}
```

- [ ] **Step 2: Remove all calls to `post_distributed_notification` in `src/ipc.rs`**

Search for `crate::notification::post_distributed_notification` and remove all occurrences. There should be calls after:
- Successful connect
- Disconnect
- Session death

Remove the call lines only — keep the surrounding code.

- [ ] **Step 3: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "refactor: remove post_distributed_notification — subscribe over TCP replaces it"
```

---

## Task 3: CLI — Replace Unix Socket with TCP

**Files:**
- Modify: `crates/fortivpn-cli/src/main.rs`

- [ ] **Step 1: Replace imports**

Remove:
```rust
use std::os::unix::net::UnixStream;
```
Add:
```rust
use std::net::TcpStream;
```

- [ ] **Step 2: Add TCP address constant**

```rust
const DAEMON_ADDR: &str = "127.0.0.1:9847";
```

- [ ] **Step 3: Remove `socket_path()` function**

Delete the function entirely (lines 5-10).

- [ ] **Step 4: Update `connect_stream()` function**

Replace `UnixStream::connect(socket_path())` with `TcpStream::connect(DAEMON_ADDR)`. The rest of the function (set_read_timeout, try_clone, BufReader) works identically with TcpStream.

- [ ] **Step 5: Remove `dirs` dependency from Cargo.toml**

If `dirs` was only used for `socket_path()`, remove it from `crates/fortivpn-cli/Cargo.toml`.

- [ ] **Step 6: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "refactor(cli): replace Unix socket with TCP localhost:9847"
```

---

## Task 4: Cross-Platform UI — Replace Unix Socket with TCP

**Files:**
- Modify: `crates/fortivpn-ui/src/ipc_client.rs`

- [ ] **Step 1: Replace imports**

Remove:
```rust
use std::os::unix::net::UnixStream;
```
Add:
```rust
use std::net::TcpStream;
```

- [ ] **Step 2: Add TCP address constant and remove `socket_path()`**

Remove the `socket_path()` function. Add:
```rust
const DAEMON_ADDR: &str = "127.0.0.1:9847";
```

- [ ] **Step 3: Update `send_command()` function**

Replace `UnixStream::connect(socket_path())` with `TcpStream::connect(DAEMON_ADDR)`. The rest (set_read_timeout, try_clone, BufReader, writeln, read_line) works identically.

- [ ] **Step 4: Update `subscribe()` function**

Replace `UnixStream::connect(socket_path())` with `TcpStream::connect(DAEMON_ADDR)`. Update the return type from `Option<BufReader<UnixStream>>` to `Option<BufReader<TcpStream>>`.

- [ ] **Step 5: Remove `dirs` dependency**

If `dirs` was only used for `socket_path()`, remove it from `crates/fortivpn-ui/Cargo.toml`.

- [ ] **Step 6: Build and test**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "refactor(ui): replace Unix socket with TCP localhost:9847"
```

---

## Task 5: Update Swift macOS UI (DaemonClient)

**Files:**
- Modify: `macos/FortiVPNTray/Sources/DaemonClient.swift`

- [ ] **Step 1: Replace Unix socket connection with TCP**

The current `DaemonClient` connects to a Unix socket using raw `Darwin.socket(AF_UNIX, ...)`. Replace with TCP:

Replace the socket creation and connection logic in `send(command:)`:

```swift
let fd = Darwin.socket(AF_INET, SOCK_STREAM, 0)
guard fd >= 0 else { return nil }
defer { Darwin.close(fd) }

var addr = sockaddr_in()
addr.sin_family = sa_family_t(AF_INET)
addr.sin_port = UInt16(9847).bigEndian
addr.sin_addr.s_addr = inet_addr("127.0.0.1")

let len = socklen_t(MemoryLayout<sockaddr_in>.size)
let connected = withUnsafePointer(to: &addr) { ptr in
    ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
        Darwin.connect(fd, $0, len)
    }
}
guard connected == 0 else { return nil }
```

- [ ] **Step 2: Remove socket path from init**

Remove the `socketPath` property and the Application Support path logic from `init()`. Replace with:
```swift
private let host = "127.0.0.1"
private let port: UInt16 = 9847
```

- [ ] **Step 3: Update `isConnected` property**

Update to use TCP:
```swift
var isConnected: Bool {
    getStatus() != nil
}
```
This already works since it calls `send(command:)` which now uses TCP.

- [ ] **Step 4: Build Swift**

```bash
cd macos/FortiVPNTray && swift build -c release
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor(swift): replace Unix socket IPC with TCP localhost:9847"
```

---

## Task 6: Cleanup and Remove Unix Socket References

**Files:**
- Modify: `src/main.rs` — remove cleanup_socket call if present
- Modify: `macos/FortiVPNTray/Sources/AppDelegate.swift` — remove ensureDaemonRunning Unix socket logic

- [ ] **Step 1: Check `src/main.rs` for cleanup_socket reference**

Search for `cleanup_socket` in `src/main.rs`. If present, remove it. The daemon no longer creates socket files.

- [ ] **Step 2: Update AppDelegate `ensureDaemonRunning`**

The Swift `ensureDaemonRunning` calls `state.client.isConnected` which now uses TCP — no changes needed. But if there's a `killDaemon` function that cleans up socket files, remove the file cleanup.

- [ ] **Step 3: Remove `dirs` from daemon Cargo.toml if unused**

Check if `dirs` is still used anywhere in `src/`. If `socket_path()` was the only user and `profile.rs` also uses `dirs::config_dir()`, keep it. Otherwise remove.

- [ ] **Step 4: Update CLAUDE.md**

Change IPC socket reference from:
```
- **IPC socket**: `~/Library/Application Support/fortivpn-tray/ipc.sock`
```
to:
```
- **IPC**: TCP `127.0.0.1:9847`
```

- [ ] **Step 5: Full CI check**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 6: Build Swift**

```bash
cd macos/FortiVPNTray && swift build -c release
```

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "chore: cleanup Unix socket references, update docs for TCP IPC"
```

---

## Task 7: Verify Cross-Platform Build

- [ ] **Step 1: Verify macOS build**

```bash
cargo build --release --workspace
cd macos/FortiVPNTray && swift build -c release
```

- [ ] **Step 2: Verify all tests pass**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 3: Integration test**

Start daemon manually, then test CLI and UI:
```bash
./target/release/fortivpn-daemon &
sleep 2
./target/release/fortivpn status
./target/release/fortivpn list
```

- [ ] **Step 4: Commit final**

```bash
git add -A && git commit -m "test: verify TCP IPC works end-to-end"
```
