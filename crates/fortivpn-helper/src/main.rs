//! Privileged helper for fortivpn-tray.
//!
//! This binary runs as root (launched via `osascript` with administrator privileges).
//! It creates a utun device and passes the file descriptor back to the unprivileged
//! parent process via a Unix socket using SCM_RIGHTS, then executes route/DNS commands.
//!
//! Protocol (over Unix socket, newline-delimited JSON):
//!   Parent -> Helper: {"cmd":"create_tun","ip":"10.0.0.1","peer_ip":"10.0.0.2","mtu":1354}
//!   Helper -> Parent: {"ok":true,"tun_name":"utun5"}  (+ sends fd via SCM_RIGHTS)
//!   Parent -> Helper: {"cmd":"add_route","dest":"10.0.0.0/24","gateway":"10.0.0.1"}
//!   Helper -> Parent: {"ok":true}
//!   Parent -> Helper: {"cmd":"configure_dns","servers":["8.8.8.8"],"search_domain":"corp.com"}
//!   Helper -> Parent: {"ok":true}
//!   Parent -> Helper: {"cmd":"cleanup","routes":[...],"gateway_ip":"1.2.3.4","orig_gateway":"192.168.1.1"}
//!   (Helper cleans up routes/DNS and exits)

#[cfg(unix)]
mod commands;

#[cfg(unix)]
mod unix_main;

#[cfg(unix)]
fn main() {
    unix_main::run();
}

#[cfg(windows)]
fn main() {
    eprintln!("fortivpn-helper is not supported on Windows");
    std::process::exit(1);
}
