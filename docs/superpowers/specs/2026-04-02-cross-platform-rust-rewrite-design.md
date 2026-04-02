# Cross-Platform Rust Rewrite — Design Spec

## Problem

The project currently has a macOS-first architecture: Swift UI app, launchd helper, Unix socket IPC, macOS-specific keychain and notification code. The cross-platform UI (`crates/fortivpn-ui/`) using wry/tao is a second-class citizen. Windows builds fail entirely due to Unix-only dependencies throughout the codebase.

## Goal

Rewrite everything in pure Rust. Single codebase that builds and runs natively on macOS, Windows, and Linux. Use GPUI for the settings UI, tray-icon/muda for the system tray, TCP localhost for IPC, and platform-abstracted helpers for privilege escalation.

## Architecture

```
┌─────────────────────────────────┐
│  GPUI App (single Rust binary)  │
│  ├── System tray (tray-icon)    │
│  ├── Settings window (GPUI)     │
│  ├── Password prompt (GPUI)     │
│  ├── Keychain access (keyring)  │
│  ├── Notifications (notify-rust)│
│  └── TCP IPC client             │
└──────────┬──────────────────────┘
           │ TCP localhost:9847
┌──────────▼──────────────────────┐
│  Daemon (Rust, tokio)           │
│  ├── TCP IPC server             │
│  ├── VPN protocol (fortivpn)    │
│  ├── Profile storage (JSON)     │
│  └── Subscribe broadcast        │
└──────────┬──────────────────────┘
           │ Platform-specific
┌──────────▼──────────────────────┐
│  Privileged Helper              │
│  ├── TUN creation (tun2)        │
│  ├── Route management           │
│  ├── DNS configuration          │
│  └── launchd / systemd /        │
│       Windows Service           │
└─────────────────────────────────┘
```

## Design Decisions

| Decision | Choice | Reason |
|----------|--------|--------|
| Process model | Two-process (UI + daemon) | CLI works independently, daemon runs headless |
| IPC transport | TCP localhost:9847 | Zero platform code, loopback is kernel-shortcut (no network overhead) |
| UI framework | GPUI + gpui-component | GPU-accelerated, native Rust, cross-platform, same framework as Zed editor |
| Tray icon | tray-icon + muda crates | Already proven cross-platform (macOS, Windows, Linux) |
| Status sync | TCP subscribe (persistent connection) | Zero polling, instant delivery, same socket as commands |
| Notifications | notify-rust in UI app | Works on all platforms, UI owns all user-facing output |
| Credential storage | keyring crate in UI + CLI | Cross-platform (macOS Keychain, Windows Credential Manager, Linux Secret Service). Daemon never accesses credentials. |
| Helper startup | Platform-specific (launchd/systemd/Windows Service) | Most native, best security model per OS |
| Helper core logic | Shared Rust (JSON commands over socket) | 80% shared code, 20% platform-specific (route/DNS commands) |
| Logging | env_logger everywhere | Simple, cross-platform, controlled via RUST_LOG env var |

## Components

### 1. GPUI App (`crates/fortivpn-app/`)

Single Rust binary providing all UI. GPUI owns the event loop. Tray icon registered within the GPUI app.

**File structure:**
```
crates/fortivpn-app/
├── Cargo.toml
└── src/
    ├── main.rs             # GPUI app init, tray icon setup, event loop
    ├── tray.rs             # Tray menu building, click handlers
    ├── settings.rs         # Settings window (GPUI views)
    ├── password_prompt.rs  # Password dialog (GPUI views)
    ├── ipc_client.rs       # TCP IPC client + subscribe listener
    ├── keychain.rs         # keyring crate wrapper (read/write/check)
    └── notification.rs     # notify-rust wrapper
```

**Tray icon:** Uses `tray-icon` + `muda` (same crates as current). Menu rebuilds on click via callback. Profile items show connect/disconnect. Settings and Quit at bottom. No status line.

**Settings window:** GPUI views using `gpui-component` widgets:
- Left panel: scrollable profile list with colored status dots
- Right panel: form (TextInput for name/host/port/username/cert, SecureInput for password)
- Fetch Certificate button
- Save/Delete/New Profile buttons
- Opens on demand, destroyed on close

**Password prompt:** Small GPUI window (~320x180):
- SecureInput for password
- Checkbox for "Remember password"
- Connect/Cancel buttons
- Opens when connecting without stored password

**Connect flow:**
1. User clicks profile in tray menu
2. App reads password from keychain via `keyring`
3. If no password → show GPUI password prompt → store if "remember" checked
4. Send `connect_with_password {"name":"...","password":"..."}` via TCP
5. Listen for result on subscribe connection
6. Show notification on success/failure, update icon

**Subscribe listener:** Background thread connects to daemon TCP port, sends `subscribe`, reads JSON lines. On status change → updates tray icon, shows notification. On error with was-connected → auto-reconnect up to 3 times with 3s delay.

**Notifications:** Via `notify-rust`. Called by the UI app when:
- Connected (from tray click or CLI or auto-reconnect)
- Disconnected (manual or drop)
- Connection failed
- Reconnecting (attempt N/3)

### 2. Daemon (`src/` → renamed workspace root)

Headless Rust process running tokio async runtime. Serves IPC commands over TCP.

**Changes from current:**
- Replace `UnixListener` with `TcpListener` on `127.0.0.1:9847`
- Remove `notification.rs` — `post_distributed_notification` no longer needed (subscribe replaces it)
- Remove `send_notification` calls — UI handles all notifications
- Replace `oslog` with `env_logger` on all platforms
- `build.rs` — conditional helper build (skip on platforms where helper is built separately)
- `installer.rs` — platform-specific behind `#[cfg]`:
  - macOS: launchctl bootstrap (current)
  - Linux: systemctl enable
  - Windows: sc.exe create / Start-Service

**IPC commands (unchanged):**
`status`, `list`, `get_profiles`, `save_profile`, `delete_profile`, `connect_with_password`, `disconnect`, `subscribe`

### 3. VPN Library (`crates/fortivpn/`)

Already 90% cross-platform. Needs `#[cfg]` additions:

| File | macOS/Linux | Windows |
|------|------------|---------|
| `helper.rs` | Unix socket to helper | Named pipe or TCP to helper |
| `routing.rs` routes | `/sbin/route` (macOS), `ip route` (Linux) | `route.exe` |
| `routing.rs` DNS | `scutil` (macOS), `resolvconf` (Linux) | `netsh interface ip set dns` |
| `routing.rs` IPv6 | `networksetup -setv6off` (macOS) | `netsh interface ipv6 set ...` |
| `async_tun.rs` | `AsyncFd<OwnedFd>` | wintun async wrapper |
| `tun.rs` | Already cross-platform via `tun2` | Already cross-platform via `tun2` |

### 4. Privileged Helper (`crates/fortivpn-helper/`)

**File structure:**
```
crates/fortivpn-helper/
└── src/
    ├── main.rs               # Entry point, dispatch to platform startup
    ├── commands.rs            # Shared: JSON command parsing, TUN creation, route/DNS
    ├── platform_macos.rs      # launchd socket activation, SCM_RIGHTS fd passing
    ├── platform_linux.rs      # systemd socket or CLI arg, SCM_RIGHTS fd passing
    └── platform_windows.rs    # Windows Service, wintun (no fd passing needed)
```

**Shared (`commands.rs`):** ~80% of logic
- Parse JSON commands
- Create TUN device via `tun2`
- Execute route add/delete (platform-dispatched)
- Execute DNS configure/restore (platform-dispatched)
- Send JSON responses

**Platform-specific (~20%):**
- macOS: `launch_activate_socket()` FFI, `SCM_RIGHTS` fd passing, 30s idle timeout
- Linux: Accept connection on Unix socket (systemd-provided or CLI arg), `SCM_RIGHTS`
- Windows: Windows Service Control Manager registration, wintun handle (no fd passing — wintun creates the adapter directly, handle is process-local)

**Communication with daemon:**
- macOS/Linux: Unix socket at well-known path, fd passing via SCM_RIGHTS
- Windows: Named pipe or TCP localhost on a helper-specific port. Wintun doesn't need fd passing — the daemon loads the wintun driver directly (no helper needed for TUN creation on Windows, only for route/DNS which need admin).

### 5. CLI (`crates/fortivpn-cli/`)

Replace `UnixStream` with `TcpStream` to `127.0.0.1:9847`. Same commands, same keychain logic (already updated in prior task). Fully cross-platform.

### 6. Install Scripts

**macOS (`install.sh`):**
- Build daemon + app + helper + CLI
- Create .app bundle
- Install helper via launchctl

**Linux (`install.sh`):**
- Build daemon + app + helper + CLI
- Install to `~/.local/bin/`
- Install helper systemd unit
- Create .desktop entry

**Windows (`install.ps1`):**
- Build daemon + app + CLI
- Install to `%LOCALAPPDATA%\FortiVPN Tray\`
- Register helper as Windows Service
- Create Start Menu shortcut

## What Gets Deleted

| Path | Reason |
|------|--------|
| `macos/FortiVPNTray/` | Entire Swift app — replaced by GPUI app |
| `crates/fortivpn-ui/` | Old wry/tao UI — replaced by GPUI app |
| `resources/settings/` | Old HTML settings — replaced by GPUI views |
| `resources/Info.plist` | macOS bundle metadata — moved into install script |
| `scripts/bundle-app.sh` | macOS-specific — replaced by cross-platform install scripts |

## What Gets Reused

| Path | Status |
|------|--------|
| `crates/fortivpn/` | Core VPN library — add `#[cfg]` for Windows |
| `src/ipc.rs` | Daemon IPC — change Unix socket to TCP |
| `src/vpn.rs` | VPN manager — unchanged |
| `src/profile.rs` | Profile storage — unchanged |
| `tests/` | Integration tests — unchanged |

## Energy Model

- **GPUI app idle (no windows):** `ControlFlow::Wait` — GPU context initialized but no rendering. Near-zero CPU.
- **Subscribe connection:** TCP socket blocked on `read()` — zero CPU until event arrives.
- **Daemon idle:** `TcpListener::accept().await` — zero CPU.
- **Helper idle:** Exits after 30s inactivity (macOS/Linux). Windows Service idles with zero CPU.

## Error Handling

- Daemon not running when UI starts: try to spawn from same directory, retry 5 times with 0.5s delay
- TCP connection refused: daemon not running, show "Daemon not running" in tray menu
- Subscribe connection drops: reconnect after 1s, retry up to 5 times
- Connect timeout (30s): show "Connection timed out" notification
- Keychain read fails: show password prompt

## Testing

- Existing VPN protocol tests (`crates/fortivpn/`) — keep all, add `#[cfg]` where needed
- Daemon IPC tests — update to use TCP instead of Unix socket
- Profile store tests — unchanged (already isolated with `in_memory`)
- GPUI UI — manual testing (no automated UI tests)
- Cross-platform CI: GitHub Actions matrix (macOS, Ubuntu, Windows)
