# CLAUDE.md — fortivpn-tray

## What This Is

A cross-platform system tray app for connecting to FortiGate SSL-VPN. Built entirely in Rust — GPUI for the UI, tokio daemon for the VPN engine, platform-abstracted helper for privilege escalation. Implements the FortiVPN protocol natively — no dependency on `openfortivpn` or any external VPN binary. Includes a CLI companion (`fortivpn`) for terminal and AI assistant usage.

## Stack

- **UI**: Rust + GPUI (GPU-accelerated, cross-platform) + tray-icon/muda (system tray)
- **Daemon**: Rust + tokio (headless, TCP IPC on `127.0.0.1:9847`)
- **VPN protocol**: Native Rust (TLS auth, PPP session, async IP bridge, TUN device)
- **Password storage**: `keyring` crate (macOS Keychain / Windows Credential Manager / Linux Secret Service)
- **Privilege separation**: Helper binary runs as root (launchd on macOS, systemd on Linux, Windows Service stub)
- **IPC**: TCP `127.0.0.1:9847` (JSON over newline-delimited text)
- **Notifications**: `notify-rust` (cross-platform)
- **Logging**: `oslog` on macOS, `env_logger` on Linux/Windows

## Project Structure

```
fortivpn-tray/
├── Cargo.toml                        # Workspace root
├── crates/
│   ├── fortivpn/                     # VPN protocol library (cross-platform)
│   │   ├── src/
│   │   │   ├── lib.rs               # VpnSession orchestration
│   │   │   ├── auth.rs              # TLS + HTTP authentication
│   │   │   ├── bridge.rs            # Async IP bridge
│   │   │   ├── tunnel.rs            # FortiGate frame encoding
│   │   │   ├── ppp.rs              # PPP/LCP/IPCP protocol
│   │   │   ├── routing.rs          # Route + DNS (per-platform #[cfg])
│   │   │   ├── helper.rs           # Helper client (Unix: SCM_RIGHTS, Windows: stub)
│   │   │   ├── tun.rs              # TUN device creation (tun2)
│   │   │   └── async_tun.rs        # Async TUN wrapper (Unix only)
│   │   └── tests/                   # Integration tests
│   ├── fortivpn-daemon/              # Headless daemon (TCP IPC server)
│   │   ├── src/
│   │   │   ├── main.rs             # Entry point, logger init
│   │   │   ├── ipc.rs              # TCP server + command handlers + subscribe
│   │   │   ├── vpn.rs              # VPN state machine
│   │   │   ├── profile.rs          # Profile storage (JSON)
│   │   │   ├── installer.rs        # Helper installation (platform-specific)
│   │   │   └── notification.rs     # No-op (clients handle notifications)
│   │   └── build.rs                 # Helper binary build (Unix only)
│   ├── fortivpn-helper/              # Privileged helper (runs as root)
│   │   └── src/
│   │       ├── main.rs             # Platform dispatch
│   │       ├── commands.rs          # Shared: JSON commands, route/DNS
│   │       └── unix_main.rs        # Unix: launchd, SCM_RIGHTS
│   ├── fortivpn-cli/                 # CLI companion
│   │   └── src/main.rs             # connect/disconnect/status/set-password
│   └── fortivpn-app/                 # GPUI tray app (cross-platform UI)
│       └── src/
│           ├── main.rs             # GPUI app, tray icon, menu
│           ├── ipc_client.rs       # TCP IPC client + subscribe
│           ├── keychain.rs         # OS credential store (keyring)
│           └── notification.rs     # Desktop notifications (notify-rust)
├── resources/
│   ├── Info.plist                   # macOS bundle metadata
│   └── com.fortivpn-tray.helper.plist  # launchd daemon config
├── icons/                            # App + tray icons
├── install.sh                        # Cross-platform install
└── uninstall.sh                      # Cross-platform uninstall
```

## Data Storage

- **Profiles**: `~/Library/Application Support/fortivpn-tray/profiles.json` (macOS), `~/.config/fortivpn-tray/` (Linux), `%APPDATA%/fortivpn-tray/` (Windows)
- **Passwords**: OS credential store (service: `fortivpn-tray`, account: profile UUID)
- **IPC**: TCP `127.0.0.1:9847`
- **Helper socket**: `/var/run/fortivpn-helper.sock` (macOS/Linux)

## Key Architecture

### Two-Process Model
- **GPUI app** — owns all UI (tray, menu, settings, password prompt, keychain, notifications)
- **Rust daemon** — owns VPN logic, profile storage, TCP IPC server. No UI, no keychain access.

They communicate via TCP `127.0.0.1:9847`. The daemon pushes status events via the `subscribe` channel (persistent TCP connection) for instant UI updates without polling.

### VPN Connection Flow
1. User clicks profile in tray menu (or CLI `fortivpn connect <name>`)
2. GPUI app reads password from keychain via `keyring` crate
3. Sends `connect_with_password {"name":"...","password":"..."}` to daemon via TCP
4. Daemon connects to helper daemon at `/var/run/fortivpn-helper.sock`
5. Helper creates TUN device and passes fd back via SCM_RIGHTS (Unix)
6. TLS connection to FortiGate gateway, HTTP auth to obtain SVPNCOOKIE
7. PPP session negotiated over TLS tunnel for IP configuration
8. Async IP bridge started between TUN device and PPP/TLS tunnel
9. Routes and DNS configured via helper, IPv6 disabled to prevent leaks
10. Daemon sends status event via subscribe broadcast → UI updates tray icon
11. Event monitor watches for session death via tokio watch channel

### Credential Isolation
- GPUI app and CLI own all keychain access via `keyring` crate
- Daemon never reads credentials — passwords passed via IPC `connect_with_password`
- This avoids macOS Secure Keyboard Entry blocking issues

### Status Sync
- Daemon has a `tokio::sync::broadcast` channel for status events
- UI subscribes via `subscribe` TCP command — persistent connection, instant delivery
- Events: connected, disconnected, error (with reason)

### Auto-Reconnect
- When VPN drops (error status), GPUI app retries up to 3 times with 3-second delays
- Manual disconnect (clean `disconnected` status) does NOT trigger reconnect

### IPC Protocol
Text-based, one command per line, one JSON response per line:
```
status → {"ok":true,"data":{"status":"connected","profile":"MIMS SG"}}
connect_with_password <json> → {"ok":true,"message":"Connected"}
disconnect → {"ok":true,"message":"Disconnected"}
subscribe → (persistent: pushes {"event":"status","data":{...}} lines)
get_profiles / save_profile / delete_profile → profile CRUD
list → profile list (for CLI)
```

## Build & Run

```bash
cargo build --release --workspace   # Build everything
cargo test --workspace              # Run all 270 tests
cargo clippy --workspace -- -D warnings  # Lint
./install.sh                        # Build + install (auto-detects platform)
```

### Logging
```bash
# macOS: Console.app
log stream --predicate 'subsystem == "com.fortivpn-tray"' --level debug

# Linux/Windows: stderr
RUST_LOG=debug ./fortivpn-daemon
```

## CI/CD

- **CI**: GitHub Actions runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` on every push/PR to `main`
- **Release**: Pushing a version tag (`v*`) builds for Apple Silicon and Intel, creates GitHub Release
- **OpenSSF Scorecard**: Weekly security analysis
- **Dependabot**: Weekly updates for Cargo and GitHub Actions dependencies
- **Branch protection**: `main` requires "Check & Test" to pass

## Commit Convention

Follow [Conventional Commits](https://www.conventionalcommits.org/).

```
<type>(<scope>): <description>
```

**Types**: `feat`, `fix`, `refactor`, `docs`, `chore`, `test`, `ci`, `perf`

**Scopes**: `vpn`, `helper`, `cli`, `ui`, `app`, `auth`, `routing`, `tray`, `ipc`, `build`

**Do not** add co-author signatures or trailers to commit messages.

## Gotchas

- The daemon binary is `fortivpn-daemon`, the UI app is `fortivpn-app`
- IPC is TCP `127.0.0.1:9847` (not Unix sockets)
- Daemon never accesses keychain — all credential access is in the GPUI app and CLI
- `connect_with_password` uses JSON format to handle profile names with spaces and passwords with special characters
- Helper binary must be installed with root privileges — `install.sh` handles this per platform
- VPN library uses `#[cfg(unix)]` / `#[cfg(windows)]` — Windows has stubs, full implementation pending
- `build.rs` in fortivpn-daemon skips helper build on Windows targets
- Tests use `ProfileStore::in_memory()` to avoid writing to real profiles file
