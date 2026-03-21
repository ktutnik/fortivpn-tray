<p align="center">
  <img src="icons/icon.png" width="128" height="128" alt="FortiVPN Tray icon">
</p>

<h1 align="center">FortiVPN Tray</h1>

<p align="center">
  A lightweight macOS system tray app for FortiGate SSL-VPN — built with Swift and Rust.
</p>

<p align="center">
  <img alt="macOS" src="https://img.shields.io/badge/macOS-000000?style=flat&logo=apple&logoColor=white">
  <img alt="Swift" src="https://img.shields.io/badge/Swift-F05138?style=flat&logo=swift&logoColor=white">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white">
  <img alt="License" src="https://img.shields.io/badge/license-MIT-blue.svg">
</p>

---

## Motivation

Connecting to a FortiGate SSL-VPN usually means either running the heavy FortiClient app or wrestling with `openfortivpn` in the terminal with `sudo`. Both have friction:

- **FortiClient** is bloated, installs kernel extensions, and runs background services you don't need.
- **openfortivpn** requires `sudo` for every connection, a config file, and terminal babysitting.

FortiVPN Tray takes a different approach — a **lightweight system tray app** that implements the FortiGate SSL-VPN protocol natively in Rust. No subprocess wrapping, no kernel extensions, no bloat. Just click to connect.

## Design

### Architecture

The app follows the **Tailscale pattern** — a native Swift UI app communicating with a Rust daemon over a Unix socket.

```mermaid
graph TD
    subgraph swift["Swift App (native macOS UI)"]
        tray["NSStatusItem (tray icon)"]
        menu["NSMenu (tray menu)"]
        settings["SwiftUI (settings window)"]
        alert["NSAlert (password prompt)"]
    end

    swift -- "Unix Socket IPC<br/>(JSON commands)" --> daemon

    subgraph daemon["Rust Daemon (headless)"]
        ipc["IPC Server"]
        vpnmgr["VPN Manager"]
        profiles["Profile Store"]
        kc["Keychain"]
        ipc --> vpnmgr
        ipc --> profiles
        ipc --> kc
    end

    subgraph cli["CLI Companion"]
        cliprog["fortivpn"]
    end

    cli -- "Unix Socket IPC" --> daemon

    vpnmgr --> crate

    subgraph crate["fortivpn crate (pure Rust)"]
        auth["TLS + Auth"]
        ppp["PPP Session"]
        bridge["IP Bridge"]
        route["Routing + DNS"]
        auth --> ppp --> bridge
        auth --> route
    end

    crate -- "Unix Socket" --> helper

    subgraph helper["Privileged Helper (launchd daemon)"]
        tun["TUN Device Creation"]
        fd["fd passing (SCM_RIGHTS)"]
        ops["Route / DNS Commands"]
    end

    style swift fill:#F05138,stroke:#c43e2d,color:#fff
    style daemon fill:#0ea5e9,stroke:#0284c7,color:#fff
    style crate fill:#2563eb,stroke:#1d4ed8,color:#fff
    style helper fill:#7c3aed,stroke:#6d28d9,color:#fff
    style cli fill:#6b7280,stroke:#4b5563,color:#fff
```

### Key Design Decisions

- **Swift + Rust split** — Swift owns all UI (tray icon, menu, settings window, password prompt). Rust runs as a headless daemon handling VPN protocol, profile storage, keychain access, and IPC. They communicate over a Unix domain socket using JSON commands.

- **Native Rust protocol implementation** — TLS, HTTP auth, PPP framing, and IP bridging are all implemented from scratch. No dependency on `openfortivpn` or any external VPN binary.

- **Near-zero battery drain** — No polling, no WebKit, no background timers. The Swift app sleeps completely when idle. The Rust daemon blocks on socket `accept()`. State refreshes only when you interact with the tray menu.

- **Privilege separation** — Only the helper process runs with elevated privileges (as a launchd daemon). It creates the TUN device and passes the file descriptor back via `SCM_RIGHTS`. The main app stays unprivileged.

- **Persistent helper** — The privileged helper is managed by launchd with socket activation. No repeated admin password prompts — install once, connect forever.

- **IPv6 leak prevention** — Automatically disables IPv6 on active interfaces when the VPN connects to prevent traffic leaking outside the tunnel, and restores it on disconnect.

- **Secure credential storage** — VPN passwords are stored in macOS Keychain, never on disk.

### Project Structure

```
fortivpn-tray/
├── macos/FortiVPNTray/          # Swift macOS app
│   ├── Sources/
│   │   ├── App.swift            # @main entry point
│   │   ├── AppDelegate.swift    # NSStatusItem, NSMenu, tray icon
│   │   ├── VPNState.swift       # Observable state (profiles, status)
│   │   ├── DaemonClient.swift   # Unix socket IPC client
│   │   ├── SettingsView.swift   # SwiftUI settings window
│   │   ├── ProfileFormView.swift # SwiftUI profile edit form
│   │   └── Models.swift         # Codable data models
│   └── Package.swift
├── src/                          # Rust daemon
│   ├── main.rs                  # Daemon entry point (tokio runtime)
│   ├── ipc.rs                   # Unix socket IPC server + command handlers
│   ├── vpn.rs                   # VPN connection lifecycle
│   ├── profile.rs               # Profile CRUD, JSON persistence
│   ├── keychain.rs              # macOS Keychain read/write/delete
│   ├── installer.rs             # Helper daemon installation
│   └── notification.rs          # Desktop notifications
├── crates/
│   ├── fortivpn/                # Core VPN library (protocol, auth, tunneling)
│   ├── fortivpn-helper/         # Privileged helper binary (TUN + routing)
│   └── fortivpn-cli/            # CLI companion tool
├── resources/
│   ├── Info.plist               # macOS app bundle metadata
│   └── com.fortivpn-tray.helper.plist  # launchd daemon config
├── scripts/
│   └── bundle-app.sh            # Build + assemble .app bundle
├── icons/                        # App and tray icons
├── Cargo.toml                    # Rust workspace root
└── build.rs                      # Build script (helper binary)
```

## Features

- One-click connect/disconnect from the system tray
- Near-zero battery drain when idle (no polling, no WebKit)
- Native SwiftUI settings with dark mode support
- Native password prompt on first connect
- Multiple VPN profile support
- CLI companion for terminal workflows
- Secure credential storage (macOS Keychain)
- Native desktop notifications
- IPv6 leak prevention
- No external VPN binaries required

## Prerequisites

- [Rust toolchain](https://rustup.rs/)
- Xcode Command Line Tools (for Swift)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
xcode-select --install
```

## Build

```bash
bash scripts/bundle-app.sh
```

This builds both the Rust daemon and Swift app, then assembles them into a macOS `.app` bundle:
- `target/release/bundle/FortiVPN Tray.app` — the macOS app bundle
- `target/release/fortivpn` — the CLI companion

### Install

```bash
# Copy app to Applications
cp -r "target/release/bundle/FortiVPN Tray.app" /Applications/

# Install privileged helper daemon
sudo bash -c '
cp target/release/fortivpn-helper /Library/PrivilegedHelperTools/fortivpn-helper &&
chmod 755 /Library/PrivilegedHelperTools/fortivpn-helper &&
chown root:wheel /Library/PrivilegedHelperTools/fortivpn-helper &&
cp resources/com.fortivpn-tray.helper.plist /Library/LaunchDaemons/ &&
chown root:wheel /Library/LaunchDaemons/com.fortivpn-tray.helper.plist &&
launchctl bootout system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist 2>/dev/null;
launchctl bootstrap system /Library/LaunchDaemons/com.fortivpn-tray.helper.plist
'
```

## Usage

### System Tray

1. Launch **FortiVPN Tray** from Applications
2. Click the shield icon in the menu bar
3. Open **Settings** to add a VPN profile (host, port, username, certificate fingerprint)
4. Click a profile to connect — enter your VPN password when prompted
5. Click again to disconnect

### CLI

The CLI controls the VPN through the daemon via a Unix socket.

```bash
fortivpn status              # Show connection status
fortivpn list                # List profiles
fortivpn connect <name>      # Connect to a profile
fortivpn disconnect          # Disconnect
```

Short aliases: `s` = status, `l` = list, `c` = connect, `d` = disconnect

Profile matching is case-insensitive and partial — `sg` matches "My SG VPN".

> The tray app must be running for the CLI to work.

## Data Storage

| Data | Location |
|------|----------|
| Profiles | `~/Library/Application Support/fortivpn-tray/profiles.json` |
| Passwords | macOS Keychain (service: `fortivpn-tray`) |
| IPC Socket | `~/Library/Application Support/fortivpn-tray/ipc.sock` |

## How FortiGate SSL-VPN Works

### What is FortiGate SSL-VPN?

FortiGate is a network security appliance made by Fortinet. Organizations deploy it at the edge of their corporate network as a firewall and VPN gateway. The **SSL-VPN** feature allows remote employees to securely access the internal corporate network over the internet using TLS (the same encryption that protects HTTPS websites).

Unlike IPsec VPNs which operate at the network layer and require special firewall rules, SSL-VPN runs over standard HTTPS (port 443 by default), making it work through almost any firewall or NAT — including hotel Wi-Fi, airport networks, and restrictive corporate proxies.

### The Big Picture

```mermaid
graph LR
    subgraph laptop["Your Laptop"]
        app["Apps<br/>(browser, ssh, etc)"]
        tun["TUN Device<br/>(virtual interface)"]
        app --> tun
    end

    subgraph corp["Corporate Network"]
        gw["FortiGate<br/>Gateway<br/>(10.0.0.1)"]
        srv["Internal<br/>Servers<br/>(10.x.x.x)"]
        gw --> srv
    end

    tun -- "TLS Tunnel<br/>(encrypted over internet)" --> gw

    style laptop fill:#0ea5e9,stroke:#0284c7,color:#fff
    style corp fill:#7c3aed,stroke:#6d28d9,color:#fff
```

When connected, your laptop gets a **virtual IP address** on the corporate network (e.g., `10.212.134.5`). All traffic destined for the corporate network is routed through a **TUN device** (a virtual network interface), encrypted via TLS, and sent to the FortiGate gateway which decrypts it and forwards it to the internal servers. Responses travel the same path back.

### How the Client Connects (Step by Step)

FortiVPN Tray implements the full FortiGate SSL-VPN client protocol in Rust. Here's what happens when you click "Connect":

#### Phase 1: TLS Authentication

```mermaid
sequenceDiagram
    participant C as Client
    participant G as FortiGate Gateway

    C->>G: TLS Handshake (port 443)
    Note over C: Verify server certificate
    G-->>C: TLS Established

    C->>G: POST /remote/logincheck
    Note over C,G: username=alice&credential=s3cret

    G-->>C: Set-Cookie: SVPNCOOKIE=abc123...
    Note over G: + XML config (routes, DNS, assigned IP)
```

The client connects to the gateway over TLS, then authenticates via an HTTP POST request with username and password. On success, the gateway returns an `SVPNCOOKIE` (a session token) and an XML configuration containing:
- **Assigned IP address** — your virtual IP on the corporate network
- **Routes** — which IP ranges should go through the VPN (split tunnel) or all traffic (full tunnel)
- **DNS servers** — corporate DNS servers for resolving internal hostnames

#### Phase 2: TUN Device Creation

A **TUN device** (`utun`) is a virtual network interface that operates at the IP layer. When you send traffic to `10.x.x.x`, the OS routing table directs it to the TUN device instead of your physical Wi-Fi adapter. The VPN client reads these packets from the TUN device and sends them through the encrypted tunnel.

Creating a TUN device requires **root privileges**. FortiVPN Tray uses a separate privileged helper process (managed by macOS `launchd`) that creates the TUN device and passes the file descriptor back to the unprivileged main app using `SCM_RIGHTS` — a Unix mechanism for sending open file descriptors between processes.

```mermaid
sequenceDiagram
    participant App as FortiVPN Tray<br/>(unprivileged)
    participant H as Helper Daemon<br/>(root, launchd)
    participant K as macOS Kernel

    App->>H: {"cmd": "create_tun", "ip": "10.212.134.5"}
    H->>K: Create utun device
    K-->>H: utun5 (fd=7)
    H-->>App: Send fd via SCM_RIGHTS
    Note over App: Now owns TUN device<br/>without ever being root
    H-->>App: {"ok": true, "tun_name": "utun5"}
```

#### Phase 3: PPP Negotiation

```mermaid
sequenceDiagram
    participant C as Client
    participant G as FortiGate Gateway

    C->>G: GET /remote/sslvpn-tunnel
    Note over C,G: Cookie: SVPNCOOKIE=abc123...
    G-->>C: HTTP/1.1 200 (keep connection open)

    rect rgb(230, 240, 255)
        Note over C,G: LCP (Link Control Protocol)
        C->>G: LCP Configure-Request (MRU=1354, Magic=0xabcd)
        G-->>C: LCP Configure-Ack
    end

    rect rgb(230, 255, 240)
        Note over C,G: IPCP (IP Control Protocol)
        C->>G: IPCP Configure-Request (request IP, DNS)
        G-->>C: IPCP Configure-Nak (your IP: 10.212.134.5)
        C->>G: IPCP Configure-Request (accept 10.212.134.5)
        G-->>C: IPCP Configure-Ack
    end
```

After authentication, a second TLS connection opens the actual VPN tunnel via HTTP. Inside this connection, FortiGate uses **PPP (Point-to-Point Protocol)** to negotiate:
- **LCP** (Link Control Protocol) — Agrees on maximum packet size and connection parameters
- **IPCP** (IP Control Protocol) — Assigns the client its virtual IP address and DNS servers

PPP frames are wrapped inside FortiGate's proprietary framing format (a 6-byte header with a magic number `0x5050` and payload length).

#### Phase 4: IP Bridge (Data Transfer)

Once PPP negotiation completes, the client runs an **async IP bridge** — two concurrent loops:

```mermaid
graph LR
    subgraph outbound["Outbound (app to corporate)"]
        direction LR
        A1["App Traffic"] --> T1["TUN Device"]
        T1 --> P1["PPP Frame"]
        P1 --> E1["TLS Encrypt"]
        E1 --> G1["FortiGate"]
    end

    subgraph inbound["Inbound (corporate to app)"]
        direction RL
        G2["FortiGate"] --> D2["TLS Decrypt"]
        D2 --> P2["PPP Frame"]
        P2 --> T2["TUN Device"]
        T2 --> A2["App Traffic"]
    end

    style outbound fill:#dbeafe,stroke:#2563eb
    style inbound fill:#dcfce7,stroke:#16a34a
```

- **TUN to Tunnel**: Read raw IP packets from the TUN device, wrap them in PPP frames with FortiGate's header, encrypt via TLS, and send to the gateway.
- **Tunnel to TUN**: Read encrypted PPP frames from the TLS connection, unwrap the IP packets, and write them to the TUN device.

The bridge also handles **LCP Echo** keep-alive messages — the gateway sends periodic echo requests, and the client must reply to prove the connection is still alive. If 3 consecutive echoes go unanswered, the gateway drops the session.

#### Phase 5: Routing

With the tunnel running, the client configures the OS routing table so that traffic to corporate networks goes through the TUN device:

```mermaid
graph TD
    subgraph split["Split Tunnel (specific routes)"]
        direction LR
        S1["10.0.0.0/8"] --> TUN1["TUN to VPN"]
        S2["172.16.0.0/12"] --> TUN1
        S3["All other traffic"] --> WAN1["Wi-Fi to Internet"]
    end

    subgraph full["Full Tunnel (all traffic)"]
        direction LR
        F1["0.0.0.0/1"] --> TUN2["TUN to VPN"]
        F2["128.0.0.0/1"] --> TUN2
        F3["Gateway host route"] --> WAN2["Wi-Fi to Internet"]
    end

    style split fill:#dbeafe,stroke:#2563eb
    style full fill:#fef3c7,stroke:#d97706
```

**Split tunnel** routes only corporate IP ranges through the VPN. All other traffic (web browsing, streaming) goes directly through your normal internet connection.

**Full tunnel** routes all IPv4 traffic through the VPN using two broad routes (`0.0.0.0/1` + `128.0.0.0/1`) that cover the entire address space without replacing the actual default route.

A **host route** to the gateway's public IP is always added via the original default gateway, so the encrypted tunnel traffic itself doesn't get routed back into the VPN (which would create a loop).

DNS is configured via macOS `scutil` to use the corporate DNS servers for resolving internal hostnames like `jira.corp.com` or `git.internal`.

#### Phase 6: Disconnect

```mermaid
sequenceDiagram
    participant App as Client
    participant H as Helper (root)
    participant G as FortiGate Gateway

    App->>App: Signal bridge tasks to stop
    App->>H: Delete routes (via helper)
    App->>H: Restore DNS
    App->>H: Close TUN device
    App->>App: Re-enable IPv6
    App->>G: GET /remote/logout (session cleanup)
    G-->>App: 200 OK
    App->>App: Close TLS connection

    Note over H: Helper stays alive<br/>for next connection
```

The helper daemon stays alive for the next connection — no admin password prompt needed.

### Security Model

| Layer | Protection |
|-------|-----------|
| **Transport** | TLS 1.2/1.3 encrypts all tunnel traffic |
| **Authentication** | Username + password over TLS (no plaintext) |
| **Certificate pinning** | Optional SHA256 fingerprint verification prevents MITM |
| **Credential storage** | Passwords in macOS Keychain (hardware-backed on Apple Silicon) |
| **Privilege separation** | Main app is unprivileged; only the helper runs as root |
| **IPv6 leak prevention** | IPv6 disabled during VPN to prevent traffic bypassing the tunnel |

## License

MIT
