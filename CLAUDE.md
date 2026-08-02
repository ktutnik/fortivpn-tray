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
7. PPP session negotiated over TLS tunnel — returns `PppSession` (IP, magic number, DNS, **MTU**)
8. Routes and DNS configured via helper, IPv6 disabled to prevent leaks
9. Async IP bridge started between TUN device and PPP/TLS tunnel
10. Daemon sends status event via subscribe broadcast → UI updates tray icon
11. Event monitor watches for session death via tokio watch channel

Note steps 5 and 8: on a reconnect that adopts a `PreservedNetwork`, both are skipped — the TUN device and routes are already in place. Routes are configured *before* the bridge starts so a failure there returns an error without leaking bridge tasks.

### Data Path Performance
- **MTU comes from the gateway**, not a constant: the peer's LCP MRU option (`LcpState::peer_mru`) capped by our own MRU — see `LcpState::negotiated_mtu()`. `ppp::DEFAULT_MRU` (1354) applies only when the gateway advertises nothing. The daemon logs the negotiated value on connect (`Connected to <profile> — tunnel MTU <n>`); check it first when large transfers stall
- **Writes are batched**: `tunnel_writer_loop` drains up to `WRITE_BATCH_LIMIT` (32) packets from the mpsc with `recv_many`, encodes them into one reusable buffer via `encode_frame_into`, then does one `write_all` + one `flush`. Previously it was one TLS record, one syscall and 3 heap allocations *per packet* with `TCP_NODELAY` set, so bursts never coalesced
- **Still unaddressed**: transport is TLS-over-TCP only, no DTLS. Inner retransmits stack on outer retransmits, so throughput collapses on a lossy underlay — FortiClient uses DTLS over UDP/10443 and sidesteps this entirely. Measure the underlay (`ping` the LAN router and the gateway) before blaming the tunnel

### Credential Isolation
- GPUI app and CLI own all keychain access via `keyring` crate
- Daemon never reads credentials — passwords passed via IPC `connect_with_password`
- This avoids macOS Secure Keyboard Entry blocking issues

### Status Sync
- Daemon has a `tokio::sync::broadcast` channel for status events
- UI subscribes via `subscribe` TCP command — persistent connection, instant delivery
- Events: connected, disconnected, error (with reason)

### Auto-Reconnect
- Daemon-side: the session monitor (`spawn_session_monitor` in `ipc.rs`) retries up to 5 times with exponential backoff (3s → 48s) using the password retained in `VpnManager.session_passwords` for the session
- Status broadcast during retries is `reconnecting`; the GPUI app shows notifications on drop/reconnected/gave-up transitions
- Manual disconnect (clean `disconnected` status) does NOT trigger reconnect; a manual connect/disconnect during backoff cancels the retry loop
- Dead-link detection: LCP echo every 10s, declared dead after 6 echo intervals with no inbound frame at all (~60s). Any inbound frame resets the counter — many FortiGate gateways never answer client echo requests
- Routes, DNS and the TUN device stay installed across a reconnect (`VpnSession::disconnect_preserving_network` → `PreservedNetwork` → `connect_reusing`). A flap is then a brief stall instead of a host-wide route teardown that kills every open connection. Adopted only when host, port, assigned IP and peer IP all match; otherwise the old state is released and rebuilt
- `PreservedNetwork` holds a **duplicate of the TUN fd** because the kernel deletes every route bound to a tun interface as soon as its last descriptor closes — keeping the routes requires keeping the interface open. Unix only; Windows rebuilds the interface on reconnect
- Whoever abandons a reconnect MUST call `release_preserved_network()` (out of attempts, no profile/password, manual disconnect) or the host is left routing into a dead tunnel

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
cargo test --workspace              # Run all 303 tests
cargo clippy --workspace -- -D warnings  # Lint
./install.sh                        # Build + install (auto-detects platform)
```

Only `fortivpn-daemon` depends on the `fortivpn` crate. A change confined to the
VPN library needs **no `sudo` and no helper reinstall** — rebuild the daemon,
copy it into `/Applications/FortiVPN Tray.app/Contents/MacOS/`, relaunch the app.

## Diagnostics

### `daemon.log` — the primary record

`~/Library/Application Support/fortivpn-tray/daemon.log` (config dir per platform).
Millisecond local timestamps, rotated at 5 MB keeping 3 generations, appended
across restarts. **This is where you look first for any drop or failed connect.**

```bash
tail -f ~/Library/Application\ Support/fortivpn-tray/daemon.log
log stream --predicate 'subsystem == "com.fortivpn-tray"' --level debug  # live, macOS
```

**Do not rely on oslog for anything historical.** macOS holds Info/Debug entries
in a memory ring buffer and purges them within *minutes* — measured 2026-08-01,
`log show --last 3h` returned zero lines for an event logged 25 minutes earlier.
That is why the file exists; it is not redundant with `log stream`.

`logging.rs` caps third-party targets at Info (`level_for`) while `vpn`, `ipc`
and `daemon` log at Debug. Without that cap rustls emits six debug lines per
handshake and buries everything during a reconnect loop.

### What gets logged

- **Connect, phase by phase, with timings** — `auth → tls connected → tunnel opened → ppp negotiated → tun created → routes configured → ESTABLISHED`. On failure the last phase logged is the one that failed, with the real `FortiError`. A typical healthy connect is ~3.5 s, dominated by a fixed 2 s wait in `open_tunnel`
- **Heartbeat every 60 s while connected** — `up 60s | in 697 frames/256 KB | out 744 frames/427 KB | echo 6 sent/5 replied | last inbound 0.9s ago`. The minute before a drop is what diagnoses it
- **Death with vitals** — `declare_dead()` logs the cause *and* the same counters. "Silent for 60 s then killed" and "died mid-transfer at 2 MB/s" are different bugs that previously produced identical messages
- **Real error text** — read and write failures carry the underlying `io::Error` and its `ErrorKind`. `FrameReader` distinguishes a clean close (`tunnel closed`) from a reset, a timeout, and a framing desync; that distinction separates "the gateway hung up" from "our Wi-Fi died"
- **Reconnect trail** — attempt number, backoff, per-attempt error, and why a reconnect stood down

Credentials and the SVPNCOOKIE are never logged. The account username is, since
connecting as the wrong account is itself a failure mode worth seeing.

### Measured behaviour of the MIMS SG gateway (2026-08-01)

- **Replies to LCP echoes** — `echo 6 sent/5 replied`. The comment claiming FortiGates commonly ignore client echoes does not hold for this one
- **Advertises no LCP MRU** — `negotiated_mtu()` falls back to `DEFAULT_MRU`, so the tunnel runs at 1354. FortiClient's 1300 is its own default, not gateway-driven. A `ping -D` sweep confirmed 1354-byte packets pass unfragmented, so MTU is not a bottleneck here
- **Pushes no split-tunnel routes** — hence full tunnel, hence *all* host traffic takes the detour

### Performance baseline, same machine and minute

| | RTT to 1.1.1.1 | Single stream | 4× parallel | TCP retransmits |
|---|---|---|---|---|
| No VPN | 27–37 ms | ~2800 KB/s | ~4200 KB/s | 0 |
| fortivpn-tray | 219 ms | 403 KB/s | 1158 KB/s | 0 |
| FortiClient | 123–1159 ms | 46–54 KB/s | — | 0 |

The slowdown is **dominated by round-trip time, not by client code**: 92 ms just
to reach the gateway (traceroute leaves Biznet ID and enters SG via AS6453), and
single-stream TCP throughput is inversely proportional to RTT — 8× the RTT
predicts ~7× slower, measured 6.9×. **FortiClient measured ~8× slower than this
client** on the same gateway, so the bottleneck is server-side.

Zero retransmits in every run means **no TCP-over-TCP collapse is occurring** on a
clean link, so DTLS/UDP would buy little today — it matters only when the underlay
loses packets. Split tunnelling, not transport, is the fix for "the whole PC is slow".

**Measure the underlay before blaming the tunnel.** `ping -c 20 <router>` must be
clean first; a link dropping 20% of packets to its own router (observed earlier
the same day) makes every other number meaningless. Block-ordered A/B tests are
also untrustworthy while a link is recovering — interleave the conditions.

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
