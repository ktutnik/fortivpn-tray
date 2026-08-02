use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{split, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio_rustls::client::TlsStream;

use crate::ppp::*;
use crate::tunnel::{encode_frame_into, read_frame, write_frame, FrameReader};
use crate::FortiError;

/// How often the client sends an LCP echo request.
const ECHO_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Echo intervals allowed to pass with no inbound frame at all before the link
/// is declared dead. Any inbound frame resets the counter, so this only ever
/// expires on a genuinely silent tunnel.
const MAX_MISSED_ECHOES: u8 = 6;

/// Packets coalesced into a single TLS write. Bounded so a flood cannot grow the
/// scratch buffer without limit or stall the shutdown branch.
const WRITE_BATCH_LIMIT: usize = 32;

/// How often the tunnel reports its vitals while healthy.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Live counters for one tunnel.
///
/// A drop is diagnosed from what the tunnel was doing in the minute before it
/// died, not from the death itself — "silent for 60s then killed" and "died
/// mid-transfer at 2 MB/s" have completely different causes and used to produce
/// the identical message.
#[derive(Debug)]
pub struct BridgeStats {
    start: std::time::Instant,
    pub frames_in: AtomicU64,
    pub frames_out: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub echoes_sent: AtomicU64,
    pub echo_replies: AtomicU64,
    /// Milliseconds since `start` when the last inbound frame arrived.
    pub last_inbound_ms: AtomicU64,
}

impl Default for BridgeStats {
    fn default() -> Self {
        Self {
            start: std::time::Instant::now(),
            frames_in: AtomicU64::new(0),
            frames_out: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            echoes_sent: AtomicU64::new(0),
            echo_replies: AtomicU64::new(0),
            last_inbound_ms: AtomicU64::new(0),
        }
    }
}

impl BridgeStats {
    pub fn uptime(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    fn mark_inbound(&self, bytes: usize) {
        self.frames_in.fetch_add(1, Ordering::Relaxed);
        self.bytes_in.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_inbound_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// How long since anything at all arrived from the gateway.
    pub fn since_last_inbound(&self) -> std::time::Duration {
        let last = self.last_inbound_ms.load(Ordering::Relaxed);
        self.start
            .elapsed()
            .saturating_sub(std::time::Duration::from_millis(last))
    }

    /// One-line vitals for the log.
    pub fn summary(&self) -> String {
        format!(
            "up {:.0}s | in {} frames/{} KB | out {} frames/{} KB | echo {} sent/{} replied | last inbound {:.1}s ago",
            self.uptime().as_secs_f64(),
            self.frames_in.load(Ordering::Relaxed),
            self.bytes_in.load(Ordering::Relaxed) / 1024,
            self.frames_out.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed) / 1024,
            self.echoes_sent.load(Ordering::Relaxed),
            self.echo_replies.load(Ordering::Relaxed),
            self.since_last_inbound().as_secs_f64(),
        )
    }
}

/// Record a tunnel death: log the cause together with the vitals that led to it,
/// then notify the session monitor.
fn declare_dead(
    reason: String,
    stats: &BridgeStats,
    alive: &AtomicBool,
    event_tx: &tokio::sync::watch::Sender<crate::VpnEvent>,
) {
    alive.store(false, Ordering::Relaxed);
    log::warn!(target: "vpn", "Tunnel died: {reason} | {}", stats.summary());
    let _ = event_tx.send(crate::VpnEvent::Died(reason));
}

/// macOS utun packet information header for IPv4 (AF_INET = 2).
#[cfg(target_os = "macos")]
const PI_HEADER_IPV4: [u8; 4] = [0, 0, 0, 2];
#[cfg(target_os = "macos")]
const PI_HEADER_SIZE: usize = 4;

#[cfg(not(target_os = "macos"))]
const PI_HEADER_SIZE: usize = 0;

/// Establish an async TLS connection to the FortiGate gateway.
pub async fn async_tls_connect(
    host: &str,
    port: u16,
    trusted_cert: &str,
) -> Result<TlsStream<TcpStream>, FortiError> {
    let addr = format!("{host}:{port}");
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| FortiError::GatewayUnreachable(format!("{e}")))?;
    tcp.set_nodelay(true).ok();

    let config = crate::auth::build_tls_config(trusted_cert);
    let connector = tokio_rustls::TlsConnector::from(config);

    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| FortiError::GatewayUnreachable(format!("Invalid hostname: {e}")))?;

    let tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| FortiError::CertificateNotTrusted(format!("{e}")))?;

    Ok(tls)
}

/// Send the tunnel request and switch to binary mode.
/// Returns the TLS stream ready for 0x5050 framing.
pub async fn open_tunnel(
    stream: &mut TlsStream<TcpStream>,
    host: &str,
    port: u16,
    cookie: &str,
) -> Result<(), FortiError> {
    let req = format!(
        "GET /remote/sslvpn-tunnel HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Cookie: SVPNCOOKIE={cookie}\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| FortiError::TunnelRejected(format!("Write tunnel request: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| FortiError::TunnelRejected(format!("Flush: {e}")))?;

    // Some gateways respond with HTTP headers before switching to binary mode,
    // others switch to binary immediately (no response until client sends PPP data).
    // Use a short timeout to detect which behavior we get.
    let mut buf = [0u8; 1];
    let mut header_buf = Vec::with_capacity(512);

    match tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        stream.read_exact(&mut buf),
    )
    .await
    {
        Ok(Ok(_)) => {
            // Got data — read the rest of the response (HTTP headers or binary)
            header_buf.push(buf[0]);

            // Keep reading until we identify the response type
            loop {
                match tokio::time::timeout(
                    tokio::time::Duration::from_millis(500),
                    stream.read_exact(&mut buf),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        header_buf.push(buf[0]);

                        // If HTTP response, read until end of headers
                        if header_buf.len() == 5 && &header_buf[..5] == b"HTTP/" {
                            while !header_buf.ends_with(b"\r\n\r\n") {
                                stream.read_exact(&mut buf).await.map_err(|e| {
                                    FortiError::TunnelRejected(format!("Read HTTP: {e}"))
                                })?;
                                header_buf.push(buf[0]);
                                if header_buf.len() > 4096 {
                                    return Err(FortiError::TunnelRejected(
                                        "HTTP response too large".to_string(),
                                    ));
                                }
                            }
                            // Check for HTTP error
                            let resp = String::from_utf8_lossy(&header_buf);
                            if resp.contains("403") || resp.contains("401") {
                                return Err(FortiError::TunnelRejected(format!(
                                    "Gateway rejected tunnel: {}",
                                    resp.lines().next().unwrap_or("")
                                )));
                            }
                            break;
                        }

                        // Non-HTTP binary data — already in tunnel mode
                        if header_buf.len() >= 6 {
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        return Err(FortiError::TunnelRejected(format!("Read: {e}")));
                    }
                    Err(_) => {
                        // Timeout — gateway switched to binary mode, no more initial data
                        break;
                    }
                }
            }
        }
        Ok(Err(e)) => {
            return Err(FortiError::TunnelRejected(format!(
                "Read tunnel response: {e}"
            )));
        }
        Err(_) => {
            // Timeout — gateway entered binary mode without sending anything.
            // This is normal: the server waits for the client to start PPP negotiation.
        }
    }

    Ok(())
}

/// What PPP negotiation settled on.
#[derive(Debug, Clone, PartialEq)]
pub struct PppSession {
    pub assigned_ip: Ipv4Addr,
    pub magic_number: u32,
    pub dns_servers: Vec<Ipv4Addr>,
    /// MTU the gateway agreed to, for the TUN device.
    pub mtu: u16,
}

/// Run PPP negotiation (LCP + IPCP) over the tunnel.
/// Returns the negotiated IP, DNS servers and MTU.
pub async fn negotiate_ppp<R, W>(reader: &mut R, writer: &mut W) -> Result<PppSession, FortiError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut lcp = LcpState::new();
    let mut ipcp = IpcpState::new();
    let mut id_counter: u8 = 1;

    // Send our LCP Configure-Request
    let our_lcp_req = lcp.build_configure_request(id_counter);
    write_frame(writer, &our_lcp_req.encode())
        .await
        .map_err(|e| FortiError::PppNegotiationFailed(format!("Send LCP req: {e}")))?;
    id_counter = id_counter.wrapping_add(1);

    let mut lcp_our_acked = false;
    let mut lcp_peer_acked = false;
    let mut ipcp_started = false;
    let mut ipcp_our_acked = false;
    let mut ipcp_peer_acked = false;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

    loop {
        if lcp_our_acked && lcp_peer_acked && ipcp_our_acked && ipcp_peer_acked {
            break;
        }

        let frame = tokio::time::timeout_at(deadline, read_frame(reader))
            .await
            .map_err(|_| FortiError::PppNegotiationFailed("Negotiation timeout".to_string()))?
            .map_err(|e| FortiError::PppNegotiationFailed(format!("Read frame: {e}")))?;

        if frame.len() < 6 {
            continue; // Skip too-short frames (keepalive empty frames)
        }

        let pkt = match PppPacket::decode(&frame) {
            Ok(p) => p,
            Err(_) => continue,
        };

        match pkt.protocol {
            LCP_PROTOCOL => match pkt.code {
                LCP_CONFIGURE_REQUEST => {
                    let ack = lcp.handle_configure_request(pkt.identifier, &pkt.data);
                    write_frame(writer, &ack.encode()).await.map_err(|e| {
                        FortiError::PppNegotiationFailed(format!("Send LCP ack: {e}"))
                    })?;
                    lcp_peer_acked = true;
                }
                LCP_CONFIGURE_ACK => {
                    lcp.handle_configure_ack();
                    lcp_our_acked = true;
                }
                LCP_CONFIGURE_NAK => {
                    lcp.handle_configure_nak(&pkt.data);
                    // Resend with updated values
                    let req = lcp.build_configure_request(id_counter);
                    write_frame(writer, &req.encode()).await.map_err(|e| {
                        FortiError::PppNegotiationFailed(format!("Resend LCP: {e}"))
                    })?;
                    id_counter = id_counter.wrapping_add(1);
                }
                LCP_CONFIGURE_REJECT => {
                    // Accept rejection — remove rejected options and resend
                    let req = lcp.build_configure_request(id_counter);
                    write_frame(writer, &req.encode()).await.map_err(|e| {
                        FortiError::PppNegotiationFailed(format!("Resend LCP: {e}"))
                    })?;
                    id_counter = id_counter.wrapping_add(1);
                }
                LCP_ECHO_REQUEST => {
                    let reply = lcp.build_echo_reply(pkt.identifier);
                    write_frame(writer, &reply.encode()).await.map_err(|e| {
                        FortiError::PppNegotiationFailed(format!("Send echo reply: {e}"))
                    })?;
                }
                _ => {}
            },
            IPCP_PROTOCOL => match pkt.code {
                LCP_CONFIGURE_REQUEST => {
                    let ack = ipcp.handle_configure_request(pkt.identifier, &pkt.data);
                    write_frame(writer, &ack.encode()).await.map_err(|e| {
                        FortiError::PppNegotiationFailed(format!("Send IPCP ack: {e}"))
                    })?;
                    ipcp_peer_acked = true;
                }
                LCP_CONFIGURE_ACK => {
                    ipcp.handle_configure_ack();
                    ipcp_our_acked = true;
                }
                LCP_CONFIGURE_NAK => {
                    ipcp.handle_configure_nak(&pkt.data);
                    // Resend with assigned values
                    let req = ipcp.build_configure_request(id_counter);
                    write_frame(writer, &req.encode()).await.map_err(|e| {
                        FortiError::PppNegotiationFailed(format!("Resend IPCP: {e}"))
                    })?;
                    id_counter = id_counter.wrapping_add(1);
                }
                _ => {}
            },
            _ => {}
        }

        // Start IPCP once LCP is opened
        if lcp_our_acked && lcp_peer_acked && !ipcp_started {
            ipcp_started = true;
            let req = ipcp.build_configure_request(id_counter);
            write_frame(writer, &req.encode())
                .await
                .map_err(|e| FortiError::PppNegotiationFailed(format!("Send IPCP req: {e}")))?;
            id_counter = id_counter.wrapping_add(1);
        }
    }

    let mut dns_servers = Vec::new();
    if !ipcp.primary_dns.is_unspecified() {
        dns_servers.push(ipcp.primary_dns);
    }
    if !ipcp.secondary_dns.is_unspecified() {
        dns_servers.push(ipcp.secondary_dns);
    }

    Ok(PppSession {
        assigned_ip: ipcp.local_ip,
        magic_number: lcp.magic_number,
        dns_servers,
        mtu: lcp.negotiated_mtu(),
    })
}

/// The result of starting the bridge — contains handles for cleanup.
pub struct BridgeHandle {
    pub tasks: Vec<JoinHandle<()>>,
    pub alive: Arc<AtomicBool>,
    pub event_rx: tokio::sync::watch::Receiver<crate::VpnEvent>,
    pub stats: Arc<BridgeStats>,
}

/// Start the bidirectional bridge between TLS tunnel and tun device.
/// Returns handles for the spawned tasks and a health flag.
pub fn start_bridge<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static>(
    tls_stream: TlsStream<TcpStream>,
    tun_device: T,
    shutdown: Arc<Notify>,
    magic_number: u32,
) -> BridgeHandle {
    let alive = Arc::new(AtomicBool::new(true));
    let stats = Arc::new(BridgeStats::default());
    let (event_tx, event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
    let event_tx = Arc::new(event_tx);
    let (tls_reader, tls_writer) = split(tls_stream);
    let (tun_reader, tun_writer) = split(tun_device);
    let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(256);

    // Task 1: Tunnel writer — drains channel and writes to TLS
    let tunnel_writer_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        let event_tx = event_tx.clone();
        let stats = stats.clone();
        tokio::spawn(async move {
            tunnel_writer_loop(tls_writer, outbound_rx, shutdown, alive, event_tx, stats).await;
        })
    };

    // Task 2: Tunnel reader — reads from TLS, handles PPP control, writes IP to tun
    let tunnel_reader_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        let outbound_tx = outbound_tx.clone();
        let event_tx = event_tx.clone();
        let stats = stats.clone();
        tokio::spawn(async move {
            tunnel_reader_loop(
                tls_reader,
                tun_writer,
                outbound_tx,
                shutdown,
                alive,
                magic_number,
                event_tx,
                stats,
            )
            .await;
        })
    };

    // Task 3: Tun reader — reads from tun, wraps in PPP, sends to channel
    let tun_reader_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        tokio::spawn(async move {
            tun_reader_loop(tun_reader, outbound_tx, shutdown, alive).await;
        })
    };

    // Task 4: Heartbeat — periodic vitals so the run-up to a drop is on record,
    // not just the drop itself.
    let heartbeat_task = {
        let shutdown = shutdown.clone();
        let alive = alive.clone();
        let stats = stats.clone();
        tokio::spawn(async move {
            heartbeat_loop(shutdown, alive, stats).await;
        })
    };

    BridgeHandle {
        tasks: vec![
            tunnel_writer_task,
            tunnel_reader_task,
            tun_reader_task,
            heartbeat_task,
        ],
        alive,
        event_rx,
        stats,
    }
}

async fn heartbeat_loop(shutdown: Arc<Notify>, alive: Arc<AtomicBool>, stats: Arc<BridgeStats>) {
    let mut timer = echo_timer(HEARTBEAT_INTERVAL);
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = timer.tick() => {
                if !alive.load(Ordering::Relaxed) {
                    break;
                }
                log::info!(target: "vpn", "Tunnel healthy | {}", stats.summary());
            }
        }
    }
}

async fn tunnel_writer_loop<W: AsyncWriteExt + Unpin>(
    mut writer: W,
    mut rx: mpsc::Receiver<Vec<u8>>,
    shutdown: Arc<Notify>,
    alive: Arc<AtomicBool>,
    event_tx: Arc<tokio::sync::watch::Sender<crate::VpnEvent>>,
    stats: Arc<BridgeStats>,
) {
    // Drain whatever has queued up into one buffer, then one write and one
    // flush. Writing per packet meant a TLS record and a syscall each — with
    // TCP_NODELAY set, zero coalescing on a burst.
    let mut batch: Vec<Vec<u8>> = Vec::with_capacity(WRITE_BATCH_LIMIT);
    let mut out: Vec<u8> = Vec::with_capacity(WRITE_BATCH_LIMIT * 1500);

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            count = rx.recv_many(&mut batch, WRITE_BATCH_LIMIT) => {
                if count == 0 {
                    break; // channel closed
                }

                out.clear();
                let mut frames = 0u64;
                for payload in batch.drain(..) {
                    encode_frame_into(&mut out, &payload);
                    frames += 1;
                }

                // Keep the real cause: "broken pipe" and "connection reset by peer"
                // point at different failures and used to collapse to one string.
                if let Err(e) = writer.write_all(&out).await.and(writer.flush().await) {
                    declare_dead(
                        format!("TLS write error: {} ({:?})", e, e.kind()),
                        &stats,
                        &alive,
                        &event_tx,
                    );
                    break;
                }

                stats.frames_out.fetch_add(frames, Ordering::Relaxed);
                stats.bytes_out.fetch_add(out.len() as u64, Ordering::Relaxed);
            }
        }
    }
}

/// Build the LCP echo timer.
///
/// `tokio::time::interval` fires its first tick immediately, which counted as a
/// missed echo the instant the bridge started — so a quiet tunnel on a gateway
/// that ignores client echo requests was killed at t=20s. Start one full period
/// out, and never bunch up ticks to "catch up" after the loop was busy.
fn echo_timer(period: std::time::Duration) -> tokio::time::Interval {
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    timer
}

#[allow(clippy::too_many_arguments)]
async fn tunnel_reader_loop<R: AsyncReadExt + Unpin, W: AsyncWriteExt + Unpin>(
    reader: R,
    mut tun_writer: W,
    outbound_tx: mpsc::Sender<Vec<u8>>,
    shutdown: Arc<Notify>,
    alive: Arc<AtomicBool>,
    magic_number: u32,
    event_tx: Arc<tokio::sync::watch::Sender<crate::VpnEvent>>,
    stats: Arc<BridgeStats>,
) {
    let mut echo_interval = echo_timer(ECHO_INTERVAL);
    let mut echo_id: u8 = 0;
    let mut missed_echoes: u8 = 0;

    // Cancel-safe: the echo timer and shutdown branches below can drop the read
    // future mid-frame, and FrameReader resumes instead of losing the bytes.
    let mut frames = FrameReader::new(reader);

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = echo_interval.tick() => {
                // Send LCP echo request
                echo_id = echo_id.wrapping_add(1);
                let echo = PppPacket {
                    protocol: LCP_PROTOCOL,
                    code: LCP_ECHO_REQUEST,
                    identifier: echo_id,
                    data: magic_number.to_be_bytes().to_vec(),
                };
                if outbound_tx.send(echo.encode()).await.is_err() {
                    break;
                }
                stats.echoes_sent.fetch_add(1, Ordering::Relaxed);
                missed_echoes = missed_echoes.saturating_add(1);
                if missed_echoes >= MAX_MISSED_ECHOES {
                    declare_dead(
                        format!(
                            "LCP echo timeout — nothing received for {:.0}s over {missed_echoes} echo intervals",
                            stats.since_last_inbound().as_secs_f64()
                        ),
                        &stats,
                        &alive,
                        &event_tx,
                    );
                    break;
                }
            }
            frame = frames.next_frame() => {
                let frame = match frame {
                    Ok(f) => f,
                    Err(e) => {
                        // FrameReader distinguishes a clean close ("tunnel closed")
                        // from a reset, a timeout and a framing desync. Collapsing
                        // them into one string is what made drops untraceable.
                        declare_dead(
                            format!("Tunnel read failed: {e}"),
                            &stats,
                            &alive,
                            &event_tx,
                        );
                        break;
                    }
                };

                stats.mark_inbound(frame.len());

                // Any inbound frame proves the link is alive — data packets, the
                // server's own LCP echo-requests, and empty keepalive frames all
                // count. Many FortiGate gateways never reply to client-initiated
                // echo-requests, so relying solely on Echo-Reply falsely kills a
                // healthy, actively-used tunnel (~30s after connect → reconnect loop).
                missed_echoes = 0;

                if frame.len() < 4 {
                    continue; // skip empty/too-short frames
                }

                let protocol = u16::from_be_bytes([frame[0], frame[1]]);

                match protocol {
                    IP_PROTOCOL => {
                        // Extract IP packet (skip 2-byte PPP protocol field)
                        let ip_data = &frame[2..];
                        #[cfg(target_os = "macos")]
                        {
                            let mut pkt = Vec::with_capacity(PI_HEADER_SIZE + ip_data.len());
                            pkt.extend_from_slice(&PI_HEADER_IPV4);
                            pkt.extend_from_slice(ip_data);
                            let _ = tun_writer.write_all(&pkt).await;
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            let _ = tun_writer.write_all(ip_data).await;
                        }
                    }
                    LCP_PROTOCOL if frame.len() >= 6 => {
                        let code = frame[2];
                        let id = frame[3];
                        match code {
                            LCP_ECHO_REQUEST => {
                                let reply = PppPacket {
                                    protocol: LCP_PROTOCOL,
                                    code: LCP_ECHO_REPLY,
                                    identifier: id,
                                    data: magic_number.to_be_bytes().to_vec(),
                                };
                                let _ = outbound_tx.send(reply.encode()).await;
                            }
                            LCP_ECHO_REPLY => {
                                stats.echo_replies.fetch_add(1, Ordering::Relaxed);
                                missed_echoes = 0;
                            }
                            LCP_TERMINATE_REQUEST => {
                                let ack = PppPacket {
                                    protocol: LCP_PROTOCOL,
                                    code: LCP_TERMINATE_ACK,
                                    identifier: id,
                                    data: vec![],
                                };
                                let _ = outbound_tx.send(ack.encode()).await;
                                declare_dead(
                                    "Server sent LCP Terminate-Request".to_string(),
                                    &stats,
                                    &alive,
                                    &event_tx,
                                );
                                break;
                            }
                            _ => {}
                        }
                    }
                    _ => {} // ignore other protocols
                }
            }
        }
    }
}

async fn tun_reader_loop<R: AsyncReadExt + Unpin>(
    mut tun_reader: R,
    outbound_tx: mpsc::Sender<Vec<u8>>,
    shutdown: Arc<Notify>,
    alive: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; 2000]; // MTU 1354 + PI header + margin

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            result = tun_reader.read(&mut buf) => {
                match result {
                    Ok(n) if n > PI_HEADER_SIZE => {
                        // Strip PI header on macOS, get raw IP packet
                        let ip_data = &buf[PI_HEADER_SIZE..n];

                        // Wrap in PPP frame: [protocol u16][IP data]
                        let mut ppp_frame = Vec::with_capacity(2 + ip_data.len());
                        ppp_frame.extend_from_slice(&IP_PROTOCOL.to_be_bytes());
                        ppp_frame.extend_from_slice(ip_data);

                        if outbound_tx.send(ppp_frame).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => continue, // too short
                    Err(_) => {
                        if !alive.load(Ordering::Relaxed) {
                            break;
                        }
                        continue;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::{read_frame, write_frame};

    use std::sync::atomic::Ordering;
    use tokio::sync::{mpsc, Notify};

    #[tokio::test]
    async fn test_echo_timer_first_tick_is_not_immediate() {
        // tokio::time::interval fires immediately, which counted as a missed echo
        // at t=0 and killed an idle tunnel three ticks later.
        let period = std::time::Duration::from_millis(80);
        let mut timer = echo_timer(period);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), timer.tick())
                .await
                .is_err(),
            "first echo tick fired immediately"
        );
        let started = std::time::Instant::now();
        timer.tick().await;
        assert!(
            started.elapsed() < period,
            "first tick came a period too late"
        );
    }

    #[tokio::test]
    async fn test_echo_timer_ticks_on_period() {
        let period = std::time::Duration::from_millis(40);
        let mut timer = echo_timer(period);
        timer.tick().await;
        let started = std::time::Instant::now();
        timer.tick().await;
        // Half a period, not a full one: timer granularity makes an exact
        // comparison flaky, and the point here is only that it waited.
        assert!(started.elapsed() >= period / 2);
    }

    #[test]
    fn test_missed_echo_budget_outlasts_a_slow_gateway() {
        // Any inbound frame resets the counter, so this budget only expires on a
        // tunnel that is completely silent. Keep it well clear of the old ~20s.
        let silence = ECHO_INTERVAL * u32::from(MAX_MISSED_ECHOES);
        assert!(silence >= std::time::Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_tunnel_writer_loop_sends_frames() {
        let (client, server) = tokio::io::duplex(65536);
        let (_, client_writer) = tokio::io::split(client);
        let (mut server_reader, _) = tokio::io::split(server);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_writer_loop(
                client_writer,
                rx,
                shutdown_clone,
                alive_clone,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send some data through the channel
        tx.send(b"hello".to_vec()).await.unwrap();
        tx.send(b"world".to_vec()).await.unwrap();

        // Read frames from the other end
        let frame1 = read_frame(&mut server_reader).await.unwrap();
        assert_eq!(frame1, b"hello");
        let frame2 = read_frame(&mut server_reader).await.unwrap();
        assert_eq!(frame2, b"world");

        // Shutdown
        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    /// Wraps a writer and counts how many `poll_write` calls reach it.
    struct CountingWriter<W> {
        inner: W,
        writes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for CountingWriter<W> {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn test_tunnel_writer_loop_coalesces_a_burst_into_one_write() {
        // The point of batching: a queued burst must reach the socket as one
        // write, not one per packet. Previously this was 20 writes and 20 flushes.
        let (client, server) = tokio::io::duplex(65536);
        let (_, client_writer) = tokio::io::split(client);
        let (mut server_reader, _) = tokio::io::split(server);

        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counting = CountingWriter {
            inner: client_writer,
            writes: writes.clone(),
        };

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        // Queue everything before the writer task exists, so recv_many sees a
        // full backlog rather than one message at a time.
        let payloads: Vec<Vec<u8>> = (0..20u8).map(|i| vec![i; 100 + i as usize]).collect();
        for p in &payloads {
            tx.send(p.clone()).await.unwrap();
        }

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_writer_loop(
                counting,
                rx,
                shutdown_clone,
                alive_clone,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        for expected in &payloads {
            let frame = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(&frame, expected);
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;

        let n = writes.load(Ordering::Relaxed);
        assert!(
            n < payloads.len(),
            "expected the burst to coalesce, got {n} writes for {} packets",
            payloads.len()
        );
    }

    #[tokio::test]
    async fn test_tunnel_writer_loop_handles_burst_over_batch_limit() {
        // More packets than fit in one batch must still all arrive, in order.
        let (client, server) = tokio::io::duplex(1 << 20);
        let (_, client_writer) = tokio::io::split(client);
        let (mut server_reader, _) = tokio::io::split(server);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let count = WRITE_BATCH_LIMIT * 3 + 7;
        let (tx, rx) = mpsc::channel::<Vec<u8>>(count);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        for i in 0..count {
            tx.send(vec![(i % 251) as u8; 64]).await.unwrap();
        }

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_writer_loop(
                client_writer,
                rx,
                shutdown_clone,
                alive_clone,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        for i in 0..count {
            let frame = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(frame, vec![(i % 251) as u8; 64], "frame {i} corrupted");
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tunnel_writer_loop_shutdown() {
        let (client, _server) = tokio::io::duplex(65536);
        let (_, client_writer) = tokio::io::split(client);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (_tx, rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_writer_loop(
                client_writer,
                rx,
                shutdown_clone,
                alive_clone,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Yield to let the spawned task reach the select! loop
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Signal shutdown
        shutdown.notify_waiters();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_tunnel_writer_loop_channel_closed() {
        let (client, _server) = tokio::io::duplex(65536);
        let (_, client_writer) = tokio::io::split(client);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_writer_loop(
                client_writer,
                rx,
                shutdown_clone,
                alive_clone,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Drop sender to close channel
        drop(tx);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_ip_packet() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tunnel);

        let (tun_stream, mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);
        let (mut tun_reader, _) = tokio::io::split(mock_tun);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, _outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                0x12345678,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send an IP packet through the tunnel (protocol 0x0021 = IP)
        let ip_data = vec![0x45, 0x00, 0x00, 0x20, 0x00, 0x01]; // minimal IP header fragment
        let mut ppp_frame = Vec::new();
        ppp_frame.extend_from_slice(&IP_PROTOCOL.to_be_bytes());
        ppp_frame.extend_from_slice(&ip_data);
        write_frame(&mut mock_writer, &ppp_frame).await.unwrap();

        // Read from tun side - should have PI header (on macOS) + IP data
        let mut buf = vec![0u8; 256];
        let n = tokio::time::timeout(std::time::Duration::from_secs(2), tun_reader.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert!(n > 0);
        // The IP data should be in the output (possibly with PI header prefix)
        let output = &buf[..n];
        // On macOS, first 4 bytes are PI header [0, 0, 0, 2], rest is IP data
        #[cfg(target_os = "macos")]
        {
            assert_eq!(&output[..4], &[0, 0, 0, 2]);
            assert_eq!(&output[4..], &ip_data);
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(output, &ip_data[..]);
        }

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_lcp_echo_request() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tunnel);

        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let magic: u32 = 0xAABBCCDD;
        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                magic,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send LCP Echo-Request
        let echo_req = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_ECHO_REQUEST,
            identifier: 42,
            data: 0xDEADBEEFu32.to_be_bytes().to_vec(),
        };
        write_frame(&mut mock_writer, &echo_req.encode())
            .await
            .unwrap();

        // Should receive LCP Echo-Reply on outbound channel
        let reply_data =
            tokio::time::timeout(std::time::Duration::from_secs(2), outbound_rx.recv())
                .await
                .unwrap()
                .unwrap();

        let reply = PppPacket::decode(&reply_data).unwrap();
        assert_eq!(reply.protocol, LCP_PROTOCOL);
        assert_eq!(reply.code, LCP_ECHO_REPLY);
        assert_eq!(reply.identifier, 42);

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tunnel_writer_loop_write_error_sets_alive_false() {
        // Create a stream but drop the read end to cause a write error
        let (client, server) = tokio::io::duplex(16); // small buffer
        let (_, client_writer) = tokio::io::split(client);
        drop(server); // drop both ends of server

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let alive_clone = alive.clone();
        let shutdown_clone = shutdown.clone();
        let handle = tokio::spawn(async move {
            tunnel_writer_loop(
                client_writer,
                rx,
                shutdown_clone,
                alive_clone,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send data - write should fail because server end is dropped
        let _ = tx.send(b"data".to_vec()).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(!alive.load(Ordering::Relaxed));
        assert!(matches!(*event_rx.borrow(), crate::VpnEvent::Died(_)));
    }

    #[tokio::test]
    async fn test_tun_reader_loop_short_read_continues() {
        let (tun_stream, mock_tun) = tokio::io::duplex(65536);
        let (tun_reader, _) = tokio::io::split(tun_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tun);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(16);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tun_reader_loop(tun_reader, outbound_tx, shutdown_clone, alive_clone).await;
        });

        // Send a very short read (less than PI_HEADER_SIZE) - should continue
        use tokio::io::AsyncWriteExt;
        mock_writer.write_all(&[0x00]).await.unwrap();

        // Then send a proper sized packet
        let mut packet = vec![0u8; PI_HEADER_SIZE + 10];
        packet[PI_HEADER_SIZE..].fill(0x45);
        mock_writer.write_all(&packet).await.unwrap();

        // Should get the proper packet as a PPP frame
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), outbound_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(frame.len() > 2); // protocol + data

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_lcp_echo_reply_resets_counter() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tunnel);

        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, _outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                0x11111111,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send LCP Echo-Reply
        let echo_reply = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_ECHO_REPLY,
            identifier: 1,
            data: 0x22222222u32.to_be_bytes().to_vec(),
        };
        write_frame(&mut mock_writer, &echo_reply.encode())
            .await
            .unwrap();

        // Give it time to process
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Should still be alive (echo reply resets missed counter)
        assert!(alive.load(Ordering::Relaxed));

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_lcp_terminate() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tunnel);

        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                0x11111111,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send LCP Terminate-Request
        let term = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_TERMINATE_REQUEST,
            identifier: 99,
            data: vec![],
        };
        write_frame(&mut mock_writer, &term.encode()).await.unwrap();

        // Should receive Terminate-Ack
        let ack_data = tokio::time::timeout(std::time::Duration::from_secs(2), outbound_rx.recv())
            .await
            .unwrap()
            .unwrap();

        let ack = PppPacket::decode(&ack_data).unwrap();
        assert_eq!(ack.code, LCP_TERMINATE_ACK);
        assert_eq!(ack.identifier, 99);

        // Should set alive to false
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
        assert!(!alive.load(Ordering::Relaxed));
        assert!(matches!(*event_rx.borrow(), crate::VpnEvent::Died(_)));
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_short_frame_skipped() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tunnel);

        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, _outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                0x11111111,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send a short frame (< 4 bytes payload) - should be skipped
        write_frame(&mut mock_writer, &[0x00, 0x01]).await.unwrap();

        // Give it time to process
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Should still be alive (short frame was just skipped)
        assert!(alive.load(Ordering::Relaxed));

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_unknown_protocol_ignored() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tunnel);

        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, _outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, _event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                0x11111111,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Send unknown protocol (0xFFFF)
        let mut unknown_frame = Vec::new();
        unknown_frame.extend_from_slice(&0xFFFFu16.to_be_bytes());
        unknown_frame.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        write_frame(&mut mock_writer, &unknown_frame).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(alive.load(Ordering::Relaxed));

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tun_reader_loop_sends_ppp_frame() {
        let (tun_stream, mock_tun) = tokio::io::duplex(65536);
        let (tun_reader, _) = tokio::io::split(tun_stream);
        let (_, mut mock_writer) = tokio::io::split(mock_tun);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(16);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tun_reader_loop(tun_reader, outbound_tx, shutdown_clone, alive_clone).await;
        });

        // Write data to mock tun (simulate a packet from the tun device)
        // On macOS, needs PI header [0,0,0,2] + IP data
        // On other platforms, just IP data
        #[cfg(target_os = "macos")]
        {
            let mut pkt = vec![0, 0, 0, 2]; // PI header
            pkt.extend_from_slice(&[0x45, 0x00, 0x00, 0x20]); // IP header start
            mock_writer.write_all(&pkt).await.unwrap();
            mock_writer.flush().await.unwrap();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let pkt = vec![0x45, 0x00, 0x00, 0x20];
            mock_writer.write_all(&pkt).await.unwrap();
            mock_writer.flush().await.unwrap();
        }

        // Should receive PPP-wrapped IP packet on outbound channel
        let ppp_frame = tokio::time::timeout(std::time::Duration::from_secs(2), outbound_rx.recv())
            .await
            .unwrap()
            .unwrap();

        // First 2 bytes should be IP protocol
        assert_eq!(&ppp_frame[..2], &IP_PROTOCOL.to_be_bytes());
        // Rest should be the IP data (without PI header)
        assert_eq!(&ppp_frame[2..], &[0x45, 0x00, 0x00, 0x20]);

        shutdown.notify_waiters();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn test_tun_reader_loop_shutdown() {
        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (tun_reader, _) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, _outbound_rx) = mpsc::channel::<Vec<u8>>(16);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tun_reader_loop(tun_reader, outbound_tx, shutdown_clone, alive_clone).await;
        });

        // Yield to let the spawned task reach the select! loop
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        shutdown.notify_waiters();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_tunnel_reader_loop_connection_lost() {
        let (tunnel_stream, mock_tunnel) = tokio::io::duplex(65536);
        let (tunnel_reader, _) = tokio::io::split(tunnel_stream);

        let (tun_stream, _mock_tun) = tokio::io::duplex(65536);
        let (_, tun_writer) = tokio::io::split(tun_stream);

        let shutdown = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let (outbound_tx, _outbound_rx) = mpsc::channel::<Vec<u8>>(16);
        let (event_tx, event_rx) = tokio::sync::watch::channel(crate::VpnEvent::Alive);
        let event_tx = Arc::new(event_tx);

        let shutdown_clone = shutdown.clone();
        let alive_clone = alive.clone();
        let handle = tokio::spawn(async move {
            tunnel_reader_loop(
                tunnel_reader,
                tun_writer,
                outbound_tx,
                shutdown_clone,
                alive_clone,
                0x11111111,
                event_tx,
                Arc::new(BridgeStats::default()),
            )
            .await;
        });

        // Drop the mock tunnel to simulate connection loss
        drop(mock_tunnel);

        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(!alive.load(Ordering::Relaxed));
        assert!(matches!(*event_rx.borrow(), crate::VpnEvent::Died(_)));
    }
}
