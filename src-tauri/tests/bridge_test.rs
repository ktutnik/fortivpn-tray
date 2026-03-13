use std::net::Ipv4Addr;
use fortivpn::bridge::negotiate_ppp;
use fortivpn::ppp::*;
use fortivpn::tunnel::{read_frame, write_frame};

/// Simulate a successful PPP negotiation (LCP + IPCP)
#[tokio::test]
async fn test_negotiate_ppp_success() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        // Read client's LCP Configure-Request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        assert_eq!(pkt.protocol, LCP_PROTOCOL);
        assert_eq!(pkt.code, LCP_CONFIGURE_REQUEST);

        // Send LCP Configure-Ack
        let ack = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Send server's LCP Configure-Request
        let mut server_lcp_data = Vec::new();
        server_lcp_data.extend(LcpOption::Mru(1354).encode());
        server_lcp_data.extend(LcpOption::MagicNumber(0xAABBCCDD).encode());
        let server_lcp = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 100,
            data: server_lcp_data,
        };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();

        // Read client's LCP Configure-Ack (for server's request)
        let frame = read_frame(&mut server_reader).await.unwrap();
        let ack_pkt = PppPacket::decode(&frame).unwrap();
        assert_eq!(ack_pkt.code, LCP_CONFIGURE_ACK);
        assert_eq!(ack_pkt.identifier, 100);

        // Read client's initial IPCP Configure-Request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        assert_eq!(pkt.protocol, IPCP_PROTOCOL);
        assert_eq!(pkt.code, LCP_CONFIGURE_REQUEST);

        // Send IPCP Configure-Nak with assigned values
        let mut nak_data = Vec::new();
        nak_data.extend(IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 100)).encode());
        nak_data.extend(IpcpOption::PrimaryDns(Ipv4Addr::new(8, 8, 8, 8)).encode());
        nak_data.extend(IpcpOption::SecondaryDns(Ipv4Addr::new(8, 8, 4, 4)).encode());
        let nak = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_NAK,
            identifier: pkt.identifier,
            data: nak_data,
        };
        write_frame(&mut server_writer, &nak.encode()).await.unwrap();

        // Send server's IPCP Configure-Request
        let server_ipcp = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 101,
            data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode(),
        };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();

        // Read client's resent IPCP Configure-Request (with updated values)
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        assert_eq!(pkt.protocol, IPCP_PROTOCOL);

        // Send IPCP Configure-Ack for client's updated request
        let ack = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Read client's IPCP Configure-Ack for server's request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let ack_pkt = PppPacket::decode(&frame).unwrap();
        assert_eq!(ack_pkt.protocol, IPCP_PROTOCOL);
        assert_eq!(ack_pkt.code, LCP_CONFIGURE_ACK);
        assert_eq!(ack_pkt.identifier, 101);
    });

    let (ip, _magic, dns) = negotiate_ppp(&mut client_reader, &mut client_writer).await.unwrap();
    server.await.unwrap();

    assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 100));
    assert_eq!(dns.len(), 2);
    assert_eq!(dns[0], Ipv4Addr::new(8, 8, 8, 8));
    assert_eq!(dns[1], Ipv4Addr::new(8, 8, 4, 4));
}

/// Test LCP echo handling during negotiation
#[tokio::test]
async fn test_negotiate_ppp_with_echo_request() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        // Read client's LCP Configure-Request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send LCP Configure-Ack
        let ack = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Send an LCP Echo-Request (should be handled during negotiation)
        let echo = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_ECHO_REQUEST,
            identifier: 50,
            data: 0xDEADBEEFu32.to_be_bytes().to_vec(),
        };
        write_frame(&mut server_writer, &echo.encode()).await.unwrap();

        // Read client's echo reply
        let frame = read_frame(&mut server_reader).await.unwrap();
        let reply = PppPacket::decode(&frame).unwrap();
        assert_eq!(reply.code, LCP_ECHO_REPLY);
        assert_eq!(reply.identifier, 50);

        // Now send server's LCP Configure-Request
        let server_lcp = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 100,
            data: LcpOption::Mru(1354).encode(),
        };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();

        // Read LCP Ack
        let _ = read_frame(&mut server_reader).await.unwrap();

        // IPCP flow (same as above, simplified)
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // ACK the IPCP directly (no NAK this time)
        let ack = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Send server IPCP request
        let server_ipcp = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 101,
            data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode(),
        };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();

        let _ = read_frame(&mut server_reader).await.unwrap();
    });

    let result = negotiate_ppp(&mut client_reader, &mut client_writer).await;
    server.await.unwrap();
    assert!(result.is_ok());
}

/// Test LCP Configure-Reject handling
#[tokio::test]
async fn test_negotiate_ppp_with_lcp_reject() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        // Read client's LCP Configure-Request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send LCP Configure-Reject (reject some option)
        let reject = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REJECT,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &reject.encode()).await.unwrap();

        // Client will resend — read it
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Now ACK
        let ack = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Server LCP request
        let server_lcp = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 100,
            data: LcpOption::Mru(1354).encode(),
        };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();

        let _ = read_frame(&mut server_reader).await.unwrap();

        // IPCP
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        let ack = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();
        let server_ipcp = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 101,
            data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode(),
        };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();
    });

    let result = negotiate_ppp(&mut client_reader, &mut client_writer).await;
    server.await.unwrap();
    assert!(result.is_ok());
}

/// Test LCP Configure-Nak handling (server suggests different MRU)
#[tokio::test]
async fn test_negotiate_ppp_with_lcp_nak() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        // Read client's LCP Configure-Request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send LCP Configure-Nak suggesting different MRU
        let nak = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_NAK,
            identifier: pkt.identifier,
            data: LcpOption::Mru(1400).encode(),
        };
        write_frame(&mut server_writer, &nak.encode()).await.unwrap();

        // Read resent LCP request
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // ACK it
        let ack = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Server LCP
        let server_lcp = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 100,
            data: LcpOption::Mru(1354).encode(),
        };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();

        // IPCP
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        let ack = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();
        let server_ipcp = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 101,
            data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode(),
        };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();
    });

    let result = negotiate_ppp(&mut client_reader, &mut client_writer).await;
    server.await.unwrap();
    assert!(result.is_ok());
}

/// Test that unknown protocols during negotiation are ignored
#[tokio::test]
async fn test_negotiate_ppp_ignores_unknown_protocol() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send unknown protocol (ignored)
        let unknown = PppPacket { protocol: 0x9999, code: 1, identifier: 1, data: vec![0x01] };
        write_frame(&mut server_writer, &unknown.encode()).await.unwrap();

        // Send unknown LCP code (ignored)
        let unknown_lcp = PppPacket { protocol: LCP_PROTOCOL, code: 99, identifier: 1, data: vec![] };
        write_frame(&mut server_writer, &unknown_lcp.encode()).await.unwrap();

        // Normal flow
        let ack = PppPacket { protocol: LCP_PROTOCOL, code: LCP_CONFIGURE_ACK, identifier: pkt.identifier, data: pkt.data.clone() };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        let server_lcp = PppPacket { protocol: LCP_PROTOCOL, code: LCP_CONFIGURE_REQUEST, identifier: 100, data: LcpOption::Mru(1354).encode() };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();

        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send unknown IPCP code before acking
        let unknown_ipcp = PppPacket { protocol: IPCP_PROTOCOL, code: 99, identifier: 1, data: vec![] };
        write_frame(&mut server_writer, &unknown_ipcp.encode()).await.unwrap();

        let ack = PppPacket { protocol: IPCP_PROTOCOL, code: LCP_CONFIGURE_ACK, identifier: pkt.identifier, data: pkt.data.clone() };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        let server_ipcp = PppPacket { protocol: IPCP_PROTOCOL, code: LCP_CONFIGURE_REQUEST, identifier: 101, data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode() };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();
    });

    let result = negotiate_ppp(&mut client_reader, &mut client_writer).await;
    server.await.unwrap();
    assert!(result.is_ok());
}

/// Test negotiate_ppp with bad PPP frame (decode error → skip)
#[tokio::test]
async fn test_negotiate_ppp_skips_bad_ppp_frame() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send a frame with valid tunnel framing but invalid PPP structure
        write_frame(&mut server_writer, &[0xFF, 0x03, 0x00, 0xC0, 0x21, 0x01]).await.unwrap();

        // Normal flow
        let ack = PppPacket { protocol: LCP_PROTOCOL, code: LCP_CONFIGURE_ACK, identifier: pkt.identifier, data: pkt.data.clone() };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        let server_lcp = PppPacket { protocol: LCP_PROTOCOL, code: LCP_CONFIGURE_REQUEST, identifier: 100, data: LcpOption::Mru(1354).encode() };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();

        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        let ack = PppPacket { protocol: IPCP_PROTOCOL, code: LCP_CONFIGURE_ACK, identifier: pkt.identifier, data: pkt.data.clone() };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();
        let server_ipcp = PppPacket { protocol: IPCP_PROTOCOL, code: LCP_CONFIGURE_REQUEST, identifier: 101, data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode() };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();
    });

    let result = negotiate_ppp(&mut client_reader, &mut client_writer).await;
    server.await.unwrap();
    assert!(result.is_ok());
}

/// Test that short frames are skipped during negotiation
#[tokio::test]
async fn test_negotiate_ppp_skips_short_frames() {
    let (client_stream, server_stream) = tokio::io::duplex(65536);
    let (mut client_reader, mut client_writer) = tokio::io::split(client_stream);
    let (mut server_reader, mut server_writer) = tokio::io::split(server_stream);

    let server = tokio::spawn(async move {
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();

        // Send a short frame (should be skipped)
        write_frame(&mut server_writer, &[0x00, 0x01]).await.unwrap();

        // Send proper LCP Ack
        let ack = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();

        // Server LCP
        let server_lcp = PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 100,
            data: LcpOption::Mru(1354).encode(),
        };
        write_frame(&mut server_writer, &server_lcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();

        // IPCP
        let frame = read_frame(&mut server_reader).await.unwrap();
        let pkt = PppPacket::decode(&frame).unwrap();
        let ack = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier: pkt.identifier,
            data: pkt.data.clone(),
        };
        write_frame(&mut server_writer, &ack.encode()).await.unwrap();
        let server_ipcp = PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier: 101,
            data: IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode(),
        };
        write_frame(&mut server_writer, &server_ipcp.encode()).await.unwrap();
        let _ = read_frame(&mut server_reader).await.unwrap();
    });

    let result = negotiate_ppp(&mut client_reader, &mut client_writer).await;
    server.await.unwrap();
    assert!(result.is_ok());
}
