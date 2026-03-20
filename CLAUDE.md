# CLAUDE.md — fortivpn-tray

## What This Is

A macOS menu bar app for connecting to FortiGate SSL-VPN. Built with Tauri 2 (Rust backend, plain HTML/JS/CSS frontend). Implements the FortiVPN protocol natively in Rust — no dependency on `openfortivpn` or any external VPN binary. Includes a CLI companion (`fortivpn`) for terminal usage.

## Stack

- **Backend**: Rust + Tauri 2 + tokio (async I/O, TLS, PPP, IP bridging)
- **Frontend**: Plain HTML/JS/CSS (no framework, no bundler). Served as static files from `src/`.
- **Password storage**: macOS Keychain via `keyring` crate
- **VPN protocol**: Native Rust implementation (TLS auth, PPP session, TUN bridge)
- **Privilege separation**: Helper process runs as root via `osascript`, passes TUN fd via SCM_RIGHTS
- **IPC**: Unix domain socket at `~/.config/fortivpn-tray/ipc.sock` (CLI talks to tray app)

## Project Structure

```
src/
  src/
    lib.rs         — App setup, tray menu, Tauri commands, window management
    vpn.rs         — VPN connection lifecycle (connect/disconnect/check_alive)
    profile.rs     — Profile CRUD, JSON persistence
    keychain.rs    — macOS Keychain read/write/delete
    ipc.rs         — Unix socket IPC server for CLI
    main.rs        — Entry point (delegates to lib.rs)
  crates/
    fortivpn/      — Core VPN library (protocol, auth, tunneling, routing)
      src/
        lib.rs       — VpnSession, FortiError, connection orchestration
        auth.rs      — TLS + HTTP authentication, SVPNCOOKIE
        tunnel.rs    — TLS tunnel management
        tun.rs       — TUN device abstraction
        async_tun.rs — AsyncRead/AsyncWrite wrapper for TUN fd
        ppp.rs       — PPP protocol framing and negotiation
        bridge.rs    — Async IP bridge (TUN <-> PPP/TLS)
        routing.rs   — Route/DNS config, IPv6 leak prevention
        helper.rs    — HelperClient (Unix socket comms with privileged helper)
    fortivpn-helper/ — Privileged helper binary (TUN creation, route/DNS commands)
      src/main.rs
    fortivpn-cli/    — CLI companion binary
      src/main.rs

src/
  index.html     — Settings UI window
  main.js        — Settings UI logic (profile CRUD via Tauri invoke)
  styles.css     — macOS-native styling with dark mode

.github/workflows/
  ci.yml         — Lint + test on push/PR
  release.yml    — Build .dmg + GitHub Release on version tags
```

## Data Storage

- **Profiles**: `~/Library/Application Support/fortivpn-tray/profiles.json`
- **Passwords**: macOS Keychain (service: `fortivpn-tray`, account: profile UUID)
- **IPC socket**: `~/.config/fortivpn-tray/ipc.sock`

## Key Architecture

### VPN Connection Flow
1. User clicks profile in tray menu (or CLI `fortivpn connect <name>`)
2. Password retrieved from Keychain
3. Helper process spawned via `osascript` (one-time macOS admin password prompt)
4. Helper creates TUN device and passes fd back via SCM_RIGHTS over Unix socket
5. TLS connection to FortiGate gateway, HTTP auth to obtain SVPNCOOKIE
6. PPP session negotiated over TLS tunnel for IP configuration
7. Async IP bridge started between TUN device and PPP/TLS tunnel
8. Routes and DNS configured via helper, IPv6 disabled to prevent leaks
9. Background monitor checks session health every 5 seconds

### Privilege Separation
- Main app runs unprivileged
- Helper runs as root, handles: TUN creation, route/DNS commands, fd passing
- Helper stays alive across connect/disconnect cycles (one password prompt per session)
- Helper shut down on app exit via `Drop` impl on `VpnManager`

### Error Handling
- `VpnStatus::Error(String)` captures connection failures
- Errors shown in tray menu (word-wrapped) and as macOS notifications
- Error state allows reconnect without restart

### Settings Window
- Singleton window opened from "Settings..." tray menu item
- Tauri commands: `get_profiles`, `save_profile`, `delete_profile`, `cmd_set_password`, `has_password`
- All mutating commands rebuild the tray menu

### Tray Menu
- Profile items: `● Name — Disconnect` (connected) or `○ Name — Connect` (disconnected)
- Status line with error wrapping across multiple menu items
- "Settings..." opens/focuses the settings window
- "Quit" disconnects first, cleans up IPC socket

## Build & Run

```bash
cargo build --release        # Build all binaries
cargo test --workspace       # Run all tests
bash scripts/bundle-app.sh   # Create .app bundle
```

### CLI companion
```bash
cargo build --release --bin fortivpn
sudo cp src/target/release/fortivpn /usr/local/bin/
```

### CLI Usage
```bash
fortivpn status          # Show VPN status
fortivpn list            # List profiles
fortivpn connect <name>  # Connect (partial match: `fortivpn sg`)
fortivpn disconnect      # Disconnect
fortivpn set-password    # Interactive password update
```

## CI/CD

- **CI**: GitHub Actions runs `cargo fmt --check`, `cargo clippy`, and `cargo test --workspace` on every push/PR to `main`
- **Release**: Pushing a version tag (`v*`) builds `.dmg` for both Apple Silicon and Intel, and creates a GitHub Release

## Commit Convention

Follow [Conventional Commits](https://www.conventionalcommits.org/) for all commit messages. These are used to auto-generate release notes.

```
<type>(<scope>): <description>

[optional body]
```

**Types**: `feat`, `fix`, `refactor`, `docs`, `chore`, `test`, `ci`, `perf`

**Scopes** (optional): `vpn`, `helper`, `cli`, `ui`, `auth`, `routing`, `tray`

**Examples**:
```
feat(vpn): add certificate pinning verification
fix(helper): handle TUN device creation failure gracefully
refactor(auth): extract cookie parsing into separate function
docs: update README with architecture diagram
ci: add release workflow for macOS builds
chore: update dependencies
```

**Do not** add co-author signatures or trailers to commit messages.

## Gotchas

- The `Cargo.toml` has `default-run = "fortivpn-tray"` — required because there are two binaries (tray app + CLI)
- The app hides from Dock using `NSApplicationActivationPolicy::Accessory` — must be set AFTER Tauri init
- macOS Menu Bar settings can accumulate ghost entries from dev builds — use "Reset Control Center..." in System Settings to clean up
- `keyring` crate stores passwords differently than `security` CLI — always use `fortivpn set-password` or the Settings UI to update passwords
- Helper binary is embedded at `src/crates/fortivpn-helper` — it must be built and accessible for VPN connections to work
