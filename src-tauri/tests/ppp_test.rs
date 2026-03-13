use std::net::Ipv4Addr;
use fortivpn::ppp::*;

// === PppPacket tests ===

#[test]
fn test_ppp_packet_encode() {
    let pkt = PppPacket {
        protocol: LCP_PROTOCOL,
        code: LCP_CONFIGURE_REQUEST,
        identifier: 1,
        data: vec![],
    };
    let bytes = pkt.encode();
    assert_eq!(&bytes[0..2], &LCP_PROTOCOL.to_be_bytes());
    assert_eq!(bytes[2], LCP_CONFIGURE_REQUEST);
    assert_eq!(bytes[3], 1);
    assert_eq!(&bytes[4..6], &4u16.to_be_bytes());
}

#[test]
fn test_ppp_packet_encode_with_data() {
    let pkt = PppPacket {
        protocol: IPCP_PROTOCOL,
        code: LCP_CONFIGURE_NAK,
        identifier: 42,
        data: vec![1, 2, 3, 4],
    };
    let bytes = pkt.encode();
    assert_eq!(&bytes[0..2], &IPCP_PROTOCOL.to_be_bytes());
    assert_eq!(bytes[2], LCP_CONFIGURE_NAK);
    assert_eq!(bytes[3], 42);
    assert_eq!(&bytes[4..6], &8u16.to_be_bytes()); // 4 + 4 data bytes
    assert_eq!(&bytes[6..], &[1, 2, 3, 4]);
}

#[test]
fn test_ppp_packet_decode() {
    let bytes = [
        0xC0, 0x21, 0x01, 0x05, 0x00, 0x08,
        0x01, 0x04, 0x05, 0x4A,
    ];
    let pkt = PppPacket::decode(&bytes).unwrap();
    assert_eq!(pkt.protocol, LCP_PROTOCOL);
    assert_eq!(pkt.code, LCP_CONFIGURE_REQUEST);
    assert_eq!(pkt.identifier, 5);
    assert_eq!(pkt.data, &[0x01, 0x04, 0x05, 0x4A]);
}

#[test]
fn test_ppp_packet_decode_too_short() {
    let bytes = [0xC0, 0x21, 0x01];
    assert!(PppPacket::decode(&bytes).is_err());
}

#[test]
fn test_ppp_packet_decode_truncated_data() {
    // Header says length=10 (6 data bytes) but only 2 data bytes present
    let bytes = [0xC0, 0x21, 0x01, 0x01, 0x00, 0x0A, 0x01, 0x02];
    assert!(PppPacket::decode(&bytes).is_err());
}

#[test]
fn test_ppp_packet_decode_no_data() {
    let bytes = [0xC0, 0x21, 0x05, 0x0A, 0x00, 0x04];
    let pkt = PppPacket::decode(&bytes).unwrap();
    assert_eq!(pkt.code, LCP_TERMINATE_REQUEST);
    assert_eq!(pkt.identifier, 10);
    assert!(pkt.data.is_empty());
}

#[test]
fn test_ppp_packet_encode_decode_roundtrip() {
    let original = PppPacket {
        protocol: LCP_PROTOCOL,
        code: LCP_ECHO_REQUEST,
        identifier: 99,
        data: vec![0xDE, 0xAD, 0xBE, 0xEF],
    };
    let encoded = original.encode();
    let decoded = PppPacket::decode(&encoded).unwrap();
    assert_eq!(decoded.protocol, original.protocol);
    assert_eq!(decoded.code, original.code);
    assert_eq!(decoded.identifier, original.identifier);
    assert_eq!(decoded.data, original.data);
}

// === LcpOption tests ===

#[test]
fn test_lcp_option_encode_mru() {
    let opt = LcpOption::Mru(1354);
    let bytes = opt.encode();
    assert_eq!(bytes, vec![0x01, 0x04, 0x05, 0x4A]);
}

#[test]
fn test_lcp_option_encode_magic() {
    let opt = LcpOption::MagicNumber(0xDEADBEEF);
    let bytes = opt.encode();
    assert_eq!(bytes, vec![0x05, 0x06, 0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn test_lcp_option_encode_accm() {
    let opt = LcpOption::Accm(0x000A0000);
    let bytes = opt.encode();
    assert_eq!(bytes, vec![0x02, 0x06, 0x00, 0x0A, 0x00, 0x00]);
}

#[test]
fn test_lcp_option_encode_unknown() {
    let opt = LcpOption::Unknown(99, vec![0x01, 0x02]);
    let bytes = opt.encode();
    assert_eq!(bytes, vec![99, 4, 0x01, 0x02]);
}

#[test]
fn test_lcp_option_decode_mru() {
    let bytes = [0x01, 0x04, 0x05, 0x4A];
    let (opt, consumed) = LcpOption::decode(&bytes).unwrap();
    assert!(matches!(opt, LcpOption::Mru(1354)));
    assert_eq!(consumed, 4);
}

#[test]
fn test_lcp_option_decode_magic_number() {
    let bytes = [0x05, 0x06, 0xDE, 0xAD, 0xBE, 0xEF];
    let (opt, consumed) = LcpOption::decode(&bytes).unwrap();
    match opt {
        LcpOption::MagicNumber(m) => assert_eq!(m, 0xDEADBEEF),
        _ => panic!("Expected MagicNumber"),
    }
    assert_eq!(consumed, 6);
}

#[test]
fn test_lcp_option_decode_accm() {
    let bytes = [0x02, 0x06, 0x00, 0x0A, 0x00, 0x00];
    let (opt, consumed) = LcpOption::decode(&bytes).unwrap();
    match opt {
        LcpOption::Accm(v) => assert_eq!(v, 0x000A0000),
        _ => panic!("Expected Accm"),
    }
    assert_eq!(consumed, 6);
}

#[test]
fn test_lcp_option_decode_unknown() {
    let bytes = [0xFF, 0x04, 0xAA, 0xBB];
    let (opt, consumed) = LcpOption::decode(&bytes).unwrap();
    match opt {
        LcpOption::Unknown(typ, data) => {
            assert_eq!(typ, 0xFF);
            assert_eq!(data, vec![0xAA, 0xBB]);
        }
        _ => panic!("Expected Unknown"),
    }
    assert_eq!(consumed, 4);
}

#[test]
fn test_lcp_option_decode_too_short() {
    let bytes = [0x01];
    assert!(LcpOption::decode(&bytes).is_err());
}

#[test]
fn test_lcp_option_decode_invalid_length() {
    // Type=1 (MRU), Length=20 but only 4 bytes available
    let bytes = [0x01, 0x14, 0x05, 0x4A];
    assert!(LcpOption::decode(&bytes).is_err());
}

#[test]
fn test_lcp_option_decode_length_less_than_2() {
    let bytes = [0x01, 0x01, 0x05, 0x4A];
    assert!(LcpOption::decode(&bytes).is_err());
}

#[test]
fn test_lcp_option_encode_decode_roundtrip_mru() {
    let original = LcpOption::Mru(1500);
    let encoded = original.encode();
    let (decoded, _) = LcpOption::decode(&encoded).unwrap();
    match decoded {
        LcpOption::Mru(v) => assert_eq!(v, 1500),
        _ => panic!("Expected Mru"),
    }
}

#[test]
fn test_lcp_option_encode_decode_roundtrip_magic() {
    let original = LcpOption::MagicNumber(0x12345678);
    let encoded = original.encode();
    let (decoded, _) = LcpOption::decode(&encoded).unwrap();
    match decoded {
        LcpOption::MagicNumber(v) => assert_eq!(v, 0x12345678),
        _ => panic!("Expected MagicNumber"),
    }
}

// === IpcpOption tests ===

#[test]
fn test_ipcp_option_encode_ip_address() {
    let opt = IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1));
    let bytes = opt.encode();
    assert_eq!(bytes, vec![3, 6, 10, 0, 0, 1]);
}

#[test]
fn test_ipcp_option_encode_primary_dns() {
    let opt = IpcpOption::PrimaryDns(Ipv4Addr::new(8, 8, 8, 8));
    let bytes = opt.encode();
    assert_eq!(bytes, vec![129, 6, 8, 8, 8, 8]);
}

#[test]
fn test_ipcp_option_encode_secondary_dns() {
    let opt = IpcpOption::SecondaryDns(Ipv4Addr::new(8, 8, 4, 4));
    let bytes = opt.encode();
    assert_eq!(bytes, vec![131, 6, 8, 8, 4, 4]);
}

#[test]
fn test_ipcp_option_encode_unknown() {
    let opt = IpcpOption::Unknown(200, vec![0x01]);
    let bytes = opt.encode();
    assert_eq!(bytes, vec![200, 3, 0x01]);
}

#[test]
fn test_ipcp_option_decode_ip_address() {
    let bytes = [3, 6, 192, 168, 1, 100];
    let (opt, consumed) = IpcpOption::decode(&bytes).unwrap();
    match opt {
        IpcpOption::IpAddress(ip) => assert_eq!(ip, Ipv4Addr::new(192, 168, 1, 100)),
        _ => panic!("Expected IpAddress"),
    }
    assert_eq!(consumed, 6);
}

#[test]
fn test_ipcp_option_decode_primary_dns() {
    let bytes = [129, 6, 10, 0, 0, 1];
    let (opt, consumed) = IpcpOption::decode(&bytes).unwrap();
    match opt {
        IpcpOption::PrimaryDns(ip) => assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 1)),
        _ => panic!("Expected PrimaryDns"),
    }
    assert_eq!(consumed, 6);
}

#[test]
fn test_ipcp_option_decode_secondary_dns() {
    let bytes = [131, 6, 1, 1, 1, 1];
    let (opt, consumed) = IpcpOption::decode(&bytes).unwrap();
    match opt {
        IpcpOption::SecondaryDns(ip) => assert_eq!(ip, Ipv4Addr::new(1, 1, 1, 1)),
        _ => panic!("Expected SecondaryDns"),
    }
    assert_eq!(consumed, 6);
}

#[test]
fn test_ipcp_option_decode_unknown() {
    let bytes = [200, 4, 0xAA, 0xBB];
    let (opt, consumed) = IpcpOption::decode(&bytes).unwrap();
    match opt {
        IpcpOption::Unknown(typ, data) => {
            assert_eq!(typ, 200);
            assert_eq!(data, vec![0xAA, 0xBB]);
        }
        _ => panic!("Expected Unknown"),
    }
    assert_eq!(consumed, 4);
}

#[test]
fn test_ipcp_option_decode_too_short() {
    let bytes = [3];
    assert!(IpcpOption::decode(&bytes).is_err());
}

#[test]
fn test_ipcp_option_decode_invalid_length() {
    let bytes = [3, 0x14, 10, 0]; // length=20 but only 4 bytes
    assert!(IpcpOption::decode(&bytes).is_err());
}

#[test]
fn test_ipcp_option_decode_length_less_than_2() {
    let bytes = [3, 1, 10, 0, 0, 1];
    assert!(IpcpOption::decode(&bytes).is_err());
}

#[test]
fn test_ipcp_option_encode_decode_roundtrip() {
    let original = IpcpOption::IpAddress(Ipv4Addr::new(172, 16, 0, 1));
    let encoded = original.encode();
    let (decoded, _) = IpcpOption::decode(&encoded).unwrap();
    match decoded {
        IpcpOption::IpAddress(ip) => assert_eq!(ip, Ipv4Addr::new(172, 16, 0, 1)),
        _ => panic!("Expected IpAddress"),
    }
}

// === LcpState tests ===

#[test]
fn test_lcp_state_new() {
    let lcp = LcpState::new();
    assert_eq!(lcp.mru, 1354);
    assert!(!lcp.opened);
    assert_eq!(lcp.peer_magic, 0);
    assert_ne!(lcp.magic_number, 0); // random, extremely unlikely to be 0
}

#[test]
fn test_lcp_build_configure_request() {
    let lcp = LcpState::new();
    let pkt = lcp.build_configure_request(1);
    assert_eq!(pkt.protocol, LCP_PROTOCOL);
    assert_eq!(pkt.code, LCP_CONFIGURE_REQUEST);
    assert_eq!(pkt.identifier, 1);
    assert!(!pkt.data.is_empty());
}

#[test]
fn test_lcp_build_configure_request_contains_mru_and_magic() {
    let lcp = LcpState::new();
    let pkt = lcp.build_configure_request(1);
    // Should contain MRU option (type=1) and MagicNumber option (type=5)
    assert!(pkt.data.len() >= 10); // 4 bytes MRU + 6 bytes Magic
    assert_eq!(pkt.data[0], 0x01); // MRU type
    assert_eq!(pkt.data[4], 0x05); // MagicNumber type
}

#[test]
fn test_lcp_handle_configure_request_from_peer() {
    let mut lcp = LcpState::new();
    let peer_data = vec![0x01, 0x04, 0x05, 0x4A];
    let response = lcp.handle_configure_request(5, &peer_data);
    assert_eq!(response.code, LCP_CONFIGURE_ACK);
    assert_eq!(response.identifier, 5);
    assert_eq!(response.data, peer_data);
}

#[test]
fn test_lcp_handle_configure_request_extracts_peer_magic() {
    let mut lcp = LcpState::new();
    let mut peer_data = Vec::new();
    peer_data.extend(LcpOption::Mru(1400).encode());
    peer_data.extend(LcpOption::MagicNumber(0xAABBCCDD).encode());
    let _ = lcp.handle_configure_request(1, &peer_data);
    assert_eq!(lcp.peer_magic, 0xAABBCCDD);
}

#[test]
fn test_lcp_handle_configure_ack() {
    let mut lcp = LcpState::new();
    assert!(!lcp.opened);
    lcp.handle_configure_ack();
    assert!(lcp.opened);
}

#[test]
fn test_lcp_handle_configure_nak_updates_mru() {
    let mut lcp = LcpState::new();
    assert_eq!(lcp.mru, 1354);
    let nak_data = LcpOption::Mru(1400).encode();
    lcp.handle_configure_nak(&nak_data);
    assert_eq!(lcp.mru, 1400);
}

#[test]
fn test_lcp_handle_configure_nak_ignores_non_mru() {
    let mut lcp = LcpState::new();
    let original_mru = lcp.mru;
    let nak_data = LcpOption::MagicNumber(0x11111111).encode();
    lcp.handle_configure_nak(&nak_data);
    assert_eq!(lcp.mru, original_mru);
}

#[test]
fn test_lcp_handle_configure_nak_multiple_options() {
    let mut lcp = LcpState::new();
    let mut nak_data = Vec::new();
    nak_data.extend(LcpOption::Mru(1500).encode());
    nak_data.extend(LcpOption::MagicNumber(0x22222222).encode());
    lcp.handle_configure_nak(&nak_data);
    assert_eq!(lcp.mru, 1500);
}

#[test]
fn test_lcp_build_echo_request() {
    let lcp = LcpState::new();
    let pkt = lcp.build_echo_request(42);
    assert_eq!(pkt.protocol, LCP_PROTOCOL);
    assert_eq!(pkt.code, LCP_ECHO_REQUEST);
    assert_eq!(pkt.identifier, 42);
    assert_eq!(pkt.data.len(), 4);
    // Data should be magic number in big-endian
    let magic = u32::from_be_bytes([pkt.data[0], pkt.data[1], pkt.data[2], pkt.data[3]]);
    assert_eq!(magic, lcp.magic_number);
}

#[test]
fn test_lcp_build_echo_reply() {
    let lcp = LcpState::new();
    let pkt = lcp.build_echo_reply(99);
    assert_eq!(pkt.protocol, LCP_PROTOCOL);
    assert_eq!(pkt.code, LCP_ECHO_REPLY);
    assert_eq!(pkt.identifier, 99);
    assert_eq!(pkt.data.len(), 4);
    let magic = u32::from_be_bytes([pkt.data[0], pkt.data[1], pkt.data[2], pkt.data[3]]);
    assert_eq!(magic, lcp.magic_number);
}

#[test]
fn test_lcp_build_terminate_request() {
    let lcp = LcpState::new();
    let pkt = lcp.build_terminate_request(55);
    assert_eq!(pkt.protocol, LCP_PROTOCOL);
    assert_eq!(pkt.code, LCP_TERMINATE_REQUEST);
    assert_eq!(pkt.identifier, 55);
    assert!(pkt.data.is_empty());
}

// === IpcpState tests ===

#[test]
fn test_ipcp_state_new() {
    let ipcp = IpcpState::new();
    assert_eq!(ipcp.local_ip, Ipv4Addr::UNSPECIFIED);
    assert_eq!(ipcp.primary_dns, Ipv4Addr::UNSPECIFIED);
    assert_eq!(ipcp.secondary_dns, Ipv4Addr::UNSPECIFIED);
    assert!(!ipcp.opened);
}

#[test]
fn test_ipcp_build_configure_request() {
    let ipcp = IpcpState::new();
    let pkt = ipcp.build_configure_request(1);
    assert_eq!(pkt.protocol, IPCP_PROTOCOL);
    assert_eq!(pkt.code, LCP_CONFIGURE_REQUEST);
    assert_eq!(pkt.identifier, 1);
    // Should contain IP, Primary DNS, Secondary DNS options (3 * 6 bytes = 18)
    assert_eq!(pkt.data.len(), 18);
}

#[test]
fn test_ipcp_build_configure_request_with_assigned_values() {
    let mut ipcp = IpcpState::new();
    ipcp.local_ip = Ipv4Addr::new(10, 0, 0, 100);
    ipcp.primary_dns = Ipv4Addr::new(8, 8, 8, 8);
    ipcp.secondary_dns = Ipv4Addr::new(8, 8, 4, 4);
    let pkt = ipcp.build_configure_request(5);
    assert_eq!(pkt.identifier, 5);
    // Verify IP address is in the data
    assert_eq!(&pkt.data[2..6], &[10, 0, 0, 100]);
}

#[test]
fn test_ipcp_handle_configure_request() {
    let ipcp = IpcpState::new();
    let peer_data = IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 1)).encode();
    let response = ipcp.handle_configure_request(10, &peer_data);
    assert_eq!(response.protocol, IPCP_PROTOCOL);
    assert_eq!(response.code, LCP_CONFIGURE_ACK);
    assert_eq!(response.identifier, 10);
    assert_eq!(response.data, peer_data);
}

#[test]
fn test_ipcp_handle_configure_ack() {
    let mut ipcp = IpcpState::new();
    assert!(!ipcp.opened);
    ipcp.handle_configure_ack();
    assert!(ipcp.opened);
}

#[test]
fn test_ipcp_handle_configure_nak_updates_ip() {
    let mut ipcp = IpcpState::new();
    assert_eq!(ipcp.local_ip, Ipv4Addr::UNSPECIFIED);
    let nak_data = IpcpOption::IpAddress(Ipv4Addr::new(10, 0, 0, 100)).encode();
    ipcp.handle_configure_nak(&nak_data);
    assert_eq!(ipcp.local_ip, Ipv4Addr::new(10, 0, 0, 100));
}

#[test]
fn test_ipcp_handle_configure_nak_updates_dns() {
    let mut ipcp = IpcpState::new();
    let mut nak_data = Vec::new();
    nak_data.extend(IpcpOption::PrimaryDns(Ipv4Addr::new(8, 8, 8, 8)).encode());
    nak_data.extend(IpcpOption::SecondaryDns(Ipv4Addr::new(8, 8, 4, 4)).encode());
    ipcp.handle_configure_nak(&nak_data);
    assert_eq!(ipcp.primary_dns, Ipv4Addr::new(8, 8, 8, 8));
    assert_eq!(ipcp.secondary_dns, Ipv4Addr::new(8, 8, 4, 4));
}

#[test]
fn test_ipcp_handle_configure_nak_all_values() {
    let mut ipcp = IpcpState::new();
    let mut nak_data = Vec::new();
    nak_data.extend(IpcpOption::IpAddress(Ipv4Addr::new(172, 16, 0, 50)).encode());
    nak_data.extend(IpcpOption::PrimaryDns(Ipv4Addr::new(10, 0, 0, 1)).encode());
    nak_data.extend(IpcpOption::SecondaryDns(Ipv4Addr::new(10, 0, 0, 2)).encode());
    ipcp.handle_configure_nak(&nak_data);
    assert_eq!(ipcp.local_ip, Ipv4Addr::new(172, 16, 0, 50));
    assert_eq!(ipcp.primary_dns, Ipv4Addr::new(10, 0, 0, 1));
    assert_eq!(ipcp.secondary_dns, Ipv4Addr::new(10, 0, 0, 2));
}

#[test]
fn test_ipcp_handle_configure_nak_ignores_unknown() {
    let mut ipcp = IpcpState::new();
    let nak_data = IpcpOption::Unknown(200, vec![0x01, 0x02, 0x03, 0x04]).encode();
    ipcp.handle_configure_nak(&nak_data);
    assert_eq!(ipcp.local_ip, Ipv4Addr::UNSPECIFIED);
}

// === Protocol constants ===

#[test]
fn test_protocol_constants() {
    assert_eq!(LCP_PROTOCOL, 0xC021);
    assert_eq!(IPCP_PROTOCOL, 0x8021);
    assert_eq!(IP_PROTOCOL, 0x0021);
}

#[test]
fn test_code_constants() {
    assert_eq!(LCP_CONFIGURE_REQUEST, 1);
    assert_eq!(LCP_CONFIGURE_ACK, 2);
    assert_eq!(LCP_CONFIGURE_NAK, 3);
    assert_eq!(LCP_CONFIGURE_REJECT, 4);
    assert_eq!(LCP_TERMINATE_REQUEST, 5);
    assert_eq!(LCP_TERMINATE_ACK, 6);
    assert_eq!(LCP_ECHO_REQUEST, 9);
    assert_eq!(LCP_ECHO_REPLY, 10);
}

#[test]
fn test_ipcp_option_constants() {
    assert_eq!(IPCP_OPT_IP_ADDRESS, 3);
    assert_eq!(IPCP_OPT_PRIMARY_DNS, 129);
    assert_eq!(IPCP_OPT_SECONDARY_DNS, 131);
}
