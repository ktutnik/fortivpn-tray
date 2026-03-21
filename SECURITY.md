# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in FortiVPN Tray, please report it responsibly.

**Do NOT open a public GitHub issue for security vulnerabilities.**

Instead, please email: **iketut.sandiarsa@mims.com**

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

## Response Timeline

- **Acknowledgment**: Within 48 hours
- **Initial assessment**: Within 1 week
- **Fix release**: As soon as possible, depending on severity

## Supported Versions

| Version | Supported |
|---------|-----------|
| Latest (`main`) | Yes |
| Older releases | No |

## Scope

The following are in scope:
- VPN protocol implementation (`crates/fortivpn/`)
- Credential handling (keychain, password storage)
- IPC communication (Unix socket protocol)
- Privileged helper daemon (`crates/fortivpn-helper/`)
- Route/DNS manipulation

The following are out of scope:
- FortiGate server-side vulnerabilities
- macOS operating system vulnerabilities
- Denial of service via network flooding
