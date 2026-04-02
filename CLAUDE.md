# CLAUDE.md — fortivpn-tray

## What This Is

A macOS system tray app for connecting to FortiGate SSL-VPN. Uses a **Swift UI + Rust daemon** architecture (Tailscale pattern). The VPN protocol is implemented natively in Rust — no dependency on `openfortivpn` or any external VPN binary. Includes a CLI companion (`fortivpn`) for terminal and AI assistant usage.

## Stack

- **macOS UI**: Swift (NSStatusItem, NSMenu, SwiftUI settings, NSAlert password prompt)
- **Daemon**: Rust + tokio (headless, IPC over TCP)
- **VPN protocol**: Native Rust (TLS auth, PPP session, async IP bridge, TUN device)
- **Password storage**: macOS Keychain via Swift `Security.framework` (UI) and `keyring` crate (daemon/CLI)
- **Privilege separation**: Helper daemon runs as root via `launchd` socket activation
- **IPC**: TCP `127.0.0.1:9847`
- **Cross-platform UI**: Rust + `tray-icon`/`muda` + `wry` for Windows/Linux (in `crates/fortivpn-ui/`)
- **Logging**: macOS unified logging (`os_log`) / `env_logger` on Linux/Windows

## Project Structure

```
fortivpn-tray/
├── macos/FortiVPNTray/           # Swift macOS app (UI only)
│   └── Sources/
│       ├── App.swift             # Entry point, accessory activation policy
│       ├── AppDelegate.swift     # Tray, menu, connect/disconnect, keychain, auto-reconnect
│       ├── VPNState.swift        # Observable state, keychain password check
│       ├── DaemonClient.swift    # IPC client (TCP, JSON)
│       ├── SettingsView.swift    # SwiftUI NavigationSplitView
│       ├── ProfileFormView.swift # Profile edit form
│       └── Models.swift          # Codable structs (VpnProfile, IpcResponse)
├── src/                           # Rust daemon
│   ├── main.rs                   # tokio runtime, logging init, IPC server start
│   ├── ipc.rs                    # IPC server + all command handlers
│   ├── vpn.rs                    # VpnManager state machine
│   ├── profile.rs                # ProfileStore (JSON persistence)
│   ├── keychain.rs               # OS credential store wrapper
│   ├── notification.rs           # Desktop notifications + distributed notifications
│   └── installer.rs              # Helper daemon installation via osascript
├── crates/
│   ├── fortivpn/                 # VPN protocol library (pure Rust, cross-platform)
│   ├── fortivpn-helper/          # Privileged helper (launchd daemon, runs as root)
│   ├── fortivpn-cli/             # CLI companion (fortivpn connect/disconnect/status)
│   └── fortivpn-ui/              # Cross-platform UI for Windows/Linux (tray-icon + wry)
├── resources/
│   ├── Info.plist                # macOS app bundle metadata
│   └── com.fortivpn-tray.helper.plist  # launchd socket activation config
├── icons/                         # App icon + tray icons (connected/disconnected)
├── install.sh                     # Cross-platform build + install
├── uninstall.sh                   # Cross-platform uninstall
├── Cargo.toml                     # Rust workspace root
└── build.rs                       # Helper binary build
```

## Data Storage

- **Profiles**: `~/Library/Application Support/fortivpn-tray/profiles.json`
- **Passwords**: macOS Keychain (service: `fortivpn-tray`, account: profile UUID)
- **IPC**: TCP `127.0.0.1:9847`
- **Helper socket**: `/var/run/fortivpn-helper.sock`

## Key Architecture

### Two-Process Model
- **Swift UI app** — owns all macOS UI (tray, menu, settings, password prompt, Keychain access)
- **Rust daemon** — owns VPN logic, profile storage, IPC server. No UI, no Keychain access on macOS

They communicate via TCP IPC on `127.0.0.1:9847` (JSON over newline-delimited text). The daemon also posts macOS distributed notifications (`CFNotificationCenter`) on state changes so the Swift app updates instantly without polling.

### VPN Connection Flow
1. User clicks profile in tray menu (or CLI `fortivpn connect <name>`)
2. Swift reads password from Keychain (has UI access for auth dialogs)
3. Swift sends `connect_with_password {"name":"...","password":"..."}` to daemon via IPC
4. Daemon connects to helper daemon at `/var/run/fortivpn-helper.sock`
5. Helper creates TUN device and passes fd back via SCM_RIGHTS
6. TLS connection to FortiGate gateway, HTTP auth to obtain SVPNCOOKIE
7. PPP session negotiated over TLS tunnel for IP configuration
8. Async IP bridge started between TUN device and PPP/TLS tunnel
9. Routes and DNS configured via helper, IPv6 disabled to prevent leaks
10. Daemon posts distributed notification → Swift updates tray icon
11. Event monitor watches for session death via tokio watch channel

### Privilege Separation
- Swift app and daemon run unprivileged
- Helper runs as root via launchd socket activation (installed once, started on demand)
- Helper exits after 30 seconds of inactivity, launchd restarts on next connection
- Helper socket at `/var/run/fortivpn-helper.sock` (mode 0666)

### Auto-Reconnect
- When VPN drops (error status), Swift auto-retries up to 3 times with 3-second delays
- Manual disconnect (clean `disconnected` status) does NOT trigger reconnect
- Swift reads password from Keychain for each retry

### Credential Isolation (macOS)
- Only Swift accesses macOS Keychain (has UI presence for auth dialogs)
- Daemon never reads Keychain — passwords passed via IPC `connect_with_password`
- CLI `connect` command still reads Keychain via `keyring` crate (works on Linux/Windows)

### Status Sync
- Daemon posts `com.fortivpn-tray.status-changed` distributed notification on every state change
- Swift listens via `DistributedNotificationCenter` — instant, zero polling
- Notification posted via `/usr/bin/swift` subprocess (tokio threads lack CFRunLoop)

### IPC Protocol
Text-based, one command per line, one JSON response per line:
```
status → {"ok":true,"data":{"status":"connected","profile":"MIMS SG"}}
list → {"ok":true,"data":[{"id":"...","name":"...","host":"...","port":443}]}
get_profiles → {"ok":true,"data":[{...full profile with username, trusted_cert...}]}
connect <name> → {"ok":true,"message":"Connected"}
connect_with_password <json> → {"ok":true,"message":"Connected"}
disconnect → {"ok":true,"message":"Disconnected"}
save_profile <json> → {"ok":true,"data":{"id":"..."}}
delete_profile <id> → {"ok":true,"message":"Deleted"}
set_password <id> <password> → {"ok":true,"message":"Password saved"}
has_password <id> → {"ok":true,"data":{"has_password":true}}
```

## Build & Run

```bash
cargo build --release --workspace   # Build daemon + helper + CLI
cargo test --workspace              # Run all 276 tests
cargo clippy --workspace -- -D warnings  # Lint
bash scripts/bundle-app.sh          # Build + assemble .app bundle (macOS)
./install.sh                        # Full build + install (auto-detects platform)
```

### Logging
```bash
# macOS: unified logging (Console.app)
log stream --predicate 'subsystem == "com.fortivpn-tray"' --level debug

# Linux/Windows: stderr
RUST_LOG=debug ./fortivpn-daemon
```

## CI/CD

- **CI**: GitHub Actions runs `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` on every push/PR to `main`
- **Release**: Pushing a version tag (`v*`) builds `.dmg` for Apple Silicon and Intel, creates GitHub Release
- **OpenSSF Scorecard**: Weekly security analysis, badge on README
- **Dependabot**: Weekly updates for Cargo and GitHub Actions dependencies
- **Branch protection**: `main` requires "Check & Test" status check to pass

## Commit Convention

Follow [Conventional Commits](https://www.conventionalcommits.org/) for all commit messages.

```
<type>(<scope>): <description>
```

**Types**: `feat`, `fix`, `refactor`, `docs`, `chore`, `test`, `ci`, `perf`

**Scopes** (optional): `vpn`, `helper`, `cli`, `ui`, `auth`, `routing`, `tray`, `ipc`, `build`

**Do not** add co-author signatures or trailers to commit messages.

## Gotchas

- The daemon binary is `fortivpn-daemon`, NOT `fortivpn-tray` (that was the old Tauri binary name)
- On macOS, only the Swift app should access Keychain — daemon Keychain access triggers blocked keyboard dialogs
- The `connect_with_password` IPC command uses JSON (not space-separated) to handle profile names with spaces and passwords with special characters
- Helper binary must be installed to `/Library/PrivilegedHelperTools/` with root:wheel ownership — `install.sh` handles this
- Distributed notifications require spawning a `/usr/bin/swift` subprocess because tokio worker threads lack a CFRunLoop
- The app hides from Dock using `NSApp.setActivationPolicy(.accessory)` — switches to `.regular` when settings window is open
- `keyring` crate (Rust) and Swift `Security.framework` read the same Keychain entries (service: `fortivpn-tray`)
- After rebuilding the daemon, macOS may prompt for Keychain access — click "Always Allow" or use the Swift app's connect flow which bypasses this
