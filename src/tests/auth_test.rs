use fortivpn::auth::{extract_cookie, parse_vpn_config_xml};
use std::net::Ipv4Addr;

#[test]
fn test_extract_cookie_from_headers() {
    let response =
        "HTTP/1.1 200 OK\r\nSet-Cookie: SVPNCOOKIE=abc123def; path=/\r\nContent-Length: 0\r\n\r\n";
    let cookie = extract_cookie(response).unwrap();
    assert_eq!(cookie, "abc123def");
}

#[test]
fn test_extract_cookie_missing() {
    let response = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
    assert!(extract_cookie(response).is_none());
}

#[test]
fn test_parse_vpn_config_xml() {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<sslvpn-tunnel ver="2" dtls="0">
  <assigned-addr ipv4="10.212.134.200"/>
  <dns>
    <ip>10.0.0.1</ip>
    <ip>10.0.0.2</ip>
    <domain>mims.local</domain>
  </dns>
  <split-tunnel-info>
    <addr ip="10.0.0.0" mask="255.0.0.0"/>
    <addr ip="172.16.0.0" mask="255.240.0.0"/>
  </split-tunnel-info>
</sslvpn-tunnel>"#;
    let config = parse_vpn_config_xml(xml).unwrap();
    assert_eq!(config.assigned_ip, Ipv4Addr::new(10, 212, 134, 200));
    assert_eq!(config.dns_servers.len(), 2);
    assert_eq!(config.dns_servers[0], Ipv4Addr::new(10, 0, 0, 1));
    assert_eq!(config.search_domain, Some("mims.local".to_string()));
    assert_eq!(config.routes.len(), 2);
    assert_eq!(
        config.routes[0],
        (Ipv4Addr::new(10, 0, 0, 0), Ipv4Addr::new(255, 0, 0, 0))
    );
}

#[test]
fn test_parse_vpn_config_xml_minimal() {
    let xml = r#"<?xml version="1.0"?>
<sslvpn-tunnel ver="2">
  <assigned-addr ipv4="10.1.1.1"/>
</sslvpn-tunnel>"#;
    let config = parse_vpn_config_xml(xml).unwrap();
    assert_eq!(config.assigned_ip, Ipv4Addr::new(10, 1, 1, 1));
    assert!(config.dns_servers.is_empty());
    assert!(config.routes.is_empty());
}

#[test]
fn test_extract_cookie_with_ret_value() {
    let response = "HTTP/1.1 200 OK\r\nSet-Cookie: SVPNCOOKIE=longcookievalue123; path=/; HttpOnly\r\n\r\nret=1,redir=/remote/index";
    let cookie = extract_cookie(response).unwrap();
    assert_eq!(cookie, "longcookievalue123");
}
